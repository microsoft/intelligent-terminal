// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../inc/ShellIntegrationDiagnostics.h"

using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;
using namespace Microsoft::Terminal::ShellIntegration::Diagnostics;

namespace TerminalAppUnitTests
{
    class ShellIntegrationDiagnosticsTests
    {
        TEST_CLASS(ShellIntegrationDiagnosticsTests);

        TEST_METHOD(ParsesReadySignal);
        TEST_METHOD(ParsesLegacyReadySignal);
        TEST_METHOD(ParsesRepairSignal);
        TEST_METHOD(ParsesRuntimeSignal);
        TEST_METHOD(RejectsUnknownTarget);
        TEST_METHOD(RejectsUnknownReason);
        TEST_METHOD(RejectsUnknownRuntimeOutcome);
        TEST_METHOD(RejectsUnknownVersion);
        TEST_METHOD(RejectsMissingField);
        TEST_METHOD(RejectsExtraField);
        TEST_METHOD(RejectsUnknownSignalName);
    };

    void ShellIntegrationDiagnosticsTests::ParsesReadySignal()
    {
        const auto signal = ParseSignal("osc:9001;ShellIntegrationReady;pwsh;8");
        VERIFY_IS_TRUE(signal.has_value());
        VERIFY_ARE_EQUAL(static_cast<int>(SignalKind::Ready), static_cast<int>(signal->kind));
        VERIFY_ARE_EQUAL(static_cast<int>(ShellTarget::Pwsh), static_cast<int>(signal->target));
        VERIFY_IS_FALSE(signal->repairReason.has_value());
        VERIFY_IS_FALSE(signal->runtimeOutcome.has_value());
    }

    void ShellIntegrationDiagnosticsTests::ParsesLegacyReadySignal()
    {
        const auto signal = ParseSignal("osc:9001;ShellIntegrationReady;powershell;7");
        VERIFY_IS_TRUE(signal.has_value());
        VERIFY_ARE_EQUAL(static_cast<int>(SignalKind::Ready), static_cast<int>(signal->kind));
    }

    void ShellIntegrationDiagnosticsTests::ParsesRepairSignal()
    {
        const auto signal = ParseSignal("osc:9001;ShellIntegrationRepair;powershell;bind-failed");
        VERIFY_IS_TRUE(signal.has_value());
        VERIFY_ARE_EQUAL(static_cast<int>(SignalKind::Repair), static_cast<int>(signal->kind));
        VERIFY_ARE_EQUAL(static_cast<int>(ShellTarget::WindowsPowerShell), static_cast<int>(signal->target));
        VERIFY_IS_TRUE(signal->repairReason.has_value());
        VERIFY_ARE_EQUAL(static_cast<int>(RepairReason::BindFailed), static_cast<int>(*signal->repairReason));
        VERIFY_IS_FALSE(signal->runtimeOutcome.has_value());
    }

    void ShellIntegrationDiagnosticsTests::ParsesRuntimeSignal()
    {
        const auto signal = ParseSignal("osc:9001;ShellIntegrationRuntime;pwsh;rebound");
        VERIFY_IS_TRUE(signal.has_value());
        VERIFY_ARE_EQUAL(static_cast<int>(SignalKind::Runtime), static_cast<int>(signal->kind));
        VERIFY_ARE_EQUAL(static_cast<int>(ShellTarget::Pwsh), static_cast<int>(signal->target));
        VERIFY_IS_FALSE(signal->repairReason.has_value());
        VERIFY_IS_TRUE(signal->runtimeOutcome.has_value());
        VERIFY_ARE_EQUAL(static_cast<int>(RuntimeOutcome::Rebound), static_cast<int>(*signal->runtimeOutcome));

        const auto failure = ParseSignal("osc:9001;ShellIntegrationRuntime;powershell;rebind-failed");
        VERIFY_IS_TRUE(failure.has_value());
        VERIFY_ARE_EQUAL(static_cast<int>(RuntimeOutcome::RebindFailed), static_cast<int>(*failure->runtimeOutcome));
    }

    void ShellIntegrationDiagnosticsTests::RejectsUnknownTarget()
    {
        VERIFY_IS_FALSE(ParseSignal("osc:9001;ShellIntegrationReady;bash;7").has_value());
    }

    void ShellIntegrationDiagnosticsTests::RejectsUnknownReason()
    {
        VERIFY_IS_FALSE(ParseSignal("osc:9001;ShellIntegrationRepair;pwsh;different-reason").has_value());
    }

    void ShellIntegrationDiagnosticsTests::RejectsUnknownRuntimeOutcome()
    {
        VERIFY_IS_FALSE(ParseSignal("osc:9001;ShellIntegrationRuntime;pwsh;different-outcome").has_value());
    }

    void ShellIntegrationDiagnosticsTests::RejectsUnknownVersion()
    {
        VERIFY_IS_FALSE(ParseSignal("osc:9001;ShellIntegrationReady;pwsh;9").has_value());
    }

    void ShellIntegrationDiagnosticsTests::RejectsMissingField()
    {
        VERIFY_IS_FALSE(ParseSignal("osc:9001;ShellIntegrationRepair;pwsh").has_value());
    }

    void ShellIntegrationDiagnosticsTests::RejectsExtraField()
    {
        VERIFY_IS_FALSE(ParseSignal("osc:9001;ShellIntegrationReady;pwsh;7;extra").has_value());
    }

    void ShellIntegrationDiagnosticsTests::RejectsUnknownSignalName()
    {
        VERIFY_IS_FALSE(ParseSignal("osc:9001;ShellIntegrationSomething;pwsh;7").has_value());
    }
}
