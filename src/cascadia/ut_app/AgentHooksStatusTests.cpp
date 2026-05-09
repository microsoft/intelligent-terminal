// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// AgentHooksStatusTests.cpp
//
// Pure-function tests for the wta-hooks-status JSON parser/formatter
// at src/cascadia/inc/AgentHooksStatus.h. The Settings UI shells out to
// `wta hooks status --json` and feeds the response into ParseStatusJson;
// these tests pin the wire contract from the consumer side so any
// breaking change on the wta side surfaces here rather than as silent
// UI mis-render.
//
// No subprocess, no winrt, no XAML — just JSON in, structs/strings out.

#include "precomp.h"

#include "../inc/AgentHooksStatus.h"

using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;
using namespace Microsoft::Terminal::AgentHooks;

namespace TerminalAppUnitTests
{
    class AgentHooksStatusTests
    {
        TEST_CLASS(AgentHooksStatusTests);

        TEST_METHOD(ParsesHappyPath);
        TEST_METHOD(RejectsUnsupportedSchemaVersion);
        TEST_METHOD(RejectsMalformedJson);
        TEST_METHOD(RejectsEmptyInput);
        TEST_METHOD(RejectsMissingClis);
        TEST_METHOD(RejectsMissingBundleSource);
        TEST_METHOD(ParsesEmptyClisArray);
        TEST_METHOD(ParsesDetectionFallback);
        TEST_METHOD(ParsesBundleSourceNone);
        TEST_METHOD(IgnoresUnknownExtraFields);

        TEST_METHOD(FormatsCliNotOnPath);
        TEST_METHOD(FormatsHooksInstalled);
        TEST_METHOD(FormatsHooksNotInstalled);
        TEST_METHOD(FormatsPartialMarketplaceOnly);
        TEST_METHOD(FormatsPartialPluginOnly);
        TEST_METHOD(FormatsPartialDisabled);
        TEST_METHOD(FormatsMarketplacePathStale);
        TEST_METHOD(FormatsFilesystemFallbackSuffix);
        TEST_METHOD(NotOnPathStillEmitsFallbackSuffix);

        TEST_METHOD(AnyBinaryOnPathTrueWhenAny);
        TEST_METHOD(AnyBinaryOnPathFalseWhenNone);
        TEST_METHOD(FindCliReturnsNullptrForMissing);
    };

    static constexpr std::string_view kHappyPathJson = R"({
        "schema_version": 3,
        "clis": [
            { "name": "copilot", "binary_on_path": true,  "binary_path": "C:\\copilot.cmd",
              "marketplace_registered": true, "marketplace_path": "C:\\repo\\hooks\\copilot",
              "marketplace_path_valid": true,
              "plugin_installed": true, "plugin_enabled": true },
            { "name": "claude",  "binary_on_path": true,  "binary_path": "C:\\claude.exe",
              "marketplace_registered": true, "marketplace_path": "C:\\repo\\hooks\\claude",
              "marketplace_path_valid": true,
              "plugin_installed": true, "plugin_enabled": true },
            { "name": "gemini",  "binary_on_path": false,
              "marketplace_registered": false, "marketplace_path_valid": false,
              "plugin_installed": false, "plugin_enabled": false }
        ],
        "bundle_source": { "kind": "exe-sibling", "path": "C:\\Program Files\\WT\\wt-agent-hooks" }
    })";

    void AgentHooksStatusTests::ParsesHappyPath()
    {
        const auto report = ParseStatusJson(kHappyPathJson);
        VERIFY_IS_TRUE(report.has_value());
        VERIFY_ARE_EQUAL(3u, report->schemaVersion);
        VERIFY_ARE_EQUAL(size_t{ 3 }, report->clis.size());

        const auto* copilot = FindCli(*report, "copilot");
        VERIFY_IS_NOT_NULL(copilot);
        VERIFY_IS_TRUE(copilot->binaryOnPath);
        VERIFY_IS_TRUE(copilot->binaryPath.has_value());
        VERIFY_ARE_EQUAL(std::string{ "C:\\copilot.cmd" }, *copilot->binaryPath);
        VERIFY_IS_TRUE(copilot->marketplaceRegistered);
        VERIFY_IS_TRUE(copilot->marketplacePath.has_value());
        VERIFY_ARE_EQUAL(std::string{ "C:\\repo\\hooks\\copilot" }, *copilot->marketplacePath);
        VERIFY_IS_TRUE(copilot->marketplacePathValid);
        VERIFY_IS_TRUE(copilot->pluginInstalled);
        VERIFY_IS_TRUE(copilot->pluginEnabled);
        VERIFY_IS_FALSE(copilot->detectionFallback.has_value());

        const auto* gemini = FindCli(*report, "gemini");
        VERIFY_IS_NOT_NULL(gemini);
        VERIFY_IS_FALSE(gemini->binaryOnPath);
        VERIFY_IS_FALSE(gemini->binaryPath.has_value());
        VERIFY_IS_FALSE(gemini->marketplacePath.has_value());
        VERIFY_IS_FALSE(gemini->marketplacePathValid);

        VERIFY_ARE_EQUAL(std::string{ "exe-sibling" }, report->bundleKind);
        VERIFY_IS_TRUE(report->bundlePath.has_value());
    }

    void AgentHooksStatusTests::RejectsUnsupportedSchemaVersion()
    {
        constexpr std::string_view js = R"({
            "schema_version": 99,
            "clis": [],
            "bundle_source": { "kind": "none" }
        })";
        VERIFY_IS_FALSE(ParseStatusJson(js).has_value());
    }

    void AgentHooksStatusTests::RejectsMalformedJson()
    {
        VERIFY_IS_FALSE(ParseStatusJson("{not json").has_value());
        VERIFY_IS_FALSE(ParseStatusJson("[1,2,3]").has_value()); // not an object
        VERIFY_IS_FALSE(ParseStatusJson("\"a string\"").has_value());
    }

    void AgentHooksStatusTests::RejectsEmptyInput()
    {
        VERIFY_IS_FALSE(ParseStatusJson("").has_value());
    }

    void AgentHooksStatusTests::RejectsMissingClis()
    {
        constexpr std::string_view js = R"({
            "schema_version": 3,
            "bundle_source": { "kind": "none" }
        })";
        VERIFY_IS_FALSE(ParseStatusJson(js).has_value());
    }

    void AgentHooksStatusTests::RejectsMissingBundleSource()
    {
        constexpr std::string_view js = R"({
            "schema_version": 3,
            "clis": []
        })";
        VERIFY_IS_FALSE(ParseStatusJson(js).has_value());
    }

    void AgentHooksStatusTests::ParsesEmptyClisArray()
    {
        constexpr std::string_view js = R"({
            "schema_version": 3,
            "clis": [],
            "bundle_source": { "kind": "none" }
        })";
        const auto r = ParseStatusJson(js);
        VERIFY_IS_TRUE(r.has_value());
        VERIFY_ARE_EQUAL(size_t{ 0 }, r->clis.size());
        VERIFY_ARE_EQUAL(std::string{ "none" }, r->bundleKind);
        VERIFY_IS_FALSE(r->bundlePath.has_value());
        VERIFY_IS_FALSE(AnyBinaryOnPath(*r));
    }

    void AgentHooksStatusTests::ParsesDetectionFallback()
    {
        constexpr std::string_view js = R"({
            "schema_version": 3,
            "clis": [
                { "name": "copilot", "binary_on_path": true,
                  "marketplace_registered": true, "marketplace_path_valid": true,
                  "plugin_installed": true, "plugin_enabled": true,
                  "detection_fallback": "fs" }
            ],
            "bundle_source": { "kind": "dev-tree", "path": "X" }
        })";
        const auto r = ParseStatusJson(js);
        VERIFY_IS_TRUE(r.has_value());
        VERIFY_IS_TRUE(r->clis[0].detectionFallback.has_value());
        VERIFY_ARE_EQUAL(std::string{ "fs" }, *r->clis[0].detectionFallback);
    }

    void AgentHooksStatusTests::ParsesBundleSourceNone()
    {
        constexpr std::string_view js = R"({
            "schema_version": 3,
            "clis": [],
            "bundle_source": { "kind": "none" }
        })";
        const auto r = ParseStatusJson(js);
        VERIFY_IS_TRUE(r.has_value());
        VERIFY_ARE_EQUAL(std::string{ "none" }, r->bundleKind);
        VERIFY_IS_FALSE(r->bundlePath.has_value());
    }

    void AgentHooksStatusTests::IgnoresUnknownExtraFields()
    {
        // Forward compatibility: wta may add fields in a future minor
        // bump. We must not reject them as long as schema_version still
        // matches the supported major.
        constexpr std::string_view js = R"({
            "schema_version": 3,
            "future_field": "ignore me",
            "clis": [
                { "name": "copilot", "binary_on_path": true,
                  "marketplace_registered": true, "marketplace_path_valid": true,
                  "plugin_installed": true, "plugin_enabled": true,
                  "future_per_cli_field": 42 }
            ],
            "bundle_source": { "kind": "dev-tree", "path": "X", "future_bundle_field": true }
        })";
        const auto r = ParseStatusJson(js);
        VERIFY_IS_TRUE(r.has_value());
        VERIFY_ARE_EQUAL(size_t{ 1 }, r->clis.size());
    }

    // ── Formatter ────────────────────────────────────────────────────────

    void AgentHooksStatusTests::FormatsCliNotOnPath()
    {
        CliStatus c{};
        c.name = "copilot";
        c.binaryOnPath = false;
        // even with bogus plugin flags, "not on PATH" should win
        c.marketplaceRegistered = true;
        c.pluginInstalled = true;
        const auto s = FormatCliStatusLine(c, L"Copilot CLI");
        VERIFY_ARE_EQUAL(std::wstring{ L"Copilot CLI — CLI not on PATH" }, s);
    }

    void AgentHooksStatusTests::FormatsHooksInstalled()
    {
        CliStatus c{};
        c.binaryOnPath = true;
        c.marketplaceRegistered = true;
        c.marketplacePathValid = true;
        c.pluginInstalled = true;
        c.pluginEnabled = true;
        const auto s = FormatCliStatusLine(c, L"Claude Code");
        VERIFY_ARE_EQUAL(std::wstring{ L"Claude Code — hooks installed" }, s);
    }

    void AgentHooksStatusTests::FormatsHooksNotInstalled()
    {
        CliStatus c{};
        c.binaryOnPath = true;
        // all plugin flags false
        const auto s = FormatCliStatusLine(c, L"Gemini CLI");
        VERIFY_ARE_EQUAL(std::wstring{ L"Gemini CLI — hooks not installed" }, s);
    }

    void AgentHooksStatusTests::FormatsPartialMarketplaceOnly()
    {
        CliStatus c{};
        c.binaryOnPath = true;
        c.marketplaceRegistered = true;
        c.marketplacePathValid = true;
        // plugin not installed
        const auto s = FormatCliStatusLine(c, L"Copilot CLI");
        VERIFY_ARE_EQUAL(
            std::wstring{ L"Copilot CLI — partially installed (marketplace registered, plugin missing)" },
            s);
    }

    void AgentHooksStatusTests::FormatsPartialPluginOnly()
    {
        CliStatus c{};
        c.binaryOnPath = true;
        c.pluginInstalled = true;
        c.pluginEnabled = true;
        // marketplace not registered
        const auto s = FormatCliStatusLine(c, L"Copilot CLI");
        VERIFY_ARE_EQUAL(
            std::wstring{ L"Copilot CLI — partially installed (marketplace missing, plugin installed)" },
            s);
    }

    void AgentHooksStatusTests::FormatsPartialDisabled()
    {
        CliStatus c{};
        c.binaryOnPath = true;
        c.marketplaceRegistered = true;
        c.marketplacePathValid = true;
        c.pluginInstalled = true;
        // pluginEnabled stays false
        const auto s = FormatCliStatusLine(c, L"Claude Code");
        VERIFY_ARE_EQUAL(
            std::wstring{ L"Claude Code — partially installed (marketplace registered, plugin installed, plugin disabled)" },
            s);
    }

    void AgentHooksStatusTests::FormatsMarketplacePathStale()
    {
        // Schema v3 (#25): plugin reports installed and the marketplace
        // entry exists by name, but the registered local source path is
        // gone — the silently-broken state. We surface it inline rather
        // than mis-rendering as "hooks installed".
        CliStatus c{};
        c.binaryOnPath = true;
        c.marketplaceRegistered = true;
        c.marketplacePathValid = false;
        c.pluginInstalled = true;
        c.pluginEnabled = true;
        const auto s = FormatCliStatusLine(c, L"Copilot CLI");
        VERIFY_ARE_EQUAL(
            std::wstring{ L"Copilot CLI — partially installed (marketplace registered, plugin installed, marketplace path stale)" },
            s);
    }

    void AgentHooksStatusTests::FormatsFilesystemFallbackSuffix()
    {
        CliStatus c{};
        c.binaryOnPath = true;
        c.marketplaceRegistered = true;
        c.marketplacePathValid = true;
        c.pluginInstalled = true;
        c.pluginEnabled = true;
        c.detectionFallback = "fs";
        const auto s = FormatCliStatusLine(c, L"Copilot CLI");
        VERIFY_ARE_EQUAL(std::wstring{ L"Copilot CLI — hooks installed (filesystem fallback)" }, s);
    }

    void AgentHooksStatusTests::NotOnPathStillEmitsFallbackSuffix()
    {
        // wta's fs fallback runs precisely when the binary isn't on PATH
        // (it can't spawn the CLI to ask). The suffix is informative,
        // not contradictory.
        CliStatus c{};
        c.binaryOnPath = false;
        c.detectionFallback = "fs";
        const auto s = FormatCliStatusLine(c, L"Gemini CLI");
        VERIFY_ARE_EQUAL(std::wstring{ L"Gemini CLI — CLI not on PATH" }, s);
    }

    // ── Helpers ─────────────────────────────────────────────────────────

    void AgentHooksStatusTests::AnyBinaryOnPathTrueWhenAny()
    {
        const auto r = ParseStatusJson(kHappyPathJson);
        VERIFY_IS_TRUE(r.has_value());
        VERIFY_IS_TRUE(AnyBinaryOnPath(*r));
    }

    void AgentHooksStatusTests::AnyBinaryOnPathFalseWhenNone()
    {
        constexpr std::string_view js = R"({
            "schema_version": 3,
            "clis": [
                { "name": "copilot", "binary_on_path": false,
                  "marketplace_registered": false, "marketplace_path_valid": false,
                  "plugin_installed": false, "plugin_enabled": false },
                { "name": "claude", "binary_on_path": false,
                  "marketplace_registered": false, "marketplace_path_valid": false,
                  "plugin_installed": false, "plugin_enabled": false }
            ],
            "bundle_source": { "kind": "none" }
        })";
        const auto r = ParseStatusJson(js);
        VERIFY_IS_TRUE(r.has_value());
        VERIFY_IS_FALSE(AnyBinaryOnPath(*r));
    }

    void AgentHooksStatusTests::FindCliReturnsNullptrForMissing()
    {
        const auto r = ParseStatusJson(kHappyPathJson);
        VERIFY_IS_TRUE(r.has_value());
        VERIFY_IS_NULL(FindCli(*r, "nonexistent"));
    }
}
