// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "ProviderRegistry.h"

namespace Microsoft::Terminal::RichTab::Provider
{
    class BuiltInProviderCatalog
    {
    public:
        static std::filesystem::path PackageRoot();
        static std::filesystem::path GitStatusRoot(const std::filesystem::path& packageRoot);
        static RegistryResult<std::vector<Registration>> Load(const std::filesystem::path& packageRoot);
    };
}
