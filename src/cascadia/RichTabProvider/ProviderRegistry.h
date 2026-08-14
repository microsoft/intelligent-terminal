// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "ProviderContracts.h"
#include "ProviderSourcePolicy.h"

#include <filesystem>
#include <optional>
#include <string>
#include <vector>

namespace Microsoft::Terminal::RichTab::Provider
{
    enum class RegistrationKind
    {
        Managed,
        Development,
    };

    struct Registration
    {
        Manifest manifest;
        RegistrationKind kind{ RegistrationKind::Managed };
        std::filesystem::path root;
        std::string payloadHash;
        bool enabled{ false };
        bool integrityValid{ false };
        ProviderSourceIdentity sourceIdentity;
    };

    template<typename T>
    struct RegistryResult
    {
        std::optional<T> value;
        std::vector<std::string> errors;

        explicit operator bool() const noexcept
        {
            return value.has_value();
        }
    };

    class ProviderRegistry
    {
    public:
        static constexpr size_t MaximumPayloadFileCount{ 256 };
        static constexpr uint64_t MaximumPayloadSize{ 32ull * 1024ull * 1024ull };

        explicit ProviderRegistry(std::filesystem::path root = {});

        const std::filesystem::path& Root() const noexcept;
        static std::filesystem::path DefaultRoot();

        RegistryResult<Registration> Install(const std::filesystem::path& manifestPath);
        RegistryResult<Registration> RegisterDevelopment(const std::filesystem::path& manifestPath);
        RegistryResult<std::vector<Registration>> List();
        RegistryResult<Registration> SetEnabled(std::string_view id, bool enabled);
        RegistryResult<bool> AppExtensionConsentEnabled(
            const ProviderSourceIdentity& identity);
        RegistryResult<bool> SetAppExtensionConsentEnabled(
            const ProviderSourceIdentity& identity,
            bool enabled);
        RegistryResult<bool> Remove(std::string_view id);

    private:
        struct StoredRegistration
        {
            std::string id;
            RegistrationKind kind{ RegistrationKind::Managed };
            std::filesystem::path root;
            std::string payloadHash;
            std::optional<std::string> consentKey;
            bool enabled{ false };
        };

        std::filesystem::path _root;

        RegistryResult<Registration> _Register(
            const std::filesystem::path& manifestPath,
            RegistrationKind kind);
        RegistryResult<StoredRegistration> _LoadStored(std::string_view id);
        RegistryResult<Registration> _Materialize(const StoredRegistration& stored);
        RegistryResult<std::vector<StoredRegistration>> _LoadAllStored();
        bool _WriteStored(const StoredRegistration& stored, std::string& error);
        bool _DeleteStored(std::string_view id, std::string& error);
        bool _RecoverManaged(const StoredRegistration& stored, std::string& error);
        bool _RecoverRemovals(std::string& error);
    };
}
