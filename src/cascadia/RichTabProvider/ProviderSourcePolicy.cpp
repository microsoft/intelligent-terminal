// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "ProviderSourcePolicy.h"

#include <windows.h>

#include <algorithm>
#include <array>

#include <wil/resource.h>

namespace Microsoft::Terminal::RichTab::Provider
{
    namespace
    {
        constexpr std::array<std::string_view, 1> ReservedBuiltInProviderIds{
            "com.microsoft.intelligent-terminal.git-status",
        };

        std::optional<std::string> _ToUtf8(const std::wstring_view value)
        {
            if (value.empty())
            {
                return std::string{};
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

        std::optional<std::string> _DevelopmentConsentKey(
            const ProviderSourceIdentity& identity)
        {
            if (identity.providerId.empty() ||
                identity.developmentRoot.empty() ||
                !identity.developmentRoot.is_absolute())
            {
                return std::nullopt;
            }

            wil::unique_hfile directory{ CreateFileW(
                identity.developmentRoot.c_str(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                nullptr,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                nullptr) };
            if (!directory)
            {
                return std::nullopt;
            }

            FILE_ID_INFO fileId{};
            if (!GetFileInformationByHandleEx(
                    directory.get(),
                    FileIdInfo,
                    &fileId,
                    sizeof(fileId)))
            {
                return std::nullopt;
            }

            constexpr char HexDigits[]{ "0123456789abcdef" };
            std::string filesystemId;
            filesystemId.reserve(16 + sizeof(fileId.FileId.Identifier) * 2);
            for (int shift = 60; shift >= 0; shift -= 4)
            {
                filesystemId.push_back(
                    HexDigits[(fileId.VolumeSerialNumber >> shift) & 0xf]);
            }
            for (const auto byte : fileId.FileId.Identifier)
            {
                filesystemId.push_back(HexDigits[byte >> 4]);
                filesystemId.push_back(HexDigits[byte & 0xf]);
            }
            return "dev|" + identity.providerId + "|" + filesystemId;
        }
    }

    ProviderSourceIdentity BuiltInSourceIdentity(std::string providerId)
    {
        ProviderSourceIdentity identity;
        identity.kind = ProviderSourceKind::BuiltIn;
        identity.providerId = std::move(providerId);
        return identity;
    }

    ProviderSourceIdentity AppExtensionSourceIdentity(
        std::string providerId,
        std::wstring extensionId,
        std::wstring packageFamilyName,
        std::wstring packageFullName,
        std::wstring publisherId,
        std::wstring packageVersion)
    {
        ProviderSourceIdentity identity;
        identity.kind = ProviderSourceKind::AppExtension;
        identity.providerId = std::move(providerId);
        identity.extensionId = std::move(extensionId);
        identity.packageFamilyName = std::move(packageFamilyName);
        identity.packageFullName = std::move(packageFullName);
        identity.publisherId = std::move(publisherId);
        identity.packageVersion = std::move(packageVersion);
        return identity;
    }

    ProviderSourceIdentity DevelopmentSourceIdentity(
        std::string providerId,
        std::filesystem::path root)
    {
        ProviderSourceIdentity identity;
        identity.kind = ProviderSourceKind::Development;
        identity.providerId = std::move(providerId);
        identity.developmentRoot = std::move(root);
        return identity;
    }

    ProviderSourceIdentity LegacyManagedSourceIdentity(
        std::string providerId,
        std::filesystem::path root)
    {
        ProviderSourceIdentity identity;
        identity.kind = ProviderSourceKind::LegacyManaged;
        identity.providerId = std::move(providerId);
        identity.developmentRoot = std::move(root);
        return identity;
    }

    bool IsReservedBuiltInProviderId(const std::string_view id) noexcept
    {
        return std::find(
                   ReservedBuiltInProviderIds.begin(),
                   ReservedBuiltInProviderIds.end(),
                   id) != ReservedBuiltInProviderIds.end();
    }

    int ProviderSourcePrecedence(const ProviderSourceKind source) noexcept
    {
        switch (source)
        {
        case ProviderSourceKind::BuiltIn:
            return 300;
        case ProviderSourceKind::AppExtension:
            return 200;
        case ProviderSourceKind::Development:
            return 100;
        default:
            return 0;
        }
    }

    ProviderConsentRequirement ConsentRequirementFor(
        const ProviderSourceKind source) noexcept
    {
        switch (source)
        {
        case ProviderSourceKind::BuiltIn:
            return ProviderConsentRequirement::NotRequired;
        case ProviderSourceKind::AppExtension:
        case ProviderSourceKind::Development:
            return ProviderConsentRequirement::Required;
        default:
            return ProviderConsentRequirement::SourceNotAllowed;
        }
    }

    std::optional<std::string> ProviderConsentKey(
        const ProviderSourceIdentity& identity)
    {
        switch (identity.kind)
        {
        case ProviderSourceKind::AppExtension:
        {
            if (identity.providerId.empty() ||
                identity.packageFamilyName.empty() ||
                identity.extensionId.empty())
            {
                return std::nullopt;
            }
            auto packageFamilyName = identity.packageFamilyName;
            CharLowerBuffW(
                packageFamilyName.data(),
                static_cast<DWORD>(packageFamilyName.size()));
            const auto package = _ToUtf8(packageFamilyName);
            const auto extension = _ToUtf8(identity.extensionId);
            if (!package || !extension)
            {
                return std::nullopt;
            }
            return "msix|" + identity.providerId + "|" + *package + "|" + *extension;
        }
        case ProviderSourceKind::Development:
            return _DevelopmentConsentKey(identity);
        default:
            return std::nullopt;
        }
    }
}
