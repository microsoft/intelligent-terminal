// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <cstdint>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <unordered_map>
#include <variant>
#include <vector>

namespace Microsoft::Terminal::RichTab::Provider
{
    inline constexpr uint32_t MinimumManifestSchemaVersion{ 1 };
    inline constexpr uint32_t CurrentManifestSchemaVersion{ 2 };
    inline constexpr uint32_t MinimumProtocolVersion{ 1 };
    inline constexpr uint32_t CurrentProtocolVersion{ 2 };
    inline constexpr uint32_t CurrentControlProtocolVersion{ 1 };
    inline constexpr size_t MaximumManifestSize{ 64 * 1024 };
    inline constexpr size_t MaximumRequestPayloadSize{ 16 * 1024 };
    inline constexpr size_t MaximumControlFrameSize{ 32 * 1024 };
    inline constexpr size_t MaximumResponseSize{ 16 * 1024 };
    inline constexpr size_t MaximumFieldCount{ 64 };
    inline constexpr size_t MaximumFieldValueSize{ 1024 };
    inline constexpr size_t MaximumPresentationTextSize{ 4096 };

    enum class RuntimeKind
    {
        NativeV1,
        PowerShellV1,
    };

    enum class HostingKind
    {
        OneShot,
        Persistent,
    };

    enum class ControlMessageKind
    {
        Start,
        Refresh,
        Lease,
        Stop,
    };

    enum class ActivationEvent
    {
        PaneConnected,
        WorkingDirectoryChanged,
        CommandFinished,
        TabActivated,
        ManualRefresh,
    };

    enum class FieldType
    {
        String,
        Boolean,
        Integer,
        Number,
    };

    struct ProtocolRange
    {
        uint32_t minimum{ CurrentProtocolVersion };
        uint32_t maximum{ CurrentProtocolVersion };
    };

    struct CommandRuntime
    {
        RuntimeKind kind{ RuntimeKind::NativeV1 };
        std::filesystem::path entrypoint;
        std::vector<std::wstring> arguments;
    };

    struct Hosting
    {
        HostingKind kind{ HostingKind::OneShot };
        uint32_t controlProtocolVersion{ 0 };

        bool operator==(const Hosting&) const = default;
    };

    struct FieldDeclaration
    {
        std::string id;
        std::string displayName;
        FieldType type{ FieldType::String };
        bool defaultVisible{ true };
    };

    struct Manifest
    {
        uint32_t schemaVersion{ CurrentManifestSchemaVersion };
        std::string id;
        std::string displayName;
        std::string publisher;
        std::string version;
        ProtocolRange protocol;
        CommandRuntime runtime;
        Hosting hosting;
        std::vector<ActivationEvent> activationEvents;
        std::vector<FieldDeclaration> fields;
        std::filesystem::path extensionRoot;
    };

    using FieldValue = std::variant<std::string, bool, int64_t, double>;

    struct Snapshot
    {
        std::string requestId;
        std::unordered_map<std::string, FieldValue> fields;
        std::optional<std::string> tooltip;
        std::optional<std::string> accessibilityText;
    };

    struct Request
    {
        uint32_t protocolVersion{ MinimumProtocolVersion };
        std::string requestId;
        std::string providerId;
        uint64_t processEpoch{ 0 };
        std::string sessionId;
        ActivationEvent reason{ ActivationEvent::ManualRefresh };
        std::filesystem::path workingDirectory;
        bool workingDirectoryAuthoritative{ false };
        uint64_t contextRevision{ 0 };
        std::optional<std::string> shellType;
        std::optional<int32_t> exitCode;
        std::optional<uint64_t> commandDurationMilliseconds;
    };

    struct PublishGrant
    {
        std::string requestId;
        std::string lease;
        uint64_t expiresInMilliseconds{ 0 };
    };

    template<typename T>
    struct ParseResult
    {
        std::optional<T> value;
        std::vector<std::string> errors;

        explicit operator bool() const noexcept
        {
            return value.has_value();
        }
    };

    ParseResult<Manifest> ParseManifest(
        std::string_view json,
        const std::filesystem::path& extensionRoot);

    ParseResult<std::string> ReadManifestFile(
        const std::filesystem::path& path);

    ParseResult<Snapshot> ParseSnapshot(
        std::string_view json,
        const Manifest& manifest,
        std::string_view expectedRequestId);

    ParseResult<std::string> SerializeRequest(
        const Request& request,
        const Manifest& manifest);

    ParseResult<std::string> SerializeControlFrame(
        ControlMessageKind kind,
        const std::optional<Request>& request,
        const std::vector<PublishGrant>& grants,
        const Manifest& manifest);

    bool IsCanonicalProviderId(std::string_view id) noexcept;
    bool IsCanonicalFieldId(std::string_view id) noexcept;
    bool IsSafeRelativeProviderPath(const std::filesystem::path& path) noexcept;
}
