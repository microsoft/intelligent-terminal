// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <Windows.h>
#include <wil/resource.h>
#include <string>

#include "WslDistroName.h"

namespace Microsoft::Terminal
{
    inline bool IsRegisteredWslDistro(const std::string_view name)
    {
        if (!WslDistroName::IsSafe(name))
        {
            return false;
        }
        wil::unique_hkey root;
        if (RegOpenKeyExW(HKEY_CURRENT_USER, L"Software\\Microsoft\\Windows\\CurrentVersion\\Lxss", 0, KEY_READ, root.put()) != ERROR_SUCCESS)
        {
            return false;
        }
        const std::wstring expected{ name.begin(), name.end() };
        for (DWORD index = 0;; ++index)
        {
            wchar_t keyName[256]{};
            DWORD keyLength = ARRAYSIZE(keyName);
            const auto result = RegEnumKeyExW(root.get(), index, keyName, &keyLength, nullptr, nullptr, nullptr, nullptr);
            if (result == ERROR_NO_MORE_ITEMS)
            {
                return false;
            }
            if (result != ERROR_SUCCESS)
            {
                LOG_WIN32(result);
                return false;
            }
            wchar_t registeredName[257]{};
            DWORD bytes = sizeof(registeredName);
            if (RegGetValueW(root.get(), keyName, L"DistributionName", RRF_RT_REG_SZ, nullptr, registeredName, &bytes) == ERROR_SUCCESS &&
                CompareStringOrdinal(registeredName, -1, expected.c_str(), -1, TRUE) == CSTR_EQUAL)
            {
                return true;
            }
        }
    }
}
