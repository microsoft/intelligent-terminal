// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "ProviderContracts.h"

#include <json/json.h>

#include <algorithm>
#include <fstream>
#include <limits>
#include <set>
#include <sstream>
#include <windows.h>

namespace Microsoft::Terminal::RichTab::Provider
{
    namespace
    {
        bool _IsLowerIdentifier(const std::string_view value, const bool requireDot) noexcept
        {
            if (value.empty() || value.size() > 128 || (requireDot && value.find('.') == std::string_view::npos))
            {
                return false;
            }

            bool segmentStart = true;
            for (const auto ch : value)
            {
                if (ch == '.')
                {
                    if (segmentStart)
                    {
                        return false;
                    }
                    segmentStart = true;
                    continue;
                }
                if (segmentStart)
                {
                    if (ch < 'a' || ch > 'z')
                    {
                        return false;
                    }
                    segmentStart = false;
                }
                else if (!((ch >= 'a' && ch <= 'z') ||
                           (ch >= '0' && ch <= '9') ||
                           ch == '-'))
                {
                    return false;
                }
            }
            return !segmentStart;
        }

        bool _ReadJson(const std::string_view json, const size_t maximumSize, Json::Value& root, std::string& error)
        {
            if (json.empty() || json.size() > maximumSize)
            {
                error = json.empty() ? "JSON input is empty" : "JSON input exceeds the size limit";
                return false;
            }

            Json::CharReaderBuilder builder;
            builder["allowComments"] = false;
            builder["allowTrailingCommas"] = false;
            builder["collectComments"] = false;
            builder["failIfExtra"] = true;
            builder["rejectDupKeys"] = true;
            builder["stackLimit"] = 32;
            builder["strictRoot"] = true;
            std::istringstream stream{ std::string{ json } };
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

        std::optional<std::wstring> _Utf8ToWide(const std::string_view value)
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
            return result;
        }

        bool _IsValidUtf8(const std::string_view value)
        {
            return value.empty() ||
                   (value.size() <= static_cast<size_t>((std::numeric_limits<int>::max)()) &&
                    MultiByteToWideChar(
                        CP_UTF8,
                        MB_ERR_INVALID_CHARS,
                        value.data(),
                        static_cast<int>(value.size()),
                        nullptr,
                        0) > 0);
        }

        std::optional<std::string> _WideToUtf8(const std::wstring_view value)
        {
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

        void _RejectUnknownMembers(
            const Json::Value& object,
            const std::initializer_list<std::string_view> allowed,
            const std::string_view context,
            std::vector<std::string>& errors)
        {
            if (!object.isObject())
            {
                errors.emplace_back(std::string{ context } + " must be an object");
                return;
            }

            for (const auto& member : object.getMemberNames())
            {
                if (std::none_of(allowed.begin(), allowed.end(), [&](const auto candidate) {
                        return candidate == member;
                    }))
                {
                    errors.emplace_back(std::string{ context } + " contains unknown member '" + member + "'");
                }
            }
        }

        std::optional<std::string> _RequiredString(
            const Json::Value& object,
            const char* name,
            const size_t maximumSize,
            std::vector<std::string>& errors)
        {
            const auto& value = object[name];
            if (!value.isString() || value.asString().empty())
            {
                errors.emplace_back(std::string{ name } + " must be a non-empty string");
                return std::nullopt;
            }
            auto result = value.asString();
            if (result.size() > maximumSize ||
                result.find('\0') != std::string::npos ||
                !_IsValidUtf8(result))
            {
                errors.emplace_back(std::string{ name } + " exceeds its limit or is not valid UTF-8");
                return std::nullopt;
            }
            return result;
        }

        std::optional<ActivationEvent> _ActivationFromString(const std::string_view value)
        {
            if (value == "onPaneConnected")
                return ActivationEvent::PaneConnected;
            if (value == "onWorkingDirectoryChanged")
                return ActivationEvent::WorkingDirectoryChanged;
            if (value == "onCommandFinished")
                return ActivationEvent::CommandFinished;
            if (value == "onTabActivated")
                return ActivationEvent::TabActivated;
            if (value == "onManualRefresh")
                return ActivationEvent::ManualRefresh;
            return std::nullopt;
        }

        std::optional<FieldType> _FieldTypeFromString(const std::string_view value)
        {
            if (value == "string")
                return FieldType::String;
            if (value == "boolean")
                return FieldType::Boolean;
            if (value == "integer")
                return FieldType::Integer;
            if (value == "number")
                return FieldType::Number;
            return std::nullopt;
        }

        std::string_view _ActivationToString(const ActivationEvent value)
        {
            switch (value)
            {
            case ActivationEvent::PaneConnected:
                return "paneConnected";
            case ActivationEvent::WorkingDirectoryChanged:
                return "workingDirectoryChanged";
            case ActivationEvent::CommandFinished:
                return "commandFinished";
            case ActivationEvent::TabActivated:
                return "tabActivated";
            case ActivationEvent::ManualRefresh:
                return "manualRefresh";
            default:
                return {};
            }
        }
    }

    bool IsCanonicalProviderId(const std::string_view id) noexcept
    {
        return _IsLowerIdentifier(id, true);
    }

    bool IsCanonicalFieldId(const std::string_view id) noexcept
    {
        return _IsLowerIdentifier(id, false);
    }

    bool IsSafeRelativeProviderPath(const std::filesystem::path& path) noexcept
    {
        if (path.empty() || path.is_absolute() || path.has_root_name() || path.has_root_directory())
        {
            return false;
        }

        for (const auto& component : path)
        {
            if (component == L".." || component == L"." || component.empty())
            {
                return false;
            }
        }
        return true;
    }

    ParseResult<std::string> ReadManifestFile(const std::filesystem::path& path)
    {
        ParseResult<std::string> result;
        std::error_code sizeError;
        const auto size = std::filesystem::file_size(path, sizeError);
        if (sizeError || size == 0 || size > MaximumManifestSize ||
            size > static_cast<uint64_t>((std::numeric_limits<std::streamsize>::max)()))
        {
            result.errors.emplace_back("Provider manifest is missing or exceeds its size limit");
            return result;
        }

        std::ifstream stream{ path, std::ios::binary };
        if (!stream)
        {
            result.errors.emplace_back("Could not open provider manifest");
            return result;
        }

        std::string contents(static_cast<size_t>(size), '\0');
        if (!stream.read(contents.data(), static_cast<std::streamsize>(contents.size())))
        {
            result.errors.emplace_back("Could not read provider manifest");
            return result;
        }
        result.value = std::move(contents);
        return result;
    }

    ParseResult<Manifest> ParseManifest(
        const std::string_view json,
        const std::filesystem::path& extensionRoot)
    {
        ParseResult<Manifest> result;
        Json::Value root;
        std::string parseError;
        if (!_ReadJson(json, MaximumManifestSize, root, parseError))
        {
            result.errors.emplace_back("Invalid manifest JSON: " + parseError);
            return result;
        }
        if (!root.isObject())
        {
            result.errors.emplace_back("manifest must be an object");
            return result;
        }

        _RejectUnknownMembers(
            root,
            { "schemaVersion", "id", "displayName", "publisher", "version", "protocol", "runtime", "activationEvents", "fields" },
            "manifest",
            result.errors);

        Manifest manifest;
        manifest.extensionRoot = extensionRoot;

        if (!root["schemaVersion"].isUInt() ||
            root["schemaVersion"].asUInt() != CurrentManifestSchemaVersion)
        {
            result.errors.emplace_back("schemaVersion must be 1");
        }
        else
        {
            manifest.schemaVersion = root["schemaVersion"].asUInt();
        }

        if (const auto id = _RequiredString(root, "id", 128, result.errors))
        {
            manifest.id = *id;
            if (!IsCanonicalProviderId(manifest.id))
            {
                result.errors.emplace_back("id must be a lowercase reverse-DNS identifier");
            }
        }
        if (const auto value = _RequiredString(root, "displayName", 128, result.errors))
            manifest.displayName = *value;
        if (const auto value = _RequiredString(root, "publisher", 128, result.errors))
            manifest.publisher = *value;
        if (const auto value = _RequiredString(root, "version", 64, result.errors))
            manifest.version = *value;

        const auto& protocol = root["protocol"];
        if (!protocol.isObject())
        {
            result.errors.emplace_back("protocol must be an object");
        }
        else
        {
            _RejectUnknownMembers(protocol, { "minVersion", "maxVersion" }, "protocol", result.errors);
            if (!protocol["minVersion"].isUInt() || !protocol["maxVersion"].isUInt())
            {
                result.errors.emplace_back("protocol minVersion and maxVersion must be unsigned integers");
            }
            else
            {
                manifest.protocol.minimum = protocol["minVersion"].asUInt();
                manifest.protocol.maximum = protocol["maxVersion"].asUInt();
                if (manifest.protocol.minimum == 0 ||
                    manifest.protocol.minimum > manifest.protocol.maximum ||
                    CurrentProtocolVersion < manifest.protocol.minimum ||
                    CurrentProtocolVersion > manifest.protocol.maximum)
                {
                    result.errors.emplace_back("provider protocol range is invalid or incompatible with this host");
                }
            }
        }

        const auto& runtime = root["runtime"];
        if (!runtime.isObject())
        {
            result.errors.emplace_back("runtime must be an object");
        }
        else
        {
            _RejectUnknownMembers(runtime, { "type", "entrypoint", "arguments" }, "runtime", result.errors);
            if (const auto type = _RequiredString(runtime, "type", 32, result.errors))
            {
                if (*type == "nativeV1")
                    manifest.runtime.kind = RuntimeKind::NativeV1;
                else if (*type == "powerShellV1")
                    manifest.runtime.kind = RuntimeKind::PowerShellV1;
                else
                    result.errors.emplace_back("runtime.type must be nativeV1 or powerShellV1");
            }
            if (const auto entrypoint = _RequiredString(runtime, "entrypoint", 1024, result.errors))
            {
                if (const auto entrypointWide = _Utf8ToWide(*entrypoint))
                {
                    manifest.runtime.entrypoint = std::filesystem::path{ *entrypointWide };
                }
                else
                {
                    result.errors.emplace_back("runtime.entrypoint must be valid UTF-8");
                }
                if (!manifest.runtime.entrypoint.empty() &&
                    !IsSafeRelativeProviderPath(manifest.runtime.entrypoint))
                {
                    result.errors.emplace_back("runtime.entrypoint must be a safe path relative to the extension root");
                }
            }
            const auto& arguments = runtime["arguments"];
            if (!arguments.isNull() && !arguments.isArray())
            {
                result.errors.emplace_back("runtime.arguments must be an array");
            }
            else if (arguments.size() > 64)
            {
                result.errors.emplace_back("runtime.arguments exceeds 64 entries");
            }
            else
            {
                for (const auto& argument : arguments)
                {
                    if (!argument.isString() || argument.asString().size() > 4096 ||
                        argument.asString().find('\0') != std::string::npos)
                    {
                        result.errors.emplace_back("runtime.arguments entries must be bounded strings without NUL");
                        break;
                    }
                    const auto value = argument.asString();
                    if (const auto argumentWide = _Utf8ToWide(value))
                    {
                        manifest.runtime.arguments.emplace_back(*argumentWide);
                    }
                    else
                    {
                        result.errors.emplace_back("runtime.arguments entries must be valid UTF-8");
                        break;
                    }
                }
            }
        }

        const auto& activationEvents = root["activationEvents"];
        std::set<ActivationEvent> uniqueActivations;
        if (!activationEvents.isArray() || activationEvents.empty())
        {
            result.errors.emplace_back("activationEvents must be a non-empty array");
        }
        else
        {
            for (const auto& event : activationEvents)
            {
                if (!event.isString())
                {
                    result.errors.emplace_back("activationEvents entries must be strings");
                    continue;
                }
                const auto parsed = _ActivationFromString(event.asString());
                if (!parsed)
                {
                    result.errors.emplace_back("activationEvents contains an unsupported event");
                }
                else if (!uniqueActivations.emplace(*parsed).second)
                {
                    result.errors.emplace_back("activationEvents contains a duplicate event");
                }
                else
                {
                    manifest.activationEvents.emplace_back(*parsed);
                }
            }
        }

        const auto& fields = root["fields"];
        std::set<std::string> uniqueFields;
        if (!fields.isArray() || fields.empty() || fields.size() > MaximumFieldCount)
        {
            result.errors.emplace_back("fields must contain between 1 and 64 entries");
        }
        else
        {
            for (const auto& field : fields)
            {
                if (!field.isObject())
                {
                    result.errors.emplace_back("fields entries must be objects");
                    continue;
                }
                _RejectUnknownMembers(field, { "id", "displayName", "type", "defaultVisible" }, "field", result.errors);
                FieldDeclaration declaration;
                if (const auto id = _RequiredString(field, "id", 128, result.errors))
                {
                    declaration.id = *id;
                    if (!IsCanonicalFieldId(declaration.id))
                        result.errors.emplace_back("field id must be a canonical lowercase identifier");
                    else if (!uniqueFields.emplace(declaration.id).second)
                        result.errors.emplace_back("fields contains a duplicate id");
                }
                if (const auto name = _RequiredString(field, "displayName", 128, result.errors))
                    declaration.displayName = *name;
                if (const auto type = _RequiredString(field, "type", 32, result.errors))
                {
                    if (const auto parsed = _FieldTypeFromString(*type))
                        declaration.type = *parsed;
                    else
                        result.errors.emplace_back("field type is unsupported");
                }
                if (!field["defaultVisible"].isNull())
                {
                    if (!field["defaultVisible"].isBool())
                        result.errors.emplace_back("field defaultVisible must be a boolean");
                    else
                        declaration.defaultVisible = field["defaultVisible"].asBool();
                }
                manifest.fields.emplace_back(std::move(declaration));
            }
        }

        if (result.errors.empty())
        {
            result.value = std::move(manifest);
        }
        return result;
    }

    ParseResult<Snapshot> ParseSnapshot(
        const std::string_view json,
        const Manifest& manifest,
        const std::string_view expectedRequestId)
    {
        ParseResult<Snapshot> result;
        Json::Value root;
        std::string parseError;
        if (!_ReadJson(json, MaximumResponseSize, root, parseError))
        {
            result.errors.emplace_back("Invalid provider response JSON: " + parseError);
            return result;
        }
        if (!root.isObject())
        {
            result.errors.emplace_back("response must be an object");
            return result;
        }

        _RejectUnknownMembers(root, { "protocolVersion", "requestId", "result" }, "response", result.errors);
        if (!root["protocolVersion"].isUInt() ||
            root["protocolVersion"].asUInt() != CurrentProtocolVersion)
        {
            result.errors.emplace_back("protocolVersion must be 1");
        }

        Snapshot snapshot;
        if (const auto requestId = _RequiredString(root, "requestId", 128, result.errors))
        {
            snapshot.requestId = *requestId;
            if (snapshot.requestId != expectedRequestId)
            {
                result.errors.emplace_back("requestId does not match the active request");
            }
        }

        const auto& body = root["result"];
        if (!body.isObject())
        {
            result.errors.emplace_back("result must be an object");
            return result;
        }
        _RejectUnknownMembers(body, { "fields", "tooltip", "accessibilityText" }, "result", result.errors);

        std::unordered_map<std::string, FieldType> declarations;
        for (const auto& field : manifest.fields)
        {
            declarations.emplace(field.id, field.type);
        }

        const auto& fields = body["fields"];
        if (!fields.isObject() || fields.size() > MaximumFieldCount)
        {
            result.errors.emplace_back("result.fields must be an object with no more than 64 entries");
        }
        else
        {
            for (const auto& id : fields.getMemberNames())
            {
                const auto declaration = declarations.find(id);
                if (declaration == declarations.end())
                {
                    result.errors.emplace_back("result.fields contains undeclared field '" + id + "'");
                    continue;
                }
                const auto& value = fields[id];
                switch (declaration->second)
                {
                case FieldType::String:
                {
                    if (!value.isString() || value.asString().size() > MaximumFieldValueSize ||
                        value.asString().find('\0') != std::string::npos ||
                        !_IsValidUtf8(value.asString()))
                        result.errors.emplace_back("field '" + id + "' must be a bounded string");
                    else
                        snapshot.fields.emplace(id, value.asString());
                    break;
                }
                case FieldType::Boolean:
                    if (!value.isBool())
                        result.errors.emplace_back("field '" + id + "' must be a boolean");
                    else
                        snapshot.fields.emplace(id, value.asBool());
                    break;
                case FieldType::Integer:
                    if (!value.isInt64() && !value.isUInt64())
                        result.errors.emplace_back("field '" + id + "' must be an integer");
                    else if (value.isUInt64() && value.asUInt64() > static_cast<Json::UInt64>((std::numeric_limits<int64_t>::max)()))
                        result.errors.emplace_back("field '" + id + "' exceeds the signed integer range");
                    else
                        snapshot.fields.emplace(id, value.asInt64());
                    break;
                case FieldType::Number:
                    if (!value.isNumeric())
                        result.errors.emplace_back("field '" + id + "' must be numeric");
                    else
                        snapshot.fields.emplace(id, value.asDouble());
                    break;
                }
            }
        }

        const auto readOptionalText = [&](const char* name, std::optional<std::string>& destination) {
            const auto& value = body[name];
            if (!value.isNull())
            {
                if (!value.isString() ||
                    value.asString().size() > MaximumPresentationTextSize ||
                    value.asString().find('\0') != std::string::npos ||
                    !_IsValidUtf8(value.asString()))
                {
                    result.errors.emplace_back(std::string{ name } + " must be a bounded string");
                }
                else
                {
                    destination = value.asString();
                }
            }
        };
        readOptionalText("tooltip", snapshot.tooltip);
        readOptionalText("accessibilityText", snapshot.accessibilityText);

        if (result.errors.empty())
        {
            result.value = std::move(snapshot);
        }
        return result;
    }

    ParseResult<std::string> SerializeRequest(
        const Request& request,
        const Manifest& manifest)
    {
        ParseResult<std::string> result;
        if (request.requestId.empty() || request.requestId.size() > 128 || !_IsValidUtf8(request.requestId))
            result.errors.emplace_back("requestId must be a bounded UTF-8 string");
        if (request.providerId != manifest.id)
            result.errors.emplace_back("providerId does not match the manifest");
        if (request.processEpoch == 0)
            result.errors.emplace_back("processEpoch must be nonzero");
        if (request.sessionId.empty() || request.sessionId.size() > 128 || !_IsValidUtf8(request.sessionId))
            result.errors.emplace_back("sessionId must be a bounded UTF-8 string");
        if (request.shellType && (request.shellType->size() > 64 || !_IsValidUtf8(*request.shellType)))
            result.errors.emplace_back("shellType must be bounded UTF-8");

        const auto reason = _ActivationToString(request.reason);
        if (reason.empty())
            result.errors.emplace_back("request reason is invalid");
        if (std::find(manifest.activationEvents.begin(), manifest.activationEvents.end(), request.reason) ==
            manifest.activationEvents.end())
        {
            result.errors.emplace_back("provider did not declare the request activation event");
        }

        std::optional<std::string> workingDirectory;
        if (!request.workingDirectory.empty())
        {
            workingDirectory = _WideToUtf8(request.workingDirectory.native());
            if (!workingDirectory)
                result.errors.emplace_back("workingDirectory is not valid Unicode");
        }

        if (!result.errors.empty())
        {
            return result;
        }

        Json::Value root;
        root["protocolVersion"] = CurrentProtocolVersion;
        root["requestId"] = request.requestId;
        root["providerId"] = request.providerId;
        root["method"] = "refresh";

        Json::Value params;
        params["processEpoch"] = Json::UInt64{ request.processEpoch };
        params["sessionId"] = request.sessionId;
        params["reason"] = std::string{ reason };
        params["contextRevision"] = Json::UInt64{ request.contextRevision };

        Json::Value cwd;
        if (workingDirectory)
            cwd["value"] = *workingDirectory;
        else
            cwd["value"] = Json::nullValue;
        cwd["authoritative"] = request.workingDirectoryAuthoritative;
        params["workingDirectory"] = std::move(cwd);

        Json::Value shell;
        if (request.shellType)
            shell["type"] = *request.shellType;
        if (request.exitCode)
            shell["exitCode"] = *request.exitCode;
        if (request.commandDurationMilliseconds)
            shell["commandDurationMilliseconds"] = Json::UInt64{ *request.commandDurationMilliseconds };
        params["shellContext"] = std::move(shell);
        root["params"] = std::move(params);

        Json::StreamWriterBuilder builder;
        builder["indentation"] = "";
        auto serialized = Json::writeString(builder, root);
        if (serialized.size() > MaximumRequestPayloadSize)
        {
            result.errors.emplace_back("serialized request exceeds the transport size limit");
            return result;
        }
        result.value = std::move(serialized);
        return result;
    }
}
