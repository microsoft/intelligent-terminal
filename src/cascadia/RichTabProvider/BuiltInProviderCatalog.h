// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "ProviderRegistry.h"

namespace Microsoft::Terminal::RichTab::Provider
{
    enum class ProviderCatalogOutcome : uint8_t
    {
        Unknown,
        InvalidPackageRoot,
        CatalogAbsent,
        CatalogUnreadable,
        ManifestUnreadable,
        ManifestInvalid,
        ProviderIdMismatch,
        ProviderRegistered,
    };

    struct BuiltInProviderCatalogResult
    {
        std::optional<std::vector<Registration>> value;
        std::vector<std::string> errors;
        ProviderCatalogOutcome outcome{ ProviderCatalogOutcome::Unknown };

        explicit operator bool() const noexcept
        {
            return value.has_value();
        }
    };

    class BuiltInProviderCatalog
    {
    public:
        static std::filesystem::path PackageRoot();
        static BuiltInProviderCatalogResult Load(const std::filesystem::path& packageRoot);
    };
}
