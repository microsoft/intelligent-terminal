// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <filesystem>
#include <optional>
#include <string>
#include <string_view>

namespace Microsoft::Terminal::RichTab::Provider
{
    enum class ProviderSourceKind
    {
        BuiltIn,
        AppExtension,
        Development,
        LegacyManaged,
    };

    struct ProviderSourceIdentity
    {
        ProviderSourceKind kind{ ProviderSourceKind::LegacyManaged };
        std::string providerId;
        std::wstring extensionId;
        std::wstring packageFamilyName;
        std::wstring packageFullName;
        std::wstring publisherId;
        std::wstring packageVersion;
        std::filesystem::path developmentRoot;

        bool operator==(const ProviderSourceIdentity&) const = default;
    };

    enum class ProviderConsentRequirement
    {
        NotRequired,
        Required,
        SourceNotAllowed,
    };

    ProviderSourceIdentity BuiltInSourceIdentity(std::string providerId);
    ProviderSourceIdentity AppExtensionSourceIdentity(
        std::string providerId,
        std::wstring extensionId,
        std::wstring packageFamilyName,
        std::wstring packageFullName,
        std::wstring publisherId,
        std::wstring packageVersion);
    ProviderSourceIdentity DevelopmentSourceIdentity(
        std::string providerId,
        std::filesystem::path root);
    ProviderSourceIdentity LegacyManagedSourceIdentity(
        std::string providerId,
        std::filesystem::path root);

    bool IsReservedBuiltInProviderId(std::string_view id) noexcept;
    int ProviderSourcePrecedence(ProviderSourceKind source) noexcept;
    ProviderConsentRequirement ConsentRequirementFor(ProviderSourceKind source) noexcept;
    std::optional<std::string> ProviderConsentKey(const ProviderSourceIdentity& identity);
}
