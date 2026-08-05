// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "RepoAwarenessService.h"

#include <json/json.h>

namespace Microsoft::Terminal::RepoAwareness
{
    inline const char* RepoAvailabilityString(const RepoAvailability availability) noexcept
    {
        using enum RepoAvailability;
        switch (availability)
        {
        case ShellIntegrationRequired:
            return "shell_integration_required";
        case Loading:
            return "loading";
        case Ready:
            return "ready";
        case NotRepository:
            return "not_repository";
        case GitUnavailable:
            return "git_unavailable";
        case UnsupportedWorkingDirectory:
            return "unsupported_working_directory";
        default:
            return "error";
        }
    }

    inline Json::Value RepoSummaryToJson(const RepoSummary& summary)
    {
        Json::Value value;
        value["availability"] = RepoAvailabilityString(summary.availability);
        if (summary.branch)
            value["branch"] = *summary.branch;
        value["head"] = summary.headOid;
        if (summary.upstream)
            value["upstream"] = *summary.upstream;
        value["ahead"] = static_cast<Json::UInt64>(summary.ahead);
        value["behind"] = static_cast<Json::UInt64>(summary.behind);
        value["modified"] = static_cast<Json::UInt64>(summary.modifiedCount);
        value["staged"] = static_cast<Json::UInt64>(summary.stagedCount);
        value["untracked"] = static_cast<Json::UInt64>(summary.untrackedCount);
        value["conflicted"] = static_cast<Json::UInt64>(summary.conflictedCount);
        value["generation"] = static_cast<Json::UInt64>(summary.generation);
        value["detached"] = summary.detached;
        value["unborn"] = summary.unborn;
        value["stale"] = summary.stale;
        value["files_truncated"] = summary.filesTruncated;
        return value;
    }

    inline std::string SerializeRepoSummary(const RepoSummary& summary)
    {
        Json::StreamWriterBuilder writer;
        writer["indentation"] = "";
        return Json::writeString(writer, RepoSummaryToJson(summary));
    }

    inline std::string SerializeRepoStateChanged(const std::string_view sessionId, const RepoSummary& summary)
    {
        Json::Value event;
        event["type"] = "event";
        event["method"] = "repo_state_changed";
        Json::Value params;
        params["pane_id"] = std::string{ sessionId };
        params["repo"] = RepoSummaryToJson(summary);
        event["params"] = std::move(params);

        Json::StreamWriterBuilder writer;
        writer["indentation"] = "";
        return Json::writeString(writer, event);
    }
}
