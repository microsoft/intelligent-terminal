// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "WtaProcess.h"

#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <unordered_map>
#include <unordered_set>

#include <json/json.h>
#include <winrt/base.h>

namespace Microsoft::Terminal::AgentAvailability
{
    struct HostAgentAvailability
    {
        bool nativeCliFound{ false };
        bool launchReady{ false };
        bool requiresNpx{ false };
    };

    struct HostAgentSnapshot
    {
        bool npxFound{ false };
        std::unordered_map<std::wstring, HostAgentAvailability> availability;
    };

    inline std::optional<HostAgentSnapshot> ParseHostAgentSnapshot(const std::string_view payload)
    {
        Json::Value root;
        Json::CharReaderBuilder builder;
        const std::unique_ptr<Json::CharReader> reader{ builder.newCharReader() };
        std::string errors;
        if (!reader->parse(payload.data(), payload.data() + payload.size(), &root, &errors) ||
            !root.isObject() ||
            !root["availability"].isArray() ||
            !root["npx_found"].isBool())
        {
            return std::nullopt;
        }

        HostAgentSnapshot snapshot;
        snapshot.npxFound = root["npx_found"].asBool();
        for (const auto& agent : root["availability"])
        {
            const auto& id = agent["id"];
            const auto& nativeCliFound = agent["native_cli_found"];
            const auto& launchReady = agent["launch_ready"];
            const auto& requiresNpx = agent["requires_npx"];
            if (!id.isString() ||
                !nativeCliFound.isBool() ||
                !launchReady.isBool() ||
                !requiresNpx.isBool())
            {
                return std::nullopt;
            }

            const auto hstringId = winrt::to_hstring(id.asString());
            snapshot.availability.emplace(
                std::wstring{ hstringId.c_str(), hstringId.size() },
                HostAgentAvailability{
                    nativeCliFound.asBool(),
                    launchReady.asBool(),
                    requiresNpx.asBool(),
                });
        }
        return snapshot;
    }

    inline std::unordered_set<std::wstring> ParseHostAgentIds(const std::string_view payload)
    {
        Json::Value root;
        Json::CharReaderBuilder builder;
        const std::unique_ptr<Json::CharReader> reader{ builder.newCharReader() };
        std::string errors;
        if (!reader->parse(payload.data(), payload.data() + payload.size(), &root, &errors) ||
            !root.isObject() ||
            !root["agents"].isArray())
        {
            return {};
        }

        std::unordered_set<std::wstring> ids;
        for (const auto& agent : root["agents"])
        {
            const auto& id = agent["id"];
            if (id.isString())
            {
                const auto hstringId = winrt::to_hstring(id.asString());
                ids.emplace(hstringId.c_str(), hstringId.size());
            }
        }
        return ids;
    }

    inline std::optional<HostAgentSnapshot> ProbeHostAgentSnapshot()
    {
        const auto wtaPath = WtaProcess::ResolveWtaExePath();
        if (wtaPath.empty())
        {
            return std::nullopt;
        }

        const auto output = WtaProcess::RunWtaCaptureStdout(
            wtaPath,
            L"probe-host-agents",
            2'000);
        return ParseHostAgentSnapshot(output);
    }

    inline std::unordered_set<std::wstring> ProbeHostAgentIds()
    {
        const auto wtaPath = WtaProcess::ResolveWtaExePath();
        if (wtaPath.empty())
        {
            return {};
        }

        const auto output = WtaProcess::RunWtaCaptureStdout(
            wtaPath,
            L"probe-host-agents",
            2'000);
        return ParseHostAgentIds(output);
    }
}
