// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "ProviderContracts.h"
#include "ProviderSourcePolicy.h"

#include <functional>
#include <future>
#include <memory>

namespace Microsoft::Terminal::RichTab::Provider
{
    inline constexpr std::wstring_view RichTabProviderAppExtensionName{
        L"com.microsoft.intelligent-terminal.rich-tab-provider"
    };

    struct AppExtensionCandidate
    {
        ProviderSourceIdentity identity;
        std::filesystem::path publicPath;
        bool packageHealthy{ false };
        std::vector<std::string> diagnostics;
    };

    enum class AppExtensionDiscoveryStatus
    {
        Discovered,
        InvalidPackageIdentity,
        PackageUnhealthy,
        PublicPathUnavailable,
        ManifestUnavailable,
        ManifestInvalid,
        DuplicateProviderId,
    };

    struct DiscoveredAppExtensionProvider
    {
        ProviderSourceIdentity identity;
        std::filesystem::path publicPath;
        AppExtensionDiscoveryStatus status{ AppExtensionDiscoveryStatus::ManifestInvalid };
        std::optional<Manifest> manifest;
        std::vector<std::string> diagnostics;
    };

    struct AppExtensionDiscoveryResult
    {
        std::vector<DiscoveredAppExtensionProvider> providers;
        std::vector<std::string> diagnostics;
    };

    class AppExtensionProviderCatalog
    {
    public:
        static std::future<AppExtensionDiscoveryResult> DiscoverAsync();
        static std::shared_ptr<void> WatchForChanges(
            std::function<void()> callback);
        static AppExtensionDiscoveryResult InspectCandidates(
            std::vector<AppExtensionCandidate> candidates);
    };
}
