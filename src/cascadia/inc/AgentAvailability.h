// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "WtaProcess.h"

#include <memory>
#include <string>
#include <string_view>
#include <unordered_set>

#include <json/json.h>
#include <winrt/base.h>

namespace Microsoft::Terminal::AgentAvailability
{
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
