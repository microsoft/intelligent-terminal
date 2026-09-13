// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <json/json.h>
#include <algorithm>
#include <array>
#include <charconv>
#include <initializer_list>
#include <string>
#include <string_view>

namespace IntelligentTerminal::Diagnostics
{
    inline constexpr Json::ArrayIndex ErrorLimit = 32;

    inline std::string Ascii(const std::wstring_view value)
    {
        std::string result;
        result.reserve(value.size());
        for (const auto ch : value)
        {
            if (ch < 0 || ch > 127)
                return {};
            result.push_back(static_cast<char>(ch));
        }
        return result;
    }

    // stage/code are developer-owned constants, never exception text.
    inline void Error(Json::Value& report, const char* stage, const char* code)
    {
        auto& errors = report["collection_errors"];
        if (!errors.isArray())
            errors = Json::Value{ Json::arrayValue };
        report["collection_error_count"] = report.get("collection_error_count", 0).asUInt64() + 1;
        if (errors.size() < ErrorLimit)
        {
            Json::Value error;
            error["stage"] = stage;
            error["code"] = code;
            errors.append(std::move(error));
        }
    }

    inline std::string Enum(const std::wstring_view value, const std::initializer_list<std::wstring_view> allowed)
    {
        for (const auto candidate : allowed)
        {
            if (value == candidate)
                return Ascii(candidate);
        }
        return "unavailable";
    }

    inline std::string Provider(const std::wstring_view value)
    {
        if (value.starts_with(L"custom:"))
            return "custom";
        return Enum(value, { L"copilot", L"claude", L"codex", L"gemini", L"opencode" });
    }

    inline bool IsDigits(const std::string_view value, const size_t limit = 20)
    {
        return !value.empty() && value.size() <= limit &&
               std::all_of(value.begin(), value.end(), [](const char ch) { return ch >= '0' && ch <= '9'; });
    }

    inline bool IsVersion(const std::string_view value)
    {
        if (value.empty() || value.size() > 40)
            return false;
        size_t start = 0;
        size_t parts = 0;
        do
        {
            const auto end = value.find('.', start);
            if (!IsDigits(value.substr(start, end == value.npos ? end : end - start), 10))
                return false;
            ++parts;
            if (end == value.npos)
                return parts >= 2 && parts <= 4;
            start = end + 1;
        } while (start < value.size());
        return false;
    }

    inline bool IsPackageIdentity(const std::string_view value)
    {
        if (value.empty() || value.size() > 256 ||
            !std::all_of(value.begin(), value.end(), [](const unsigned char ch) {
                   return (ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z') ||
                          (ch >= '0' && ch <= '9') || ch == '.' || ch == '-' || ch == '_';
               }))
            return false;
        std::array<std::string_view, 5> parts;
        size_t start = 0;
        for (size_t i = 0; i < parts.size(); ++i)
        {
            const auto end = value.find('_', start);
            if ((i == parts.size() - 1) != (end == value.npos))
                return false;
            parts[i] = value.substr(start, end == value.npos ? end : end - start);
            start = end + 1;
        }
        return !parts[0].empty() && IsVersion(parts[1]) &&
               (parts[2] == "x64" || parts[2] == "x86" || parts[2] == "arm64" || parts[2] == "arm" || parts[2] == "neutral") &&
               parts[4].size() == 13;
    }

    inline bool IsGuid(const std::string_view value)
    {
        if (value.size() != 38 || value.front() != '{' || value.back() != '}')
            return false;
        for (size_t i = 1; i < 37; ++i)
        {
            const auto ch = value[i];
            if (i == 9 || i == 14 || i == 19 || i == 24)
            {
                if (ch != '-')
                    return false;
            }
            else if (!((ch >= '0' && ch <= '9') || (ch >= 'a' && ch <= 'f') || (ch >= 'A' && ch <= 'F')))
                return false;
        }
        return true;
    }

    // Optional, advisory metadata cannot change protocol negotiation. Rebuild
    // the object from typed allowlisted fields; never forward arbitrary JSON.
    inline Json::Value ServerIdentity(const Json::Value& input)
    {
        Json::Value result;
        result["status"] = input.isNull() ? "unavailable" : "invalid";
        if (!input.isObject() || !input["pid"].isUInt() || input["pid"].asUInt() == 0 ||
            !input["start_time_filetime"].isString() || !IsDigits(input["start_time_filetime"].asString()))
            return result;
        const auto start = input["start_time_filetime"].asString();
        uint64_t ticks{};
        if (std::from_chars(start.data(), start.data() + start.size(), ticks).ec != std::errc{} || ticks == 0)
            return result;

        result["status"] = "available";
        result["pid"] = input["pid"];
        result["start_time_filetime"] = input["start_time_filetime"];
        // Derived, not trusted server-supplied free text.
        result["instance_id"] = std::to_string(input["pid"].asUInt()) + "-" + input["start_time_filetime"].asString();
        if (input["package_full_name"].isString() && IsPackageIdentity(input["package_full_name"].asString()))
            result["package_full_name"] = input["package_full_name"];
        if (input["package_version"].isString() && IsVersion(input["package_version"].asString()))
            result["package_version"] = input["package_version"];
        if (input["product_version"].isString() && IsVersion(input["product_version"].asString()))
            result["product_version"] = input["product_version"];
        if (input["com_clsid"].isString() && IsGuid(input["com_clsid"].asString()))
            result["com_clsid"] = input["com_clsid"];
        if (input["package_status"] == "packaged" || input["package_status"] == "unpackaged" || input["package_status"] == "unavailable")
            result["package_status"] = input["package_status"];
        result["build_commit_status"] = "unavailable";
        return result;
    }
}
