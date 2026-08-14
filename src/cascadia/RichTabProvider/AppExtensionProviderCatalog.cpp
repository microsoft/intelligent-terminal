// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "AppExtensionProviderCatalog.h"

#include <winrt/Windows.ApplicationModel.AppExtensions.h>
#include <winrt/Windows.Foundation.Collections.h>
#include <winrt/Windows.Storage.h>

#include <unordered_map>

namespace Microsoft::Terminal::RichTab::Provider
{
    namespace
    {
        struct AppExtensionCatalogWatcher
        {
            winrt::Windows::ApplicationModel::AppExtensions::AppExtensionCatalog catalog{ nullptr };
            winrt::event_token installed;
            winrt::event_token updating;
            winrt::event_token updated;
            winrt::event_token uninstalling;
            winrt::event_token statusChanged;

            ~AppExtensionCatalogWatcher()
            {
                if (catalog)
                {
                    catalog.PackageInstalled(installed);
                    catalog.PackageUpdating(updating);
                    catalog.PackageUpdated(updated);
                    catalog.PackageUninstalling(uninstalling);
                    catalog.PackageStatusChanged(statusChanged);
                }
            }
        };

        bool _HasIdentity(const ProviderSourceIdentity& identity) noexcept
        {
            return identity.kind == ProviderSourceKind::AppExtension &&
                   !identity.extensionId.empty() &&
                   !identity.packageFamilyName.empty() &&
                   !identity.packageFullName.empty() &&
                   !identity.publisherId.empty() &&
                   !identity.packageVersion.empty();
        }

        std::wstring _VersionString(const winrt::Windows::ApplicationModel::PackageVersion& version)
        {
            return std::to_wstring(version.Major) + L"." +
                   std::to_wstring(version.Minor) + L"." +
                   std::to_wstring(version.Build) + L"." +
                   std::to_wstring(version.Revision);
        }

        DiscoveredAppExtensionProvider _InspectCandidate(AppExtensionCandidate candidate)
        {
            DiscoveredAppExtensionProvider provider{
                std::move(candidate.identity),
                std::move(candidate.publicPath),
            };
            provider.diagnostics = std::move(candidate.diagnostics);
            if (!_HasIdentity(provider.identity))
            {
                provider.status = AppExtensionDiscoveryStatus::InvalidPackageIdentity;
                provider.diagnostics.emplace_back("App Extension package identity is incomplete");
                return provider;
            }
            if (!candidate.packageHealthy)
            {
                provider.status = AppExtensionDiscoveryStatus::PackageUnhealthy;
                provider.diagnostics.emplace_back("App Extension package status is not healthy");
                return provider;
            }

            std::error_code directoryError;
            if (provider.publicPath.empty() ||
                !provider.publicPath.is_absolute() ||
                !std::filesystem::is_directory(provider.publicPath, directoryError) ||
                directoryError)
            {
                provider.status = AppExtensionDiscoveryStatus::PublicPathUnavailable;
                provider.diagnostics.emplace_back("App Extension public path is unavailable");
                return provider;
            }

            const auto contents = ReadManifestFile(provider.publicPath / L"provider.json");
            if (!contents)
            {
                provider.status = AppExtensionDiscoveryStatus::ManifestUnavailable;
                provider.diagnostics = contents.errors;
                return provider;
            }

            auto manifest = ParseManifest(*contents.value, provider.publicPath);
            if (!manifest)
            {
                provider.status = AppExtensionDiscoveryStatus::ManifestInvalid;
                provider.diagnostics = std::move(manifest.errors);
                return provider;
            }

            provider.status = AppExtensionDiscoveryStatus::Discovered;
            provider.manifest = std::move(*manifest.value);
            provider.identity.providerId = provider.manifest->id;
            return provider;
        }
    }

    AppExtensionDiscoveryResult AppExtensionProviderCatalog::InspectCandidates(
        std::vector<AppExtensionCandidate> candidates)
    {
        AppExtensionDiscoveryResult result;
        result.providers.reserve(candidates.size());
        for (auto& candidate : candidates)
        {
            result.providers.emplace_back(_InspectCandidate(std::move(candidate)));
        }

        std::unordered_map<std::string, std::vector<size_t>> providerIds;
        for (size_t index = 0; index < result.providers.size(); ++index)
        {
            const auto& provider = result.providers[index];
            if (provider.status == AppExtensionDiscoveryStatus::Discovered && provider.manifest)
            {
                providerIds[provider.manifest->id].emplace_back(index);
            }
        }
        for (const auto& [id, indexes] : providerIds)
        {
            if (indexes.size() < 2)
            {
                continue;
            }
            for (const auto index : indexes)
            {
                auto& provider = result.providers[index];
                provider.status = AppExtensionDiscoveryStatus::DuplicateProviderId;
                provider.diagnostics.emplace_back(
                    "Multiple App Extensions declare provider id '" + id + "'");
            }
        }
        return result;
    }

    std::future<AppExtensionDiscoveryResult> AppExtensionProviderCatalog::DiscoverAsync()
    {
        return std::async(std::launch::async, []() {
            AppExtensionDiscoveryResult result;
            try
            {
                winrt::init_apartment(winrt::apartment_type::multi_threaded);
                namespace AppExtensions = winrt::Windows::ApplicationModel::AppExtensions;

                const auto catalog = AppExtensions::AppExtensionCatalog::Open(
                    RichTabProviderAppExtensionName);
                const auto extensions = catalog.FindAllAsync().get();
                std::vector<AppExtensionCandidate> candidates;
                candidates.reserve(extensions.Size());
                for (const auto& extension : extensions)
                {
                    AppExtensionCandidate candidate;
                    try
                    {
                        const auto package = extension.Package();
                        const auto packageId = package.Id();
                        candidate.identity = AppExtensionSourceIdentity(
                            {},
                            std::wstring{ extension.Id() },
                            std::wstring{ packageId.FamilyName() },
                            std::wstring{ packageId.FullName() },
                            std::wstring{ packageId.PublisherId() },
                            _VersionString(packageId.Version()));
                        candidate.packageHealthy = package.Status().VerifyIsOK();
                    }
                    catch (const winrt::hresult_error& error)
                    {
                        candidate.diagnostics.emplace_back(
                            "Could not read App Extension package identity: " +
                            winrt::to_string(error.message()));
                        candidates.emplace_back(std::move(candidate));
                        continue;
                    }

                    if (candidate.packageHealthy)
                    {
                        try
                        {
                            if (const auto extension3 = extension.try_as<AppExtensions::IAppExtension3>())
                            {
                                candidate.publicPath = std::filesystem::path{ extension3.GetPublicPath().c_str() };
                            }
                            else if (const auto folder = extension.GetPublicFolderAsync().get())
                            {
                                candidate.publicPath = std::filesystem::path{ folder.Path().c_str() };
                            }
                        }
                        catch (const winrt::hresult_error& error)
                        {
                            candidate.diagnostics.emplace_back(
                                "Could not read App Extension public path: " +
                                winrt::to_string(error.message()));
                        }
                    }
                    candidates.emplace_back(std::move(candidate));
                }
                result = InspectCandidates(std::move(candidates));
            }
            catch (const winrt::hresult_error& error)
            {
                result.diagnostics.emplace_back(
                    "Could not enumerate Rich Tab App Extensions: " +
                    winrt::to_string(error.message()));
            }
            catch (const std::exception& error)
            {
                result.diagnostics.emplace_back(
                    "Could not enumerate Rich Tab App Extensions: " +
                    std::string{ error.what() });
            }
            return result;
        });
    }

    std::shared_ptr<void> AppExtensionProviderCatalog::WatchForChanges(
        std::function<void()> callback)
    {
        try
        {
            namespace AppExtensions = winrt::Windows::ApplicationModel::AppExtensions;
            auto watcher = std::make_shared<AppExtensionCatalogWatcher>();
            watcher->catalog = AppExtensions::AppExtensionCatalog::Open(
                RichTabProviderAppExtensionName);
            const auto changed = [callback = std::move(callback)](const auto&, const auto&) {
                callback();
            };
            watcher->installed = watcher->catalog.PackageInstalled(changed);
            watcher->updating = watcher->catalog.PackageUpdating(changed);
            watcher->updated = watcher->catalog.PackageUpdated(changed);
            watcher->uninstalling = watcher->catalog.PackageUninstalling(changed);
            watcher->statusChanged = watcher->catalog.PackageStatusChanged(changed);
            return watcher;
        }
        catch (...)
        {
            return {};
        }
    }
}
