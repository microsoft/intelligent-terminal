// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// AgentHooksStatus.h
//
// Pure parser + formatter for the JSON contract emitted by
// `wta hooks status --json` (see wta/src/agent_hooks_installer.rs ::
// StatusReport). Lives in src/cascadia/inc/ so the Settings UI
// (TerminalSettingsEditor::AIAgentsViewModel) and the unit tests in
// ut_app can both consume it without a project-reference cycle.
//
// No winrt, no Win32, no I/O — pure JsonCpp + STL. Subprocess spawning
// (the part that actually shells out to wta.exe) lives in the
// ViewModel; this header is the testable boundary in between.

#pragma once

#include <cstdint>
#include <optional>
#include <sstream>
#include <string>
#include <string_view>
#include <vector>

#include <json/json.h>

namespace Microsoft::Terminal::AgentHooks
{
    // Major schema version this code understands. The wta-side type is
    // pinned at 3 (see STATUS_SCHEMA_VERSION in agent_hooks_installer.rs);
    // mismatch produces a parse failure rather than silent mis-render.
    //
    // v3 added `marketplace_path` and `marketplace_path_valid` per CLI
    // (#25) — `marketplace_registered: true` no longer implies the
    // registered `source.path` actually exists on disk; consumers should
    // consult `marketplacePathValid` for that.
    inline constexpr uint32_t SupportedStatusSchemaVersion = 3;

    // One entry of `clis[]` from the JSON report.
    struct CliStatus
    {
        std::string name; // "copilot" | "claude" | "gemini"
        bool binaryOnPath{ false };
        std::optional<std::string> binaryPath;
        bool marketplaceRegistered{ false };
        // v3: registered local source path of the `wt-local` marketplace
        // entry. Absent for `github`-shaped sources or when the CLI's
        // source-of-truth file couldn't be read.
        std::optional<std::string> marketplacePath;
        // v3: true when the marketplace entry exists *and* its
        // registered local path is still resolvable. Drives the
        // "marketplace path stale" diagnostic when a previous install
        // pointed at a now-deleted bundle directory.
        bool marketplacePathValid{ false };
        bool pluginInstalled{ false };
        bool pluginEnabled{ false };
        std::optional<std::string> detectionFallback; // e.g. "fs"
    };

    // Top-level shape of `wta hooks status --json`.
    struct StatusReport
    {
        uint32_t schemaVersion{ 0 };
        std::vector<CliStatus> clis;
        std::string bundleKind; // "env" | "exe-sibling" | "dev-tree" | "none"
        std::optional<std::string> bundlePath;
    };

    // Returns nullopt when the JSON is malformed, missing required
    // fields, or carries an unsupported schema_version. Tolerates
    // unknown-but-well-formed extra fields (forward compatibility).
    inline std::optional<StatusReport> ParseStatusJson(std::string_view json)
    {
        if (json.empty())
        {
            return std::nullopt;
        }

        Json::Value root;
        {
            Json::CharReaderBuilder rb;
            const std::unique_ptr<Json::CharReader> reader{ rb.newCharReader() };
            std::string errs;
            if (!reader->parse(json.data(), json.data() + json.size(), &root, &errs))
            {
                return std::nullopt;
            }
        }
        if (!root.isObject())
        {
            return std::nullopt;
        }

        StatusReport out;

        // schema_version — required, must match.
        if (!root.isMember("schema_version") || !root["schema_version"].isUInt())
        {
            return std::nullopt;
        }
        out.schemaVersion = root["schema_version"].asUInt();
        if (out.schemaVersion != SupportedStatusSchemaVersion)
        {
            return std::nullopt;
        }

        // clis[] — required array, may be empty.
        if (!root.isMember("clis") || !root["clis"].isArray())
        {
            return std::nullopt;
        }
        const auto& clisJson = root["clis"];
        out.clis.reserve(clisJson.size());
        for (const auto& entry : clisJson)
        {
            if (!entry.isObject())
            {
                return std::nullopt;
            }
            CliStatus cli;

            if (!entry.isMember("name") || !entry["name"].isString())
            {
                return std::nullopt;
            }
            cli.name = entry["name"].asString();

            // Booleans default to false on absence — wta always emits
            // them, but be lenient so we don't fail whole report on a
            // single-CLI write hiccup.
            cli.binaryOnPath = entry.get("binary_on_path", false).asBool();
            cli.marketplaceRegistered = entry.get("marketplace_registered", false).asBool();
            cli.marketplacePathValid = entry.get("marketplace_path_valid", false).asBool();
            cli.pluginInstalled = entry.get("plugin_installed", false).asBool();
            cli.pluginEnabled = entry.get("plugin_enabled", false).asBool();

            if (entry.isMember("binary_path") && entry["binary_path"].isString())
            {
                cli.binaryPath = entry["binary_path"].asString();
            }
            if (entry.isMember("marketplace_path") && entry["marketplace_path"].isString())
            {
                cli.marketplacePath = entry["marketplace_path"].asString();
            }
            if (entry.isMember("detection_fallback") && entry["detection_fallback"].isString())
            {
                cli.detectionFallback = entry["detection_fallback"].asString();
            }

            out.clis.push_back(std::move(cli));
        }

        // bundle_source — required, but only `kind` is required inside.
        if (!root.isMember("bundle_source") || !root["bundle_source"].isObject())
        {
            return std::nullopt;
        }
        const auto& bundleJson = root["bundle_source"];
        if (!bundleJson.isMember("kind") || !bundleJson["kind"].isString())
        {
            return std::nullopt;
        }
        out.bundleKind = bundleJson["kind"].asString();
        if (bundleJson.isMember("path") && bundleJson["path"].isString())
        {
            out.bundlePath = bundleJson["path"].asString();
        }

        return out;
    }

    // Look up one CLI by name. Returns nullptr if absent.
    inline const CliStatus* FindCli(const StatusReport& report, std::string_view name)
    {
        for (const auto& cli : report.clis)
        {
            if (cli.name == name)
            {
                return &cli;
            }
        }
        return nullptr;
    }

    // True when at least one CLI binary is on PATH — drives the
    // "Install hooks" button enabled state.
    inline bool AnyBinaryOnPath(const StatusReport& report)
    {
        for (const auto& cli : report.clis)
        {
            if (cli.binaryOnPath)
            {
                return true;
            }
        }
        return false;
    }

    // Render one row as a localizable-later display string. Mirrors the
    // strings the previous fs-detection code produced, with three new
    // states added that the JSON contract makes visible:
    //   * "partially installed" — marketplace is registered but the
    //     plugin itself isn't (or vice-versa). The old fs check
    //     conflated this with "installed" or "not installed" depending
    //     on which sentinel file happened to exist.
    //   * "marketplace path stale" — schema v3 (#25): the CLI still
    //     remembers the `wt-local` marketplace by name, but the local
    //     `source.path` it was registered against no longer exists.
    //     A reinstall is required to repoint it.
    //   * "(filesystem fallback)" suffix when wta couldn't talk to the
    //     CLI and used fs heuristics.
    //
    // Examples:
    //   "Copilot CLI — CLI not on PATH"
    //   "Copilot CLI — hooks installed"
    //   "Copilot CLI — hooks not installed"
    //   "Claude Code — partially installed (marketplace registered, plugin missing)"
    //   "Claude Code — partially installed (marketplace registered, plugin installed, marketplace path stale)"
    //   "Gemini CLI — hooks installed (filesystem fallback)"
    inline std::wstring FormatCliStatusLine(const CliStatus& cli, std::wstring_view displayName)
    {
        std::wstring text{ displayName };
        text += L" — ";

        if (!cli.binaryOnPath)
        {
            text += L"CLI not on PATH";
            return text;
        }

        // v3: "fully installed" requires the marketplace path to still
        // be valid on disk. Mirrors `wta`'s own
        // format_hooks_status_human gating.
        const bool fully = cli.marketplaceRegistered && cli.marketplacePathValid &&
                           cli.pluginInstalled && cli.pluginEnabled;
        const bool none = !cli.marketplaceRegistered && !cli.pluginInstalled;

        if (fully)
        {
            text += L"hooks installed";
        }
        else if (none)
        {
            text += L"hooks not installed";
        }
        else
        {
            text += L"partially installed (";
            bool first = true;
            const auto append = [&](std::wstring_view tag) {
                if (!first)
                {
                    text += L", ";
                }
                text += tag;
                first = false;
            };
            append(cli.marketplaceRegistered ? L"marketplace registered" : L"marketplace missing");
            append(cli.pluginInstalled ? L"plugin installed" : L"plugin missing");
            if (cli.pluginInstalled && !cli.pluginEnabled)
            {
                append(L"plugin disabled");
            }
            // Only useful to surface when the marketplace claims to be
            // registered — otherwise "marketplace missing" already
            // covers it and adding "marketplace path stale" would be
            // redundant noise.
            if (cli.marketplaceRegistered && !cli.marketplacePathValid)
            {
                append(L"marketplace path stale");
            }
            text += L")";
        }

        if (cli.detectionFallback.has_value())
        {
            text += L" (filesystem fallback)";
        }
        return text;
    }
}
