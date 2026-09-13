// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "DiagnosticsMetadata.h"
#include "TerminalProtocolClsid.h"
#include <windows.h>
#include <appmodel.h>
#include <wil/resource.h>
#include <filesystem>
#include <vector>

#pragma comment(lib, "version.lib")

namespace IntelligentTerminal::Diagnostics
{
    inline std::string UtcNow()
    {
        SYSTEMTIME time{};
        GetSystemTime(&time);
        char buffer[32]{};
        sprintf_s(buffer, "%04u-%02u-%02uT%02u:%02u:%02u.%03uZ",
                  time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond, time.wMilliseconds);
        return buffer;
    }

    inline std::filesystem::path ProcessPath(HANDLE process)
    {
        std::wstring path(32768, L'\0');
        DWORD length = static_cast<DWORD>(path.size());
        if (!QueryFullProcessImageNameW(process, 0, path.data(), &length))
            return {};
        path.resize(length);
        return path;
    }

    inline Json::Value ProductVersion(const std::filesystem::path& path)
    {
        DWORD ignored{};
        const auto size = GetFileVersionInfoSizeW(path.c_str(), &ignored);
        if (!size || size > 1024 * 1024)
            return {};
        std::vector<BYTE> data(size);
        if (!GetFileVersionInfoW(path.c_str(), 0, size, data.data()))
            return {};
        VS_FIXEDFILEINFO* info{};
        UINT length{};
        if (!VerQueryValueW(data.data(), L"\\", reinterpret_cast<void**>(&info), &length) ||
            length < sizeof(*info) || info->dwSignature != 0xfeef04bd)
            return {};
        return std::to_string(HIWORD(info->dwProductVersionMS)) + "." +
               std::to_string(LOWORD(info->dwProductVersionMS)) + "." +
               std::to_string(HIWORD(info->dwProductVersionLS)) + "." +
               std::to_string(LOWORD(info->dwProductVersionLS));
    }

    inline Json::Value ProcessIdentity(HANDLE process)
    {
        Json::Value identity;
        identity["pid"] = static_cast<Json::UInt>(GetProcessId(process));
        FILETIME created{}, exited{}, kernel{}, user{};
        if (GetProcessTimes(process, &created, &exited, &kernel, &user))
        {
            const auto ticks = (static_cast<uint64_t>(created.dwHighDateTime) << 32) | created.dwLowDateTime;
            identity["start_time_filetime"] = std::to_string(ticks);
        }
        UINT32 length{};
        const auto packageResult = GetPackageFullName(process, &length, nullptr);
        identity["package_status"] = packageResult == APPMODEL_ERROR_NO_PACKAGE ? "unpackaged" : "unavailable";
        if (packageResult == ERROR_INSUFFICIENT_BUFFER && length <= 257)
        {
            std::wstring name(length, L'\0');
            if (GetPackageFullName(process, &length, name.data()) == ERROR_SUCCESS)
            {
                name.resize(length - 1);
                const auto ascii = Ascii(name);
                if (IsPackageIdentity(ascii))
                {
                    identity["package_full_name"] = ascii;
                    identity["package_status"] = "packaged";
                    const auto begin = ascii.find('_') + 1;
                    identity["package_version"] = ascii.substr(begin, ascii.find('_', begin) - begin);
                }
            }
        }
        return ServerIdentity(identity);
    }

    inline Json::Value CurrentServerIdentity()
    {
        auto identity = ProcessIdentity(GetCurrentProcess());
        const auto version = ProductVersion(ProcessPath(GetCurrentProcess()));
        if (!version.isNull())
            identity["product_version"] = version;
        identity["com_clsid"] = "{" __CLSID_TerminalProtocolServer "}";
        return identity;
    }

    inline const char* BuildBrand() noexcept
    {
#if defined(WT_BRANDING_RELEASE)
        return "release";
#elif defined(WT_BRANDING_PREVIEW)
        return "preview";
#elif defined(WT_BRANDING_CANARY)
        return "canary";
#else
        return "development_or_unbranded";
#endif
    }

    inline Json::Value EnvironmentProvenance()
    {
        Json::Value value;
        value["scope"] = "collecting_process_only";
        wchar_t clsid[128]{};
        const auto length = GetEnvironmentVariableW(L"WT_COM_CLSID", clsid, ARRAYSIZE(clsid));
        value["wt_com_clsid_status"] = length ? "invalid" : "absent";
        GUID guid{};
        // Only canonical GUID syntax, never ProgIDs or registry lookups.
        if (length == 38 && clsid[0] == L'{' && clsid[37] == L'}' && SUCCEEDED(CLSIDFromString(clsid, &guid)))
        {
            wchar_t canonical[40]{};
            if (StringFromGUID2(guid, canonical, ARRAYSIZE(canonical)))
            {
                const std::wstring_view text{ canonical };
                value["wt_com_clsid"] = Ascii(text);
                value["wt_com_clsid_status"] = "available";
            }
        }
        SetLastError(ERROR_SUCCESS);
        const auto overrideLength = GetEnvironmentVariableW(L"WT_WTCLI_PATH", nullptr, 0);
        value["wt_wtcli_path_override_present"] = overrideLength != 0 || GetLastError() != ERROR_ENVVAR_NOT_FOUND;
        value["wt_wtcli_path_source"] = value["wt_wtcli_path_override_present"].asBool() ? "environment_override_present_not_resolved" : "default_resolution";
        value["wt_wtcli_path_value"] = "omitted";
        value["logging_filters"] = "startup_logs_only";
        return value;
    }
}
