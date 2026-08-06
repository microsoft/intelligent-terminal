// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "ProviderRegistry.h"

#include "IntelligentTerminalPaths.h"

#include <windows.h>
#include <bcrypt.h>
#include <json/json.h>

#include <algorithm>
#include <array>
#include <fstream>
#include <limits>
#include <set>
#include <sstream>
#include <thread>

#include <wil/resource.h>

#pragma comment(lib, "bcrypt.lib")

namespace Microsoft::Terminal::RichTab::Provider
{
    namespace
    {
        constexpr std::wstring_view ManifestFilename{ L"provider.json" };

        struct PayloadTree
        {
            std::vector<std::filesystem::path> files;
            uint64_t totalSize{ 0 };
        };

        bool _HasReparsePoint(const std::filesystem::path& absolutePath, std::string& error);

        class RegistryLock
        {
        public:
            static std::optional<RegistryLock> Acquire(
                const std::filesystem::path& root,
                std::string& error)
            {
                std::error_code directoryError;
                std::filesystem::create_directories(root, directoryError);
                if (directoryError)
                {
                    error = "Could not create the provider registry: " + std::to_string(directoryError.value());
                    return std::nullopt;
                }

                const auto path = root / L"registry.lock";
                for (size_t attempt = 0; attempt < 50; ++attempt)
                {
                    wil::unique_hfile file{ CreateFileW(
                        path.c_str(),
                        GENERIC_READ | GENERIC_WRITE,
                        0,
                        nullptr,
                        OPEN_ALWAYS,
                        FILE_ATTRIBUTE_NORMAL,
                        nullptr) };
                    if (file)
                    {
                        return RegistryLock{ std::move(file) };
                    }
                    const auto lastError = GetLastError();
                    if (lastError != ERROR_SHARING_VIOLATION && lastError != ERROR_LOCK_VIOLATION)
                    {
                        error = "Could not lock the provider registry: " + std::to_string(lastError);
                        return std::nullopt;
                    }
                    std::this_thread::sleep_for(std::chrono::milliseconds{ 100 });
                }
                error = "Timed out waiting for the provider registry lock";
                return std::nullopt;
            }

            RegistryLock(RegistryLock&&) noexcept = default;
            RegistryLock& operator=(RegistryLock&&) noexcept = default;

        private:
            explicit RegistryLock(wil::unique_hfile file) :
                _file{ std::move(file) }
            {
            }

            wil::unique_hfile _file;
        };

        class RegistryDirectoryGuards
        {
        public:
            RegistryDirectoryGuards() = default;

            static std::optional<RegistryDirectoryGuards> Acquire(
                const std::filesystem::path& root,
                std::string& error)
            {
                static constexpr std::array<std::wstring_view, 4> children{
                    L"installed",
                    L"staging",
                    L"registrations",
                    L"removals",
                };
                std::error_code directoryError;
                std::filesystem::create_directories(root, directoryError);
                for (const auto child : children)
                {
                    std::filesystem::create_directories(root / child, directoryError);
                }
                if (directoryError)
                {
                    error = "Could not create provider registry directories: " + std::to_string(directoryError.value());
                    return std::nullopt;
                }
                if (_HasReparsePoint(root, error))
                {
                    return std::nullopt;
                }

                RegistryDirectoryGuards guards;
                const auto open = [&](const std::filesystem::path& path) {
                    wil::unique_hfile handle{ CreateFileW(
                        path.c_str(),
                        FILE_READ_ATTRIBUTES,
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                        nullptr,
                        OPEN_EXISTING,
                        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                        nullptr) };
                    if (!handle)
                    {
                        error = "Could not guard provider registry directory: " + std::to_string(GetLastError());
                        return false;
                    }
                    FILE_ATTRIBUTE_TAG_INFO tag{};
                    if (!GetFileInformationByHandleEx(
                            handle.get(),
                            FileAttributeTagInfo,
                            &tag,
                            sizeof(tag)) ||
                        (tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0)
                    {
                        error = "Provider registry directories cannot be reparse points";
                        return false;
                    }
                    guards._handles.emplace_back(std::move(handle));
                    return true;
                };

                if (!open(root))
                {
                    return std::nullopt;
                }
                for (const auto child : children)
                {
                    if (!open(root / child))
                    {
                        return std::nullopt;
                    }
                }
                return guards;
            }

            RegistryDirectoryGuards(RegistryDirectoryGuards&&) noexcept = default;
            RegistryDirectoryGuards& operator=(RegistryDirectoryGuards&&) noexcept = default;

        private:
            std::vector<wil::unique_hfile> _handles;
        };

        std::optional<std::string> _PathToUtf8(const std::filesystem::path& path)
        {
            const auto value = path.native();
            if (value.empty() || value.size() > static_cast<size_t>((std::numeric_limits<int>::max)()))
            {
                return std::nullopt;
            }
            const auto required = WideCharToMultiByte(
                CP_UTF8,
                WC_ERR_INVALID_CHARS,
                value.data(),
                static_cast<int>(value.size()),
                nullptr,
                0,
                nullptr,
                nullptr);
            if (required <= 0)
            {
                return std::nullopt;
            }
            std::string result(static_cast<size_t>(required), '\0');
            if (WideCharToMultiByte(
                    CP_UTF8,
                    WC_ERR_INVALID_CHARS,
                    value.data(),
                    static_cast<int>(value.size()),
                    result.data(),
                    required,
                    nullptr,
                    nullptr) != required)
            {
                return std::nullopt;
            }
            return result;
        }

        std::optional<std::filesystem::path> _PathFromUtf8(const std::string_view value)
        {
            if (value.empty() || value.size() > static_cast<size_t>((std::numeric_limits<int>::max)()))
            {
                return std::nullopt;
            }
            const auto required = MultiByteToWideChar(
                CP_UTF8,
                MB_ERR_INVALID_CHARS,
                value.data(),
                static_cast<int>(value.size()),
                nullptr,
                0);
            if (required <= 0)
            {
                return std::nullopt;
            }
            std::wstring result(static_cast<size_t>(required), L'\0');
            if (MultiByteToWideChar(
                    CP_UTF8,
                    MB_ERR_INVALID_CHARS,
                    value.data(),
                    static_cast<int>(value.size()),
                    result.data(),
                    required) != required)
            {
                return std::nullopt;
            }
            return std::filesystem::path{ result };
        }

        bool _ReadFile(
            const std::filesystem::path& path,
            const size_t maximumSize,
            std::string& contents,
            std::string& error)
        {
            std::error_code fileError;
            const auto size = std::filesystem::file_size(path, fileError);
            if (fileError)
            {
                error = "Could not read '" + path.string() + "': " + std::to_string(fileError.value());
                return false;
            }
            if (size == 0 || size > maximumSize)
            {
                error = "File is empty or exceeds its size limit: " + path.string();
                return false;
            }

            std::ifstream stream{ path, std::ios::binary };
            if (!stream)
            {
                error = "Could not open '" + path.string() + "'";
                return false;
            }
            contents.assign(static_cast<size_t>(size), '\0');
            stream.read(contents.data(), static_cast<std::streamsize>(contents.size()));
            if (!stream)
            {
                error = "Could not read the complete file '" + path.string() + "'";
                return false;
            }
            return true;
        }

        bool _HasReparsePoint(const std::filesystem::path& absolutePath, std::string& error)
        {
            if (!absolutePath.is_absolute())
            {
                error = "Provider paths must be absolute";
                return true;
            }

            auto current = absolutePath.root_path();
            for (const auto& component : absolutePath.relative_path())
            {
                current /= component;
                const auto attributes = GetFileAttributesW(current.c_str());
                if (attributes == INVALID_FILE_ATTRIBUTES)
                {
                    error = "Could not inspect provider path component '" + current.string() + "'";
                    return true;
                }
                if ((attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0)
                {
                    error = "Provider paths cannot contain reparse points: " + current.string();
                    return true;
                }
            }
            return false;
        }

        bool _EnumeratePayload(
            const std::filesystem::path& root,
            PayloadTree& payload,
            std::string& error)
        {
            payload = {};
            if (_HasReparsePoint(root, error))
            {
                return false;
            }

            std::error_code iteratorError;
            std::filesystem::recursive_directory_iterator iterator{
                root,
                std::filesystem::directory_options::none,
                iteratorError
            };
            const std::filesystem::recursive_directory_iterator end;
            if (iteratorError)
            {
                error = "Could not enumerate provider payload: " + std::to_string(iteratorError.value());
                return false;
            }

            for (; iterator != end; iterator.increment(iteratorError))
            {
                if (iteratorError)
                {
                    error = "Could not enumerate provider payload: " + std::to_string(iteratorError.value());
                    return false;
                }

                const auto attributes = GetFileAttributesW(iterator->path().c_str());
                if (attributes == INVALID_FILE_ATTRIBUTES ||
                    (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0)
                {
                    error = "Provider payload contains an unreadable path or reparse point: " + iterator->path().string();
                    return false;
                }
                if ((attributes & FILE_ATTRIBUTE_DIRECTORY) != 0)
                {
                    continue;
                }
                if ((attributes & FILE_ATTRIBUTE_DEVICE) != 0)
                {
                    error = "Provider payload contains an unsupported file: " + iterator->path().string();
                    return false;
                }

                const auto relative = iterator->path().lexically_relative(root);
                if (!IsSafeRelativeProviderPath(relative))
                {
                    error = "Provider payload escaped its root";
                    return false;
                }
                const auto size = iterator->file_size(iteratorError);
                if (iteratorError)
                {
                    error = "Could not read provider file size: " + std::to_string(iteratorError.value());
                    return false;
                }
                if (size > ProviderRegistry::MaximumPayloadSize ||
                    payload.totalSize > ProviderRegistry::MaximumPayloadSize - size)
                {
                    error = "Provider payload exceeds 32 MiB";
                    return false;
                }
                payload.totalSize += size;
                payload.files.emplace_back(relative);
                if (payload.files.size() > ProviderRegistry::MaximumPayloadFileCount)
                {
                    error = "Provider payload exceeds 256 files";
                    return false;
                }
                if (iteratorError)
                {
                    error = "Could not enumerate provider payload: " + std::to_string(iteratorError.value());
                    return false;
                }
            }

            if (std::find(payload.files.begin(), payload.files.end(), std::filesystem::path{ ManifestFilename }) ==
                payload.files.end())
            {
                error = "Provider payload does not contain provider.json";
                return false;
            }
            std::sort(payload.files.begin(), payload.files.end());
            return true;
        }

        bool _HashPayload(
            const std::filesystem::path& root,
            const PayloadTree& payload,
            std::string& hashText,
            std::string& error,
            std::string* manifestContents = nullptr)
        {
            BCRYPT_ALG_HANDLE algorithm = nullptr;
            if (BCryptOpenAlgorithmProvider(&algorithm, BCRYPT_SHA256_ALGORITHM, nullptr, 0) < 0)
            {
                error = "Could not initialize SHA-256";
                return false;
            }
            const auto closeAlgorithm = wil::scope_exit([&]() {
                BCryptCloseAlgorithmProvider(algorithm, 0);
            });

            DWORD objectSize = 0;
            DWORD hashSize = 0;
            DWORD bytes = 0;
            if (BCryptGetProperty(
                    algorithm,
                    BCRYPT_OBJECT_LENGTH,
                    reinterpret_cast<PUCHAR>(&objectSize),
                    sizeof(objectSize),
                    &bytes,
                    0) < 0 ||
                BCryptGetProperty(
                    algorithm,
                    BCRYPT_HASH_LENGTH,
                    reinterpret_cast<PUCHAR>(&hashSize),
                    sizeof(hashSize),
                    &bytes,
                    0) < 0)
            {
                error = "Could not query SHA-256 properties";
                return false;
            }

            std::vector<UCHAR> object(objectSize);
            std::vector<UCHAR> hash(hashSize);
            BCRYPT_HASH_HANDLE handle = nullptr;
            if (BCryptCreateHash(
                    algorithm,
                    &handle,
                    object.data(),
                    static_cast<ULONG>(object.size()),
                    nullptr,
                    0,
                    0) < 0)
            {
                error = "Could not create SHA-256 hash";
                return false;
            }
            const auto destroyHash = wil::scope_exit([&]() {
                BCryptDestroyHash(handle);
            });

            const auto hashBytes = [&](const void* data, const size_t size) {
                return size <= static_cast<size_t>((std::numeric_limits<ULONG>::max)()) &&
                       BCryptHashData(
                           handle,
                           reinterpret_cast<PUCHAR>(const_cast<void*>(data)),
                           static_cast<ULONG>(size),
                           0) >= 0;
            };
            const auto hashUint64 = [&](const uint64_t value) {
                std::array<UCHAR, sizeof(value)> encoded{};
                for (size_t index = 0; index < encoded.size(); ++index)
                {
                    encoded[index] = static_cast<UCHAR>((value >> (index * 8)) & 0xff);
                }
                return hashBytes(encoded.data(), encoded.size());
            };

            static constexpr std::array<UCHAR, 16> framing{
                'I', 'T', 'R', 'T', 'P', 'A', 'Y', 'L',
                'O', 'A', 'D', 0, 1, 0, 0, 0
            };
            if (!hashBytes(framing.data(), framing.size()) ||
                !hashUint64(payload.files.size()))
            {
                error = "Could not hash provider framing";
                return false;
            }

            std::array<char, 64 * 1024> buffer{};
            for (const auto& relative : payload.files)
            {
                const auto relativeUtf8 = _PathToUtf8(relative.generic_wstring());
                if (!relativeUtf8)
                {
                    error = "Provider payload contains a path that is not valid Unicode";
                    return false;
                }
                const auto filePath = root / relative;
                wil::unique_hfile file{ CreateFileW(
                    filePath.c_str(),
                    GENERIC_READ,
                    FILE_SHARE_READ,
                    nullptr,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                    nullptr) };
                FILE_ATTRIBUTE_TAG_INFO attributes{};
                LARGE_INTEGER size{};
                if (!file ||
                    !GetFileInformationByHandleEx(
                        file.get(),
                        FileAttributeTagInfo,
                        &attributes,
                        sizeof(attributes)) ||
                    (attributes.FileAttributes &
                     (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_DEVICE | FILE_ATTRIBUTE_REPARSE_POINT)) != 0 ||
                    !GetFileSizeEx(file.get(), &size) ||
                    size.QuadPart < 0)
                {
                    error = "Could not securely open provider payload while hashing";
                    return false;
                }
                const auto declaredSize = static_cast<uint64_t>(size.QuadPart);
                if (declaredSize > ProviderRegistry::MaximumPayloadSize ||
                    !hashUint64(relativeUtf8->size()) ||
                    !hashBytes(relativeUtf8->data(), relativeUtf8->size()) ||
                    !hashUint64(declaredSize))
                {
                    error = "Could not hash provider file framing";
                    return false;
                }

                const auto captureManifest =
                    manifestContents != nullptr &&
                    relative == std::filesystem::path{ ManifestFilename };
                if (captureManifest)
                {
                    manifestContents->clear();
                    manifestContents->reserve(static_cast<size_t>(declaredSize));
                }
                uint64_t bytesRead = 0;
                while (bytesRead < declaredSize)
                {
                    DWORD count = 0;
                    const auto remaining = declaredSize - bytesRead;
                    const auto requested = static_cast<DWORD>(
                        (std::min)(remaining, static_cast<uint64_t>(buffer.size())));
                    if (!ReadFile(file.get(), buffer.data(), requested, &count, nullptr) ||
                        count == 0 ||
                        !hashBytes(buffer.data(), count))
                    {
                        error = "Could not read provider payload while hashing";
                        return false;
                    }
                    if (captureManifest)
                    {
                        manifestContents->append(buffer.data(), count);
                    }
                    bytesRead += count;
                }
                LARGE_INTEGER finalSize{};
                if (!GetFileSizeEx(file.get(), &finalSize) ||
                    bytesRead != declaredSize ||
                    finalSize.QuadPart != size.QuadPart)
                {
                    error = "Provider payload changed while it was being hashed";
                    return false;
                }
            }

            if (BCryptFinishHash(handle, hash.data(), static_cast<ULONG>(hash.size()), 0) < 0)
            {
                error = "Could not finish SHA-256 hash";
                return false;
            }

            static constexpr char digits[]{ "0123456789abcdef" };
            hashText.clear();
            hashText.reserve(hash.size() * 2);
            for (const auto value : hash)
            {
                hashText.push_back(digits[value >> 4]);
                hashText.push_back(digits[value & 0x0f]);
            }
            return true;
        }

        RegistryResult<Manifest> _LoadManifest(const std::filesystem::path& manifestPath)
        {
            RegistryResult<Manifest> result;
            std::string contents;
            std::string error;
            if (!_ReadFile(manifestPath, MaximumManifestSize, contents, error))
            {
                result.errors.emplace_back(std::move(error));
                return result;
            }
            const auto parsed = ParseManifest(contents, manifestPath.parent_path());
            if (!parsed)
            {
                result.errors = parsed.errors;
                return result;
            }
            result.value = *parsed.value;
            return result;
        }

        bool _WriteAtomic(
            const std::filesystem::path& target,
            const std::string_view contents,
            std::string& error)
        {
            std::error_code directoryError;
            std::filesystem::create_directories(target.parent_path(), directoryError);
            if (directoryError)
            {
                error = "Could not create registration directory: " + std::to_string(directoryError.value());
                return false;
            }

            auto temporary = target;
            temporary += L".tmp-" + std::to_wstring(GetCurrentProcessId()) + L"-" + std::to_wstring(GetTickCount64());
            wil::unique_hfile file{ CreateFileW(
                temporary.c_str(),
                GENERIC_WRITE,
                0,
                nullptr,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                nullptr) };
            if (!file)
            {
                error = "Could not create temporary registration: " + std::to_string(GetLastError());
                return false;
            }
            const auto cleanup = wil::scope_exit([&]() {
                file.reset();
                DeleteFileW(temporary.c_str());
            });

            size_t offset = 0;
            while (offset < contents.size())
            {
                DWORD written = 0;
                const auto chunk = static_cast<DWORD>(std::min<size_t>(
                    contents.size() - offset,
                    (std::numeric_limits<DWORD>::max)()));
                if (!WriteFile(file.get(), contents.data() + offset, chunk, &written, nullptr))
                {
                    error = "Could not write registration: " + std::to_string(GetLastError());
                    return false;
                }
                offset += written;
            }
            if (!FlushFileBuffers(file.get()))
            {
                error = "Could not flush registration: " + std::to_string(GetLastError());
                return false;
            }
            file.reset();
            if (!MoveFileExW(
                    temporary.c_str(),
                    target.c_str(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH))
            {
                error = "Could not publish registration: " + std::to_string(GetLastError());
                return false;
            }
            return true;
        }

        bool _ReadRegistrationJson(
            const std::filesystem::path& path,
            Json::Value& root,
            std::string& error)
        {
            std::string contents;
            if (!_ReadFile(path, MaximumManifestSize, contents, error))
            {
                return false;
            }
            Json::CharReaderBuilder builder;
            builder["allowComments"] = false;
            builder["allowTrailingCommas"] = false;
            builder["collectComments"] = false;
            builder["failIfExtra"] = true;
            builder["rejectDupKeys"] = true;
            builder["stackLimit"] = 16;
            builder["strictRoot"] = true;
            std::istringstream stream{ contents };
            try
            {
                return Json::parseFromStream(builder, stream, &root, &error);
            }
            catch (const Json::Exception& exception)
            {
                error = exception.what();
                return false;
            }
        }

        std::filesystem::path _RegistrationPath(
            const std::filesystem::path& root,
            const std::string_view id)
        {
            return root / L"registrations" /
                   (std::wstring{ id.begin(), id.end() } + L".json");
        }

        std::filesystem::path _ManagedRoot(
            const std::filesystem::path& root,
            const std::string_view id)
        {
            return root / L"installed" / std::wstring{ id.begin(), id.end() };
        }

        std::filesystem::path _PendingRoot(
            const std::filesystem::path& root,
            const std::string_view id)
        {
            return root / L"staging" /
                   (std::wstring{ id.begin(), id.end() } + L".pending");
        }

        std::filesystem::path _BackupRoot(
            const std::filesystem::path& root,
            const std::string_view id)
        {
            return root / L"staging" /
                   (std::wstring{ id.begin(), id.end() } + L".backup");
        }

        std::filesystem::path _RemovalPath(
            const std::filesystem::path& root,
            const std::string_view id)
        {
            return root / L"removals" /
                   (std::wstring{ id.begin(), id.end() } + L".json");
        }

        bool _RemoveTree(const std::filesystem::path& path, std::string& error)
        {
            const auto rootAttributes = GetFileAttributesW(path.c_str());
            if (rootAttributes == INVALID_FILE_ATTRIBUTES)
            {
                const auto lastError = GetLastError();
                if (lastError == ERROR_FILE_NOT_FOUND || lastError == ERROR_PATH_NOT_FOUND)
                {
                    return true;
                }
                error = "Could not inspect removal path: " + std::to_string(lastError);
                return false;
            }
            if ((rootAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0)
            {
                std::error_code existsError;
                std::filesystem::remove(path, existsError);
                if (existsError)
                {
                    error = "Could not remove reparse point: " + std::to_string(existsError.value());
                    return false;
                }
                return true;
            }

            std::vector<std::filesystem::path> entries;
            std::error_code iteratorError;
            for (std::filesystem::recursive_directory_iterator iterator{
                     path,
                     std::filesystem::directory_options::none,
                     iteratorError
                 },
                 end;
                 iterator != end;
                 iterator.increment(iteratorError))
            {
                if (iteratorError)
                {
                    error = "Could not inspect removal tree: " + std::to_string(iteratorError.value());
                    return false;
                }
                const auto attributes = GetFileAttributesW(iterator->path().c_str());
                if (attributes == INVALID_FILE_ATTRIBUTES)
                {
                    error = "Could not inspect removal tree entry: " + iterator->path().string();
                    return false;
                }
                if ((attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0)
                {
                    iterator.disable_recursion_pending();
                }
                entries.emplace_back(iterator->path());
            }
            if (iteratorError)
            {
                error = "Could not inspect removal tree: " + std::to_string(iteratorError.value());
                return false;
            }

            std::error_code removeError;
            for (auto iterator = entries.rbegin(); iterator != entries.rend(); ++iterator)
            {
                std::filesystem::remove(*iterator, removeError);
                if (removeError)
                {
                    error = "Could not remove '" + iterator->string() + "': " + std::to_string(removeError.value());
                    return false;
                }
            }
            std::filesystem::remove(path, removeError);
            if (removeError)
            {
                error = "Could not remove '" + path.string() + "': " + std::to_string(removeError.value());
                return false;
            }
            return true;
        }
    }

    ProviderRegistry::ProviderRegistry(std::filesystem::path root) :
        _root{ root.empty() ? DefaultRoot() : std::move(root) }
    {
    }

    const std::filesystem::path& ProviderRegistry::Root() const noexcept
    {
        return _root;
    }

    std::filesystem::path ProviderRegistry::DefaultRoot()
    {
        const auto stateRoot = ::IntelligentTerminal::StateRoot();
        return stateRoot.empty() ? stateRoot : stateRoot / L"RichTabProviders";
    }

    RegistryResult<Registration> ProviderRegistry::Install(const std::filesystem::path& manifestPath)
    {
        return _Register(manifestPath, RegistrationKind::Managed);
    }

    RegistryResult<Registration> ProviderRegistry::RegisterDevelopment(const std::filesystem::path& manifestPath)
    {
        return _Register(manifestPath, RegistrationKind::Development);
    }

    RegistryResult<Registration> ProviderRegistry::_Register(
        const std::filesystem::path& manifestPath,
        const RegistrationKind kind)
    {
        RegistryResult<Registration> result;
        if (_root.empty())
        {
            result.errors.emplace_back("Provider registry root is unavailable");
            return result;
        }

        std::error_code pathError;
        const auto absoluteManifest = std::filesystem::absolute(manifestPath, pathError);
        if (pathError)
        {
            result.errors.emplace_back("Manifest path is invalid: " + std::to_string(pathError.value()));
            return result;
        }
        std::string validationError;
        if (_HasReparsePoint(absoluteManifest, validationError))
        {
            result.errors.emplace_back(std::move(validationError));
            return result;
        }

        const auto sourceManifest = _LoadManifest(absoluteManifest);
        if (!sourceManifest)
        {
            result.errors = sourceManifest.errors;
            return result;
        }

        PayloadTree sourcePayload;
        if (!_EnumeratePayload(absoluteManifest.parent_path(), sourcePayload, validationError))
        {
            result.errors.emplace_back(std::move(validationError));
            return result;
        }

        std::string sourceHash;
        if (!_HashPayload(absoluteManifest.parent_path(), sourcePayload, sourceHash, validationError))
        {
            result.errors.emplace_back(std::move(validationError));
            return result;
        }

        std::string lockError;
        auto lock = RegistryLock::Acquire(_root, lockError);
        if (!lock)
        {
            result.errors.emplace_back(std::move(lockError));
            return result;
        }
        auto guards = RegistryDirectoryGuards::Acquire(_root, lockError);
        if (!guards || !_RecoverRemovals(lockError))
        {
            result.errors.emplace_back(std::move(lockError));
            return result;
        }

        std::optional<StoredRegistration> previous;
        if (const auto loaded = _LoadStored(sourceManifest.value->id); loaded)
        {
            previous = *loaded.value;
            if (previous->kind != kind)
            {
                result.errors.emplace_back("A provider with this id is registered from a different source type");
                return result;
            }
            if (!_RecoverManaged(*previous, validationError))
            {
                result.errors.emplace_back(std::move(validationError));
                return result;
            }
        }
        else if (std::filesystem::exists(_RegistrationPath(_root, sourceManifest.value->id)))
        {
            result.errors = loaded.errors;
            return result;
        }

        StoredRegistration stored;
        stored.id = sourceManifest.value->id;
        stored.kind = kind;
        stored.enabled = false;

        if (kind == RegistrationKind::Development)
        {
            stored.root = absoluteManifest.parent_path();
            stored.payloadHash = std::move(sourceHash);
            stored.enabled = previous &&
                             previous->payloadHash == stored.payloadHash &&
                             previous->enabled;
            auto materialized = _Materialize(stored);
            if (!materialized)
            {
                return materialized;
            }
            if (!_WriteStored(stored, validationError))
            {
                result.errors.emplace_back(std::move(validationError));
                return result;
            }
            return materialized;
        }

        const auto pending = _PendingRoot(_root, stored.id);
        const auto backup = _BackupRoot(_root, stored.id);
        const auto target = _ManagedRoot(_root, stored.id);
        std::error_code directoryError;
        std::filesystem::create_directories(target.parent_path(), directoryError);
        std::filesystem::create_directories(pending.parent_path(), directoryError);
        if (directoryError)
        {
            result.errors.emplace_back("Could not create provider storage directories: " + std::to_string(directoryError.value()));
            return result;
        }
        if (!_RemoveTree(pending, validationError) ||
            !_RemoveTree(backup, validationError))
        {
            result.errors.emplace_back(std::move(validationError));
            return result;
        }

        for (const auto& relative : sourcePayload.files)
        {
            std::error_code copyError;
            std::filesystem::create_directories((pending / relative).parent_path(), copyError);
            if (copyError ||
                !std::filesystem::copy_file(
                    absoluteManifest.parent_path() / relative,
                    pending / relative,
                    std::filesystem::copy_options::none,
                    copyError))
            {
                _RemoveTree(pending, validationError);
                result.errors.emplace_back("Could not stage provider payload: " + std::to_string(copyError.value()));
                return result;
            }
        }

        const auto stagedManifest = _LoadManifest(pending / ManifestFilename);
        if (!stagedManifest || stagedManifest.value->id != stored.id)
        {
            _RemoveTree(pending, validationError);
            result.errors = stagedManifest.errors;
            result.errors.emplace_back("Staged provider manifest did not preserve its id");
            return result;
        }
        PayloadTree stagedPayload;
        if (!_EnumeratePayload(pending, stagedPayload, validationError) ||
            !_HashPayload(pending, stagedPayload, stored.payloadHash, validationError))
        {
            _RemoveTree(pending, validationError);
            result.errors.emplace_back(std::move(validationError));
            return result;
        }
        if (stored.payloadHash != sourceHash)
        {
            _RemoveTree(pending, validationError);
            result.errors.emplace_back("Provider source changed while it was being installed");
            return result;
        }
        stored.enabled = previous &&
                         previous->payloadHash == stored.payloadHash &&
                         previous->enabled;

        std::error_code moveError;
        if (std::filesystem::exists(target, moveError))
        {
            std::filesystem::rename(target, backup, moveError);
            if (moveError)
            {
                _RemoveTree(pending, validationError);
                result.errors.emplace_back("Could not preserve the previous provider: " + std::to_string(moveError.value()));
                return result;
            }
        }

        std::filesystem::rename(pending, target, moveError);
        if (moveError)
        {
            if (std::filesystem::exists(backup))
            {
                std::error_code restoreError;
                std::filesystem::rename(backup, target, restoreError);
            }
            _RemoveTree(pending, validationError);
            result.errors.emplace_back("Could not publish provider payload: " + std::to_string(moveError.value()));
            return result;
        }

        stored.root = target;
        if (!_WriteStored(stored, validationError))
        {
            const auto writeError = validationError;
            std::string cleanupError;
            _RemoveTree(target, cleanupError);
            if (std::filesystem::exists(backup))
            {
                std::error_code restoreError;
                std::filesystem::rename(backup, target, restoreError);
            }
            result.errors.emplace_back(writeError);
            return result;
        }

        _RemoveTree(backup, validationError);
        return _Materialize(stored);
    }

    RegistryResult<std::vector<ProviderRegistry::StoredRegistration>> ProviderRegistry::_LoadAllStored()
    {
        RegistryResult<std::vector<StoredRegistration>> result;
        std::vector<StoredRegistration> stored;
        const auto registrations = _root / L"registrations";
        std::error_code iteratorError;
        if (!std::filesystem::exists(registrations, iteratorError))
        {
            result.value = std::move(stored);
            return result;
        }

        for (std::filesystem::directory_iterator iterator{ registrations, iteratorError }, end;
             iterator != end;
             iterator.increment(iteratorError))
        {
            if (iteratorError)
            {
                result.errors.emplace_back("Could not enumerate provider registrations: " + std::to_string(iteratorError.value()));
                break;
            }
            if (iterator->path().extension() != L".json")
            {
                continue;
            }

            const auto id = iterator->path().stem().string();
            const auto loaded = _LoadStored(id);
            if (loaded)
            {
                stored.emplace_back(*loaded.value);
            }
            else
            {
                result.errors.insert(result.errors.end(), loaded.errors.begin(), loaded.errors.end());
            }
        }
        std::sort(stored.begin(), stored.end(), [](const auto& first, const auto& second) {
            return first.id < second.id;
        });
        result.value = std::move(stored);
        return result;
    }

    RegistryResult<std::vector<Registration>> ProviderRegistry::List()
    {
        RegistryResult<std::vector<Registration>> result;
        std::string lockError;
        auto lock = RegistryLock::Acquire(_root, lockError);
        if (!lock)
        {
            result.errors.emplace_back(std::move(lockError));
            return result;
        }
        auto guards = RegistryDirectoryGuards::Acquire(_root, lockError);
        if (!guards || !_RecoverRemovals(lockError))
        {
            result.errors.emplace_back(std::move(lockError));
            return result;
        }

        const auto stored = _LoadAllStored();
        if (!stored)
        {
            result.errors = stored.errors;
            return result;
        }
        result.errors = stored.errors;

        std::vector<Registration> registrations;
        for (const auto& item : *stored.value)
        {
            std::string recoveryError;
            if (!_RecoverManaged(item, recoveryError))
            {
                result.errors.emplace_back(std::move(recoveryError));
                continue;
            }
            const auto materialized = _Materialize(item);
            if (materialized)
            {
                registrations.emplace_back(*materialized.value);
            }
            result.errors.insert(result.errors.end(), materialized.errors.begin(), materialized.errors.end());
        }
        result.value = std::move(registrations);
        return result;
    }

    RegistryResult<ProviderRegistry::StoredRegistration> ProviderRegistry::_LoadStored(const std::string_view id)
    {
        RegistryResult<StoredRegistration> result;
        if (!IsCanonicalProviderId(id))
        {
            result.errors.emplace_back("Provider id is invalid");
            return result;
        }

        Json::Value root;
        std::string error;
        const auto path = _RegistrationPath(_root, id);
        if (!_ReadRegistrationJson(path, root, error) || !root.isObject())
        {
            result.errors.emplace_back("Could not load registration for '" + std::string{ id } + "': " + error);
            return result;
        }
        static const std::set<std::string> allowed{
            "schemaVersion",
            "id",
            "kind",
            "root",
            "payloadHash",
            "enabled",
        };
        for (const auto& member : root.getMemberNames())
        {
            if (!allowed.contains(member))
            {
                result.errors.emplace_back("Registration contains unknown member '" + member + "'");
                return result;
            }
        }
        if (!root["schemaVersion"].isUInt() || root["schemaVersion"].asUInt() != 1 ||
            !root["id"].isString() || root["id"].asString() != id ||
            !root["kind"].isString() ||
            !root["root"].isString() ||
            !root["payloadHash"].isString() || root["payloadHash"].asString().size() != 64 ||
            !root["enabled"].isBool())
        {
            result.errors.emplace_back("Registration has an invalid shape");
            return result;
        }
        const auto payloadHash = root["payloadHash"].asString();
        if (!std::all_of(payloadHash.begin(), payloadHash.end(), [](const char value) {
                return (value >= '0' && value <= '9') || (value >= 'a' && value <= 'f');
            }))
        {
            result.errors.emplace_back("Registration payload hash is invalid");
            return result;
        }
        const auto registrationRoot = _PathFromUtf8(root["root"].asString());
        if (!registrationRoot || !registrationRoot->is_absolute())
        {
            result.errors.emplace_back("Registration root is invalid");
            return result;
        }

        StoredRegistration stored;
        stored.id = std::string{ id };
        if (root["kind"].asString() == "managed")
            stored.kind = RegistrationKind::Managed;
        else if (root["kind"].asString() == "development")
            stored.kind = RegistrationKind::Development;
        else
        {
            result.errors.emplace_back("Registration kind is invalid");
            return result;
        }
        stored.root = *registrationRoot;
        if (stored.kind == RegistrationKind::Managed &&
            stored.root.lexically_normal() != _ManagedRoot(_root, id).lexically_normal())
        {
            result.errors.emplace_back("Managed registration root is outside the registry");
            return result;
        }
        stored.payloadHash = payloadHash;
        stored.enabled = root["enabled"].asBool();
        result.value = std::move(stored);
        return result;
    }

    RegistryResult<Registration> ProviderRegistry::_Materialize(const StoredRegistration& stored)
    {
        RegistryResult<Registration> result;
        PayloadTree payload;
        std::string hash;
        std::string error;
        std::string manifestContents;
        if (!_EnumeratePayload(stored.root, payload, error) ||
            !_HashPayload(stored.root, payload, hash, error, &manifestContents))
        {
            result.errors.emplace_back(std::move(error));
            return result;
        }
        const auto manifest = ParseManifest(manifestContents, stored.root);
        if (!manifest)
        {
            result.errors = manifest.errors;
            return result;
        }
        if (manifest.value->id != stored.id)
        {
            result.errors.emplace_back("Registered provider id does not match provider.json");
            return result;
        }

        Registration registration;
        registration.manifest = *manifest.value;
        registration.kind = stored.kind;
        registration.root = stored.root;
        registration.payloadHash = std::move(hash);
        registration.enabled = stored.enabled;
        registration.integrityValid =
            stored.kind == RegistrationKind::Development ||
            registration.payloadHash == stored.payloadHash;
        if (!registration.integrityValid)
        {
            result.errors.emplace_back("Managed provider payload hash does not match its registration");
        }
        result.value = std::move(registration);
        return result;
    }

    bool ProviderRegistry::_WriteStored(const StoredRegistration& stored, std::string& error)
    {
        const auto rootUtf8 = _PathToUtf8(stored.root);
        if (!rootUtf8)
        {
            error = "Registration path is not valid Unicode";
            return false;
        }

        Json::Value root;
        root["schemaVersion"] = 1;
        root["id"] = stored.id;
        root["kind"] = stored.kind == RegistrationKind::Managed ? "managed" : "development";
        root["root"] = *rootUtf8;
        root["payloadHash"] = stored.payloadHash;
        root["enabled"] = stored.enabled;
        Json::StreamWriterBuilder builder;
        builder["indentation"] = "";
        return _WriteAtomic(
            _RegistrationPath(_root, stored.id),
            Json::writeString(builder, root),
            error);
    }

    bool ProviderRegistry::_DeleteStored(const std::string_view id, std::string& error)
    {
        const auto path = _RegistrationPath(_root, id);
        if (!DeleteFileW(path.c_str()))
        {
            error = "Could not delete provider registration: " + std::to_string(GetLastError());
            return false;
        }
        return true;
    }

    bool ProviderRegistry::_RecoverManaged(
        const StoredRegistration& stored,
        std::string& error)
    {
        if (stored.kind != RegistrationKind::Managed)
        {
            return true;
        }

        const auto pending = _PendingRoot(_root, stored.id);
        const auto backup = _BackupRoot(_root, stored.id);
        if (!_RemoveTree(pending, error))
        {
            return false;
        }

        std::error_code existsError;
        const auto backupExists = std::filesystem::exists(backup, existsError);
        if (existsError)
        {
            error = "Could not inspect provider recovery backup: " + std::to_string(existsError.value());
            return false;
        }
        if (!backupExists)
        {
            return true;
        }

        const auto hashTree = [&](const std::filesystem::path& root, std::string& hash) {
            PayloadTree payload;
            return _EnumeratePayload(root, payload, error) &&
                   _HashPayload(root, payload, hash, error);
        };

        std::string backupHash;
        if (!hashTree(backup, backupHash))
        {
            return false;
        }
        const auto targetExists = std::filesystem::exists(stored.root, existsError);
        if (existsError)
        {
            error = "Could not inspect provider recovery target: " + std::to_string(existsError.value());
            return false;
        }
        if (targetExists)
        {
            std::string targetHash;
            if (!hashTree(stored.root, targetHash))
            {
                return false;
            }
            if (targetHash == stored.payloadHash)
            {
                return _RemoveTree(backup, error);
            }
            if (backupHash != stored.payloadHash)
            {
                error = "Neither provider recovery payload matches the registered hash";
                return false;
            }
            if (!_RemoveTree(stored.root, error))
            {
                return false;
            }
        }
        else if (backupHash != stored.payloadHash)
        {
            error = "Provider recovery backup does not match the registered hash";
            return false;
        }

        std::error_code restoreError;
        std::filesystem::rename(backup, stored.root, restoreError);
        if (restoreError)
        {
            error = "Could not restore provider recovery backup: " + std::to_string(restoreError.value());
            return false;
        }
        return true;
    }

    bool ProviderRegistry::_RecoverRemovals(std::string& error)
    {
        const auto removals = _root / L"removals";
        std::error_code iteratorError;
        for (std::filesystem::directory_iterator iterator{ removals, iteratorError }, end;
             iterator != end;
             iterator.increment(iteratorError))
        {
            if (iteratorError)
            {
                error = "Could not enumerate provider removal transactions: " + std::to_string(iteratorError.value());
                return false;
            }
            if (iterator->path().extension() != L".json")
            {
                continue;
            }
            const auto id = iterator->path().stem().string();
            if (!IsCanonicalProviderId(id))
            {
                error = "Removal transaction has an invalid provider id";
                return false;
            }

            const auto registrationPath = _RegistrationPath(_root, id);
            const auto backup = _BackupRoot(_root, id);
            const auto target = _ManagedRoot(_root, id);
            std::error_code existsError;
            const auto registrationExists = std::filesystem::exists(registrationPath, existsError);
            const auto backupExists = std::filesystem::exists(backup, existsError);
            const auto targetExists = std::filesystem::exists(target, existsError);
            if (existsError)
            {
                error = "Could not inspect provider removal transaction: " + std::to_string(existsError.value());
                return false;
            }

            if (registrationExists && backupExists && !targetExists)
            {
                std::filesystem::rename(backup, target, iteratorError);
                if (iteratorError)
                {
                    error = "Could not roll back provider removal: " + std::to_string(iteratorError.value());
                    return false;
                }
            }
            else if (!registrationExists && backupExists)
            {
                if (!_RemoveTree(backup, error))
                {
                    return false;
                }
            }
            else if (registrationExists && backupExists && targetExists)
            {
                if (!_RemoveTree(backup, error))
                {
                    return false;
                }
            }

            if (!DeleteFileW(iterator->path().c_str()))
            {
                error = "Could not complete provider removal recovery: " + std::to_string(GetLastError());
                return false;
            }
        }
        if (iteratorError)
        {
            error = "Could not enumerate provider removal transactions: " + std::to_string(iteratorError.value());
            return false;
        }
        return true;
    }

    RegistryResult<Registration> ProviderRegistry::SetEnabled(
        const std::string_view id,
        const bool enabled)
    {
        RegistryResult<Registration> result;
        std::string lockError;
        auto lock = RegistryLock::Acquire(_root, lockError);
        if (!lock)
        {
            result.errors.emplace_back(std::move(lockError));
            return result;
        }
        auto guards = RegistryDirectoryGuards::Acquire(_root, lockError);
        if (!guards || !_RecoverRemovals(lockError))
        {
            result.errors.emplace_back(std::move(lockError));
            return result;
        }

        auto stored = _LoadStored(id);
        if (!stored)
        {
            result.errors = stored.errors;
            return result;
        }
        if (!enabled)
        {
            stored.value->enabled = false;
            std::string writeError;
            if (!_WriteStored(*stored.value, writeError))
            {
                result.errors.emplace_back(std::move(writeError));
                return result;
            }

            auto materialized = _Materialize(*stored.value);
            if (materialized)
            {
                result.value = std::move(*materialized.value);
            }
            else
            {
                Registration registration;
                registration.manifest.id = stored.value->id;
                registration.kind = stored.value->kind;
                registration.root = stored.value->root;
                registration.payloadHash = stored.value->payloadHash;
                registration.enabled = false;
                registration.integrityValid = false;
                result.value = std::move(registration);
                result.errors = std::move(materialized.errors);
            }
            return result;
        }
        if (!_RecoverManaged(*stored.value, lockError))
        {
            result.errors.emplace_back(std::move(lockError));
            return result;
        }
        auto materialized = _Materialize(*stored.value);
        if (!materialized)
        {
            result.errors = materialized.errors;
            return result;
        }
        if (enabled && !materialized.value->integrityValid)
        {
            result.errors = materialized.errors;
            result.errors.emplace_back("Refusing to enable a provider whose managed payload changed");
            return result;
        }

        stored.value->enabled = enabled;
        std::string writeError;
        if (!_WriteStored(*stored.value, writeError))
        {
            result.errors.emplace_back(std::move(writeError));
            return result;
        }
        materialized.value->enabled = enabled;
        result.value = std::move(*materialized.value);
        return result;
    }

    RegistryResult<bool> ProviderRegistry::Remove(const std::string_view id)
    {
        RegistryResult<bool> result;
        std::string lockError;
        auto lock = RegistryLock::Acquire(_root, lockError);
        if (!lock)
        {
            result.errors.emplace_back(std::move(lockError));
            return result;
        }
        auto guards = RegistryDirectoryGuards::Acquire(_root, lockError);
        if (!guards || !_RecoverRemovals(lockError))
        {
            result.errors.emplace_back(std::move(lockError));
            return result;
        }

        const auto stored = _LoadStored(id);
        if (!stored)
        {
            result.errors = stored.errors;
            return result;
        }
        if (!_RecoverManaged(*stored.value, lockError))
        {
            result.errors.emplace_back(std::move(lockError));
            return result;
        }

        const auto backup = _BackupRoot(_root, id);
        std::string operationError;
        Json::Value tombstone;
        tombstone["id"] = std::string{ id };
        Json::StreamWriterBuilder writer;
        writer["indentation"] = "";
        if (!_WriteAtomic(
                _RemovalPath(_root, id),
                Json::writeString(writer, tombstone),
                operationError))
        {
            result.errors.emplace_back(std::move(operationError));
            return result;
        }
        if (stored.value->kind == RegistrationKind::Managed)
        {
            if (!_RemoveTree(backup, operationError))
            {
                result.errors.emplace_back(std::move(operationError));
                return result;
            }
            std::error_code moveError;
            if (std::filesystem::exists(stored.value->root, moveError))
            {
                std::filesystem::rename(stored.value->root, backup, moveError);
                if (moveError)
                {
                    result.errors.emplace_back("Could not stage provider removal: " + std::to_string(moveError.value()));
                    return result;
                }
            }
        }

        if (!_DeleteStored(id, operationError))
        {
            if (stored.value->kind == RegistrationKind::Managed &&
                std::filesystem::exists(backup))
            {
                std::error_code restoreError;
                std::filesystem::rename(backup, stored.value->root, restoreError);
            }
            result.errors.emplace_back(std::move(operationError));
            return result;
        }
        if (!_RemoveTree(backup, operationError))
        {
            result.errors.emplace_back(std::move(operationError));
            return result;
        }
        if (!DeleteFileW(_RemovalPath(_root, id).c_str()))
        {
            result.errors.emplace_back("Could not complete provider removal transaction: " + std::to_string(GetLastError()));
            return result;
        }
        result.value = true;
        return result;
    }
}
