// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include <array>
#include <string>

#include "../TerminalApp/BashProfileAnalyzer.h"
#include "../inc/BashShellIntegration.h"

using namespace Microsoft::Terminal::ShellIntegration;
using namespace Microsoft::Terminal::ShellIntegration::Health;
using namespace Microsoft::Terminal::ShellIntegration::Bash::ProfileAnalyzer;
using namespace WEX::TestExecution;

namespace TerminalAppUnitTests
{
    class BashProfileAnalyzerTests
    {
        TEST_CLASS(BashProfileAnalyzerTests);

        TEST_METHOD(HealthyLfBlockReportsExactOffsets);
        TEST_METHOD(HealthyCrLfBlockReportsExactOffsets);
        TEST_METHOD(RootLevelAliasAfterBlockIsNotLast);
        TEST_METHOD(RootLevelExportAfterBlockIsNotLast);
        TEST_METHOD(RootLevelEvalAfterBlockIsNotLast);
        TEST_METHOD(MarkerTextInStringIsNotInstalled);
        TEST_METHOD(MarkerTextInHeredocIsNotInstalled);
        TEST_METHOD(NestedMarkersAreIndeterminate);
        TEST_METHOD(DuplicateMarkersAreIndeterminate);
        TEST_METHOD(OrphanMarkerIsIndeterminate);
        TEST_METHOD(NonCanonicalBlockIsIndeterminate);
        TEST_METHOD(IndentedMarkerIsIndeterminate);
        TEST_METHOD(SyntaxErrorIsIndeterminate);
        TEST_METHOD(MissingSyntaxTokenIsIndeterminate);
        TEST_METHOD(OversizedInputIsIndeterminate);
    };

    namespace
    {
        std::string CrLf(std::string_view text)
        {
            std::string result;
            result.reserve(text.size() * 2);
            for (const auto ch : text)
            {
                if (ch == '\n')
                {
                    result += "\r\n";
                }
                else
                {
                    result += ch;
                }
            }
            return result;
        }

        void VerifyStatus(const AnalysisResult& actual, const Status expectedStatus, const Reason expectedReason)
        {
            VERIFY_ARE_EQUAL(expectedStatus, actual.status);
            VERIFY_ARE_EQUAL(expectedReason, actual.reason);
        }
    }

    void BashProfileAnalyzerTests::HealthyLfBlockReportsExactOffsets()
    {
        const auto block = Bash::BuildBlock("\n");
        const std::string profile = "# user setup\n" + block + "\n# user comment\n";

        const auto actual = Analyze(profile);

        VerifyStatus(actual, Status::Healthy, Reason::None);
        VERIFY_ARE_EQUAL(size_t{ 13 }, actual.blockStart);
        VERIFY_ARE_EQUAL(actual.blockStart + block.size(), actual.blockEnd);
    }

    void BashProfileAnalyzerTests::HealthyCrLfBlockReportsExactOffsets()
    {
        const auto block = CrLf(Bash::BuildBlock("\n"));
        const std::string profile = "# user setup\r\n" + block + "\r\n# user comment\r\n";

        const auto actual = Analyze(profile);

        VerifyStatus(actual, Status::Healthy, Reason::None);
        VERIFY_ARE_EQUAL(size_t{ 14 }, actual.blockStart);
        VERIFY_ARE_EQUAL(actual.blockStart + block.size(), actual.blockEnd);
    }

    void BashProfileAnalyzerTests::RootLevelAliasAfterBlockIsNotLast()
    {
        const auto block = Bash::BuildBlock("\n");
        const auto actual = Analyze(block + "\nalias ll='ls -la'\n");

        VerifyStatus(actual, Status::BlockNotLast, Reason::None);
    }

    void BashProfileAnalyzerTests::RootLevelExportAfterBlockIsNotLast()
    {
        const auto block = Bash::BuildBlock("\n");
        const auto actual = Analyze(block + "\nexport EDITOR=vim\n");

        VerifyStatus(actual, Status::BlockNotLast, Reason::None);
    }

    void BashProfileAnalyzerTests::RootLevelEvalAfterBlockIsNotLast()
    {
        const auto block = Bash::BuildBlock("\n");
        const auto actual = Analyze(block + "\neval \"$(starship init bash)\"\n");

        VerifyStatus(actual, Status::BlockNotLast, Reason::None);
    }

    void BashProfileAnalyzerTests::MarkerTextInStringIsNotInstalled()
    {
        const std::string profile = "message='# >>> intelligent-terminal shell-integration >>>'\n";

        VerifyStatus(Analyze(profile), Status::NotInstalled, Reason::MissingBlock);
    }

    void BashProfileAnalyzerTests::MarkerTextInHeredocIsNotInstalled()
    {
        const std::string profile = "cat <<'EOF'\n# >>> intelligent-terminal shell-integration >>>\n# <<< intelligent-terminal shell-integration <<<\nEOF\n";

        VerifyStatus(Analyze(profile), Status::NotInstalled, Reason::MissingBlock);
    }

    void BashProfileAnalyzerTests::NestedMarkersAreIndeterminate()
    {
        const auto block = Bash::BuildBlock("\n");
        const std::array<std::string, 5> profiles = {
            "function f() {\n" + block + "\n}\n",
            "if true; then\n" + block + "\nfi\n",
            "case x in\nx)\n" + block + "\n;;\nesac\n",
            "(\n" + block + "\n)\n",
            "value=$( \n" + block + "\n)\n",
        };

        for (const auto& profile : profiles)
        {
            VerifyStatus(Analyze(profile), Status::Indeterminate, Reason::MalformedBlock);
        }
    }

    void BashProfileAnalyzerTests::DuplicateMarkersAreIndeterminate()
    {
        const auto block = Bash::BuildBlock("\n");

        VerifyStatus(Analyze(block + "\n" + block), Status::Indeterminate, Reason::MalformedBlock);
    }

    void BashProfileAnalyzerTests::OrphanMarkerIsIndeterminate()
    {
        VerifyStatus(
            Analyze(std::string{ kShellIntegrationBlockOpenMarker } + "\n# unfinished\n"),
            Status::Indeterminate,
            Reason::MalformedBlock);
        VerifyStatus(
            Analyze(std::string{ kShellIntegrationBlockCloseMarker } + "\n# unfinished\n"),
            Status::Indeterminate,
            Reason::MalformedBlock);
    }

    void BashProfileAnalyzerTests::NonCanonicalBlockIsIndeterminate()
    {
        auto profile = Bash::BuildBlock("\n");
        const auto body = profile.find("    unset __it_si");
        VERIFY_IS_TRUE(body != std::string::npos);
        profile.replace(body, std::string_view{ "    unset __it_si" }.size(), "    :");

        VerifyStatus(Analyze(profile), Status::Indeterminate, Reason::MalformedBlock);
    }

    void BashProfileAnalyzerTests::IndentedMarkerIsIndeterminate()
    {
        auto profile = Bash::BuildBlock("\n");
        profile.insert(0, "  ");

        VerifyStatus(Analyze(profile), Status::Indeterminate, Reason::MalformedBlock);
    }

    void BashProfileAnalyzerTests::SyntaxErrorIsIndeterminate()
    {
        const auto block = Bash::BuildBlock("\n");

        VerifyStatus(Analyze(block + "\nif then\n"), Status::Indeterminate, Reason::ParseFailed);
    }

    void BashProfileAnalyzerTests::MissingSyntaxTokenIsIndeterminate()
    {
        const auto block = Bash::BuildBlock("\n");

        VerifyStatus(Analyze(block + "\nif true; then\n"), Status::Indeterminate, Reason::ParseFailed);
    }

    void BashProfileAnalyzerTests::OversizedInputIsIndeterminate()
    {
        const std::string profile(MaximumProfileBytes + 1, '#');

        VerifyStatus(Analyze(profile), Status::Indeterminate, Reason::FileTooLarge);
    }
}
