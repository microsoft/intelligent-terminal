// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "ProviderContracts.h"

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

    enum class RegistrationOrigin
    {
        Unknown,
        BuiltInCatalog,
    };

    struct Registration
    {
        Manifest manifest;
        RegistrationKind kind{ RegistrationKind::Managed };
        RegistrationOrigin origin{ RegistrationOrigin::Unknown };
        std::filesystem::path root;
        std::string payloadHash;
        bool enabled{ false };
        bool integrityValid{ false };
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

}
