// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "BuiltInProviderCatalog.h"

#include <windows.h>

#include <array>

namespace Microsoft::Terminal::RichTab::Provider
{
    namespace
    {
        struct BuiltInProvider
        {
            std::filesystem::path relativeRoot;
            std::string_view expectedId;
        };

        const std::array builtInProviders{
            BuiltInProvider{ LR"(RichTabProviders\GitStatus)", "com.microsoft.intelligent-terminal.git-status" },
        };

    }

    std::filesystem::path BuiltInProviderCatalog::PackageRoot()
    {
        std::wstring path(260, L'\0');
        for (;;)
        {
            const auto length = GetModuleFileNameW(nullptr, path.data(), static_cast<DWORD>(path.size()));
            if (length == 0)
            {
                return {};
            }
            if (length < path.size())
            {
                path.resize(length);
                return std::filesystem::path{ path }.parent_path();
            }
            if (path.size() >= 32768)
            {
                return {};
            }
            path.resize(path.size() * 2);
        }
    }

    RegistryResult<std::vector<Registration>> BuiltInProviderCatalog::Load(
        const std::filesystem::path& packageRoot)
    {
        RegistryResult<std::vector<Registration>> result;
        result.value.emplace();
        if (packageRoot.empty() || !packageRoot.is_absolute())
        {
            result.errors.emplace_back("Built-in provider package root must be absolute");
            return result;
        }

        const auto catalogRoot = packageRoot / L"RichTabProviders";
        std::error_code existsError;
        if (!std::filesystem::exists(catalogRoot, existsError))
        {
            if (existsError)
            {
                result.errors.emplace_back("Could not inspect the built-in provider catalog");
            }
            return result;
        }
        if (!std::filesystem::is_directory(catalogRoot, existsError) || existsError)
        {
            result.errors.emplace_back("Built-in provider catalog is not a directory");
            return result;
        }

        for (const auto& builtIn : builtInProviders)
        {
            const auto root = packageRoot / builtIn.relativeRoot;
            const auto contents = ReadManifestFile(root / L"provider.json");
            if (!contents)
            {
                result.errors.insert(result.errors.end(), contents.errors.begin(), contents.errors.end());
                continue;
            }

            auto manifest = ParseManifest(*contents.value, root);
            if (!manifest)
            {
                result.errors.insert(result.errors.end(), manifest.errors.begin(), manifest.errors.end());
                continue;
            }
            if (manifest.value->id != builtIn.expectedId)
            {
                result.errors.emplace_back("Built-in provider manifest id does not match the product catalog");
                continue;
            }

            Registration registration;
            registration.manifest = std::move(*manifest.value);
            registration.kind = RegistrationKind::Managed;
            registration.root = root;
            registration.enabled = true;
            registration.integrityValid = true;
            registration.sourceIdentity = BuiltInSourceIdentity(registration.manifest.id);
            result.value->emplace_back(std::move(registration));
        }
        return result;
    }
}
