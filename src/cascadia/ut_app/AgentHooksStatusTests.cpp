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
        TEST_METHOD(ParsesVersions);
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
        TEST_METHOD(HidesDetectedCliWithoutHooks);
        TEST_METHOD(ShowsHookStateWithoutCli);
        TEST_METHOD(HidesAbsentCliWithoutHooks);

        TEST_METHOD(ParsesInstallReport);
        TEST_METHOD(NamesEveryFailedInstallCli);
        TEST_METHOD(FormatsNoFailuresAsEmpty);
        TEST_METHOD(TreatsUnknownInstallOutcomeAsNonFailure);
        TEST_METHOD(FallsBackToRawNameForUnknownCli);
        TEST_METHOD(RejectsUnsupportedInstallSchemaVersion);
        TEST_METHOD(RejectsMalformedInstallReport);
    };

    static constexpr std::string_view kHappyPathJson = R"({
        "schema_version": 4,
        "clis": [
            { "name": "copilot", "binary_on_path": true,  "binary_path": "C:\\copilot.cmd",
              "marketplace_registered": true, "marketplace_path": "C:\\repo\\hooks\\copilot",
              "marketplace_path_valid": true,
              "plugin_installed": true, "plugin_enabled": true,
              "installed_version": "0.1.5", "bundle_version": "0.1.5" },
            { "name": "claude",  "binary_on_path": true,  "binary_path": "C:\\claude.exe",
              "marketplace_registered": true, "marketplace_path": "C:\\repo\\hooks\\claude",
              "marketplace_path_valid": true,
              "plugin_installed": true, "plugin_enabled": true,
              "installed_version": "0.1.4", "bundle_version": "0.1.5" },
            { "name": "gemini",  "binary_on_path": false,
              "marketplace_registered": false, "marketplace_path_valid": false,
              "plugin_installed": false, "plugin_enabled": false,
              "bundle_version": "0.1.5" },
            { "name": "opencode", "binary_on_path": true,
               "marketplace_registered": true, "marketplace_path": "C:\\Users\\test\\.config\\opencode\\plugins",
               "marketplace_path_valid": true,
               "plugin_installed": true, "plugin_enabled": true }
        ],
        "bundle_source": { "kind": "exe-sibling", "path": "C:\\Program Files\\WT\\wt-agent-hooks" }
    })";

    void AgentHooksStatusTests::ParsesHappyPath()
    {
        const auto report = ParseStatusJson(kHappyPathJson);
        VERIFY_IS_TRUE(report.has_value());
        VERIFY_ARE_EQUAL(4u, report->schemaVersion);
        VERIFY_ARE_EQUAL(size_t{ 4 }, report->clis.size());

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

        const auto* openCode = FindCli(*report, "opencode");
        VERIFY_IS_NOT_NULL(openCode);
        VERIFY_IS_TRUE(openCode->binaryOnPath);
        VERIFY_IS_TRUE(openCode->pluginInstalled);
        VERIFY_IS_TRUE(openCode->pluginEnabled);

        VERIFY_ARE_EQUAL(std::string{ "exe-sibling" }, report->bundleKind);
        VERIFY_IS_TRUE(report->bundlePath.has_value());
    }

    // A CLI that's merely installed on the machine gets no row: the row only
    // exists to carry the per-CLI Remove action, and there's nothing to
    // remove. Regression guard for the OpenCode row that used to linger with
    // a permanently-disabled Remove button after its hooks were uninstalled.
    void AgentHooksStatusTests::HidesDetectedCliWithoutHooks()
    {
        CliStatus openCode{};
        openCode.name = "opencode";
        openCode.binaryOnPath = true;

        VERIFY_IS_FALSE(HasHookState(&openCode));
    }

    void AgentHooksStatusTests::ShowsHookStateWithoutCli()
    {
        CliStatus openCode{};
        openCode.name = "opencode";
        openCode.pluginInstalled = true;

        VERIFY_IS_TRUE(HasHookState(&openCode));
    }

    void AgentHooksStatusTests::HidesAbsentCliWithoutHooks()
    {
        CliStatus openCode{};
        openCode.name = "opencode";

        VERIFY_IS_FALSE(HasHookState(&openCode));
        VERIFY_IS_FALSE(HasHookState(nullptr));
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
            "schema_version": 4,
            "bundle_source": { "kind": "none" }
        })";
        VERIFY_IS_FALSE(ParseStatusJson(js).has_value());
    }

    void AgentHooksStatusTests::RejectsMissingBundleSource()
    {
        constexpr std::string_view js = R"({
            "schema_version": 4,
            "clis": []
        })";
        VERIFY_IS_FALSE(ParseStatusJson(js).has_value());
    }

    void AgentHooksStatusTests::ParsesEmptyClisArray()
    {
        constexpr std::string_view js = R"({
            "schema_version": 4,
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
            "schema_version": 4,
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
            "schema_version": 4,
            "clis": [],
            "bundle_source": { "kind": "none" }
        })";
        const auto r = ParseStatusJson(js);
        VERIFY_IS_TRUE(r.has_value());
        VERIFY_ARE_EQUAL(std::string{ "none" }, r->bundleKind);
        VERIFY_IS_FALSE(r->bundlePath.has_value());
    }

    // v4: the version pair is the only part of the report that distinguishes
    // "hooks are installed" from "the hooks this build ships are installed",
    // so both halves must survive the parse — including the asymmetric case
    // where a CLI has nothing installed but the bundle still has a version to
    // offer.
    void AgentHooksStatusTests::ParsesVersions()
    {
        const auto report = ParseStatusJson(kHappyPathJson);
        VERIFY_IS_TRUE(report.has_value());

        const auto* copilot = FindCli(*report, "copilot");
        VERIFY_IS_NOT_NULL(copilot);
        VERIFY_IS_TRUE(copilot->installedVersion.has_value());
        VERIFY_ARE_EQUAL(std::string{ "0.1.5" }, *copilot->installedVersion);
        VERIFY_IS_TRUE(copilot->bundleVersion.has_value());
        VERIFY_ARE_EQUAL(std::string{ "0.1.5" }, *copilot->bundleVersion);

        // Installed, but a build behind the bundle.
        const auto* claude = FindCli(*report, "claude");
        VERIFY_IS_NOT_NULL(claude);
        VERIFY_ARE_EQUAL(std::string{ "0.1.4" }, *claude->installedVersion);
        VERIFY_ARE_EQUAL(std::string{ "0.1.5" }, *claude->bundleVersion);

        // Nothing installed: no installed version to report, but the bundle
        // still names what an install would produce.
        const auto* gemini = FindCli(*report, "gemini");
        VERIFY_IS_NOT_NULL(gemini);
        VERIFY_IS_FALSE(gemini->installedVersion.has_value());
        VERIFY_IS_TRUE(gemini->bundleVersion.has_value());
        VERIFY_ARE_EQUAL(std::string{ "0.1.5" }, *gemini->bundleVersion);

        // wta omits both fields when it can't establish them; that must read
        // as "unknown", not as a malformed report.
        const auto* openCode = FindCli(*report, "opencode");
        VERIFY_IS_NOT_NULL(openCode);
        VERIFY_IS_FALSE(openCode->installedVersion.has_value());
        VERIFY_IS_FALSE(openCode->bundleVersion.has_value());
    }

    void AgentHooksStatusTests::IgnoresUnknownExtraFields()
    {
        // Forward compatibility: wta may add fields in a future minor
        // bump. We must not reject them as long as schema_version still
        // matches the supported major.
        constexpr std::string_view js = R"({
            "schema_version": 4,
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
            "schema_version": 4,
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

    // ---- wta hooks install --json --------------------------------------

    // Shape of a real failing run: Codex's marketplace still points at a
    // moved bundle, so its `marketplace add` fails while every other CLI
    // installs. This is the case the Settings UI used to report as a bare
    // "Hooks installation failed" with no way to tell which CLI broke.
    static constexpr std::string_view kInstallReportJson = R"({
        "schema_version": 1,
        "clis": [
            { "name": "copilot", "outcome": "installed" },
            { "name": "claude", "outcome": "installed" },
            { "name": "gemini", "outcome": "skipped" },
            { "name": "codex", "outcome": "failed",
              "reason": "codex plugin marketplace add failed: already added from a different source" },
            { "name": "opencode", "outcome": "skipped" }
        ]
    })";

    void AgentHooksStatusTests::ParsesInstallReport()
    {
        const auto r = ParseInstallReportJson(kInstallReportJson);
        VERIFY_IS_TRUE(r.has_value());
        VERIFY_ARE_EQUAL(1u, r->schemaVersion);
        VERIFY_ARE_EQUAL(size_t{ 5 }, r->clis.size());

        VERIFY_ARE_EQUAL(std::string{ "codex" }, r->clis[3].name);
        VERIFY_ARE_EQUAL(std::string{ "failed" }, r->clis[3].outcome);
        VERIFY_IS_TRUE(r->clis[3].reason.has_value());

        // A non-failure carries no reason, and absence must not be an error.
        VERIFY_IS_FALSE(r->clis[0].reason.has_value());

        VERIFY_ARE_EQUAL(std::wstring{ L"Codex CLI" }, FormatFailedCliList(*r));
    }

    void AgentHooksStatusTests::NamesEveryFailedInstallCli()
    {
        constexpr std::string_view js = R"({
            "schema_version": 1,
            "clis": [
                { "name": "copilot", "outcome": "failed" },
                { "name": "claude", "outcome": "installed" },
                { "name": "opencode", "outcome": "failed" }
            ]
        })";
        const auto r = ParseInstallReportJson(js);
        VERIFY_IS_TRUE(r.has_value());
        // Display names must match the row titles in AIAgents.xaml so the
        // summary names each CLI the same way the list above it does.
        VERIFY_ARE_EQUAL(std::wstring{ L"GitHub Copilot, OpenCode" }, FormatFailedCliList(*r));
    }

    // An all-clear report must produce no list — the caller uses "empty" to
    // decide it has nothing to attribute and falls back to the generic
    // message rather than rendering "failed for ." at the user.
    void AgentHooksStatusTests::FormatsNoFailuresAsEmpty()
    {
        constexpr std::string_view js = R"({
            "schema_version": 1,
            "clis": [
                { "name": "copilot", "outcome": "installed" },
                { "name": "gemini", "outcome": "skipped" }
            ]
        })";
        const auto r = ParseInstallReportJson(js);
        VERIFY_IS_TRUE(r.has_value());
        VERIFY_IS_TRUE(FormatFailedCliList(*r).empty());
    }

    // Forward compatibility: an outcome this build doesn't know must not be
    // reported as a failure, or a future wta that adds a state would make
    // every successful install look broken.
    void AgentHooksStatusTests::TreatsUnknownInstallOutcomeAsNonFailure()
    {
        constexpr std::string_view js = R"({
            "schema_version": 1,
            "clis": [
                { "name": "copilot", "outcome": "deferred" },
                { "name": "claude" }
            ]
        })";
        const auto r = ParseInstallReportJson(js);
        VERIFY_IS_TRUE(r.has_value());
        VERIFY_IS_TRUE(FormatFailedCliList(*r).empty());
    }

    // A CLI wta supports but this build has no display name for is still
    // worth naming — dropping it would report a failure attributed to
    // nothing at all.
    void AgentHooksStatusTests::FallsBackToRawNameForUnknownCli()
    {
        constexpr std::string_view js = R"({
            "schema_version": 1,
            "clis": [ { "name": "newcli", "outcome": "failed" } ]
        })";
        const auto r = ParseInstallReportJson(js);
        VERIFY_IS_TRUE(r.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"newcli" }, FormatFailedCliList(*r));
    }

    void AgentHooksStatusTests::RejectsUnsupportedInstallSchemaVersion()
    {
        constexpr std::string_view js = R"({
            "schema_version": 99,
            "clis": []
        })";
        VERIFY_IS_FALSE(ParseInstallReportJson(js).has_value());
    }

    void AgentHooksStatusTests::RejectsMalformedInstallReport()
    {
        VERIFY_IS_FALSE(ParseInstallReportJson("").has_value());
        VERIFY_IS_FALSE(ParseInstallReportJson("not json").has_value());
        // Missing `clis` entirely.
        VERIFY_IS_FALSE(ParseInstallReportJson(R"({ "schema_version": 1 })").has_value());
        // An entry without a name can't be attributed to a CLI.
        VERIFY_IS_FALSE(
            ParseInstallReportJson(R"({ "schema_version": 1, "clis": [ { "outcome": "failed" } ] })").has_value());
    }
}
