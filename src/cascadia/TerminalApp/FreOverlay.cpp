// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "FreOverlay.h"
#include "FreAgentEntry.g.cpp"
#include "FreOverlay.g.cpp"

#include "../inc/AgentRegistry.h"
#include "../inc/WtaProcess.h"
#include "../inc/ShellIntegration.h"
#include "../inc/RtlHelper.h"
#include "AgentPaneLog.h"

#include <winrt/Windows.UI.Xaml.Documents.h>

using namespace winrt::Windows::Foundation;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;
using namespace winrt::Windows::UI::Xaml::Documents;
namespace Automation = winrt::Windows::UI::Xaml::Automation;

namespace winrt::TerminalApp::implementation
{
    FreOverlay::FreOverlay()
    {
        InitializeComponent();

        // Seed the overlay's status text from the existing localized
        // resource (reused here rather than adding a new .Text key
        // across every locale).
        SavingStatusText().Text(RS_(L"FreOverlay_SettingUp"));
    }

    // ── Detection helpers ───────────────────────────────────────────────

    bool FreOverlay::_IsAgentInstalled(const wchar_t* name)
    {
        wchar_t buf[MAX_PATH]{};
        if (SearchPathW(nullptr, name, L".exe", MAX_PATH, buf, nullptr) > 0)
        {
            _agentPaneLog("[FRE] _IsAgentInstalled: " + winrt::to_string(winrt::hstring{ name }) + " found at " + winrt::to_string(winrt::hstring{ buf }));
            return true;
        }
        const auto cmdName = std::wstring(name) + L".cmd";
        if (SearchPathW(nullptr, cmdName.c_str(), nullptr, MAX_PATH, buf, nullptr) > 0)
        {
            _agentPaneLog("[FRE] _IsAgentInstalled: " + winrt::to_string(winrt::hstring{ name }) + " found at " + winrt::to_string(winrt::hstring{ buf }));
            return true;
        }
        _agentPaneLog("[FRE] _IsAgentInstalled: " + winrt::to_string(winrt::hstring{ name }) + " NOT found on PATH");
        return false;
    }

    bool FreOverlay::_IsNodeInstalled()
    {
        wchar_t buf[MAX_PATH];
        if (SearchPathW(nullptr, L"npx", L".cmd", MAX_PATH, buf, nullptr) > 0)
            return true;
        if (SearchPathW(nullptr, L"npx", L".exe", MAX_PATH, buf, nullptr) > 0)
            return true;
        return false;
    }

    // Detect whether winget itself is available on PATH. When winget is
    // missing (e.g. App Installer not installed, or stripped on LTSC/Server
    // SKUs) the Copilot/Node bootstrap calls would fail with a generic
    // "install failed" error that wrongly points at the package; surface a
    // dedicated message that links to the winget setup docs instead.
    bool FreOverlay::_IsWingetInstalled()
    {
        wchar_t buf[MAX_PATH];
        return SearchPathW(nullptr, L"winget", L".exe", MAX_PATH, buf, nullptr) > 0;
    }

    // ── Agent ComboBox ──────────────────────────────────────────────────

    // (Re)build the agent dropdown from the GPO-filtered registry. Each entry's
    // status label reflects the live install state at call time, so calling this
    // again after a save refreshes Copilot from "(will install)" to
    // "(installed)" once the winget install has actually succeeded. Preserves
    // the currently selected agent across rebuilds.
    void FreOverlay::_PopulateAgentComboBox()
    {
        if (!_settings)
            return;

        namespace Reg = ::Microsoft::Terminal::Settings::Model::AgentRegistry;
        const auto& globals = _settings.GlobalSettings();

        // Keep the user's current selection across a rebuild: prefer the live
        // ComboBox selection, falling back to the effective settings value the
        // first time (when nothing is selected yet).
        winrt::hstring selectedId;
        if (const auto selected = AgentComboBox().SelectedItem())
        {
            if (const auto entry = selected.try_as<winrt::TerminalApp::FreAgentEntry>())
            {
                selectedId = entry.Id();
            }
        }
        if (selectedId.empty())
        {
            selectedId = globals.EffectiveAcpAgent();
        }

        const auto allowedAgents = Reg::FilteredAcpAgents();
        auto items = AgentComboBox().Items();
        items.Clear();
        int32_t selectedIndex = 0;
        int32_t idx = 0;

        for (const auto& a : allowedAgents)
        {
            const bool installed = _IsAgentInstalled(std::wstring{ a.id }.c_str());
            const bool isCopilot = (a.id == L"copilot");

            // Show Copilot always + detected agents only
            if (!isCopilot && !installed)
                continue;

            auto entry = winrt::make<FreAgentEntry>();
            entry.Id(winrt::hstring{ a.id });

            if (isCopilot && !installed)
            {
                entry.DisplayLabel(winrt::hstring{ std::wstring(a.displayName) + std::wstring(RS_(L"FreOverlay_AgentStatusWillInstall")) });
            }
            else
            {
                entry.DisplayLabel(winrt::hstring{ std::wstring(a.displayName) + std::wstring(RS_(L"FreOverlay_AgentStatusInstalled")) });
            }

            items.Append(entry);

            if (a.id == selectedId)
            {
                selectedIndex = idx;
            }
            idx++;
        }

        if (items.Size() > 0)
        {
            AgentComboBox().SelectedIndex(selectedIndex);
        }
    }

    // ── Initialize ──────────────────────────────────────────────────────

    void FreOverlay::Initialize(const winrt::Microsoft::Terminal::Settings::Model::CascadiaSettings& settings)
    {
        _settings = settings;
        const auto& globals = _settings.GlobalSettings();

        // Honor RTL languages on the FRE root grid. XAML cascades
        // FlowDirection down the tree and auto-mirrors HorizontalAlignment,
        // so this single line is enough to flip the entire two-page wizard
        // for any RTL language the OS knows about (and the qps-plocm
        // pseudo-locale used for validation). We honor the explicit
        // `Language` override from settings.json first (matches the way
        // AppLogic::_ApplyLanguageSettingChange resolves it), then fall
        // back to the OS preferred UI language.
        {
            winrt::hstring language = globals.Language();
            if (language.empty())
            {
                try
                {
                    const auto langs = winrt::Windows::Globalization::ApplicationLanguages::Languages();
                    if (langs && langs.Size() > 0)
                    {
                        language = langs.GetAt(0);
                    }
                }
                CATCH_LOG();
            }
            // Explicit on both branches so that re-initializing the
            // same overlay element for a different language correctly
            // resets the cascade — Initialize is called every time the
            // FRE is shown, and the underlying XAML element is reused.
            using winrt::Windows::UI::Xaml::FlowDirection;
            RootGrid().FlowDirection(::Microsoft::Terminal::RtlHelper::IsRtlLocale(language)
                                         ? FlowDirection::RightToLeft
                                         : FlowDirection::LeftToRight);
        }

        // Set subtitle Run texts (can't use x:Uid for <Run> inside <Hyperlink>)
        WelcomeSubtitlePrefix().Text(RS_(L"FreOverlay_WelcomeSubtitlePrefix"));
        WelcomeSubtitleLink().Text(RS_(L"FreOverlay_WelcomeSubtitleLink"));
        SettingsSubtitlePrefix().Text(RS_(L"FreOverlay_SettingsSubtitlePrefix"));
        SettingsSubtitleLink().Text(RS_(L"FreOverlay_SettingsSubtitleLink"));
        AutoDetectShellIntegrationHintPrefix().Text(RS_(L"FreOverlay_AutoDetectShellIntegrationHintPrefix"));
        AutoDetectShellIntegrationHintLink().Text(RS_(L"FreOverlay_AutoDetectShellIntegrationHintLink"));

        // Split the description on "ACP" (locked token) so it can be rendered as an inline Hyperlink.
        {
            const auto descStr = RS_(L"FreOverlay_AgentDescription/Text");
            const std::wstring_view desc{ descStr };
            constexpr std::wstring_view token{ L"ACP" };
            const auto pos = desc.find(token);
            if (pos != std::wstring_view::npos)
            {
                AgentDescriptionBefore().Text(winrt::hstring{ desc.substr(0, pos) });
                AgentDescriptionAcpToken().Text(winrt::hstring{ token });
                AgentDescriptionAfter().Text(winrt::hstring{ desc.substr(pos + token.size()) });
            }
            else
            {
                // Fallback (shouldn't happen — ACP is locked): degrade to plain text.
                AgentDescriptionBefore().Text(winrt::hstring{ desc });
            }
        }

        // Set toggle On/Off labels
        AutoDetectToggle().OnContent(winrt::box_value(RS_(L"FreOverlay_ToggleOn")));
        AutoDetectToggle().OffContent(winrt::box_value(RS_(L"FreOverlay_ToggleOff")));
        AutoErrorToggle().OnContent(winrt::box_value(RS_(L"FreOverlay_ToggleOn")));
        AutoErrorToggle().OffContent(winrt::box_value(RS_(L"FreOverlay_ToggleOff")));
        SessionManagementToggle().OnContent(winrt::box_value(RS_(L"FreOverlay_ToggleOn")));
        SessionManagementToggle().OffContent(winrt::box_value(RS_(L"FreOverlay_ToggleOff")));

        // Populate agent ComboBox using GPO-filtered list — only agents
        // permitted by policy are shown. Each entry's status label reflects the
        // live install state, so this is re-run after a save to flip Copilot
        // from "(will install)" to "(installed)".
        _PopulateAgentComboBox();

        // Agent dropdown — show policy notice if AllowedAgents GPO is active
        if (globals.IsAgentPolicyLocked())
        {
            const auto policyText = RS_(L"FreOverlay_PolicyLocked");
            AgentPolicyNotice().Text(policyText);
            AgentPolicyNotice().Visibility(Visibility::Visible);
            Automation::AutomationProperties::SetHelpText(AgentComboBox(), policyText);
        }

        // Populate pane position ComboBox
        auto posItems = PanePositionComboBox().Items();
        posItems.Clear();
        posItems.Append(winrt::box_value(RS_(L"FreOverlay_PanePositionBottom")));
        posItems.Append(winrt::box_value(RS_(L"FreOverlay_PanePositionRight")));
        posItems.Append(winrt::box_value(RS_(L"FreOverlay_PanePositionLeft")));
        posItems.Append(winrt::box_value(RS_(L"FreOverlay_PanePositionTop")));

        const auto currentPos = globals.AgentPanePosition();
        if (currentPos == L"right") PanePositionComboBox().SelectedIndex(1);
        else if (currentPos == L"left") PanePositionComboBox().SelectedIndex(2);
        else if (currentPos == L"top") PanePositionComboBox().SelectedIndex(3);
        else PanePositionComboBox().SelectedIndex(0); // default: bottom

        // Set toggles from current settings, respecting GPO policy.
        // Detection drives the suggestion toggle's enabled state (see
        // _UpdateSuggestionEnabledState), so configure it first.
        AutoDetectToggle().IsOn(globals.EffectiveAutoErrorDetectionEnabled());

        // Master-detail: EffectiveAutoFixEnabled already returns false when
        // detection is off, so the suggestion toggle starts consistent with the
        // master toggle (and reflects the stored preference when detection is
        // on).
        AutoErrorToggle().IsOn(globals.EffectiveAutoFixEnabled());
        if (globals.IsAutoFixPolicyLocked())
        {
            const auto policyText = RS_(L"FreOverlay_PolicyLocked");
            AutoErrorPolicyNotice().Text(policyText);
            AutoErrorPolicyNotice().Visibility(Visibility::Visible);
            // Accessibility: explain why the toggle is disabled
            Automation::AutomationProperties::SetHelpText(AutoErrorToggle(), policyText);
        }

        // Apply the detection→suggestion dependency once both toggles are
        // configured (also covers the GPO-locked case via the policy check
        // inside the helper).
        _UpdateSuggestionEnabledState();

        // Session management toggle — honour AllowAgentSessionHooks GPO
        if (globals.IsAgentSessionHooksPolicyLocked())
        {
            SessionManagementToggle().IsOn(false);
            SessionManagementToggle().IsEnabled(false);
            const auto policyText = RS_(L"FreOverlay_PolicyLocked");
            SessionHooksPolicyNotice().Text(policyText);
            SessionHooksPolicyNotice().Visibility(Visibility::Visible);
            // Accessibility: explain why the toggle is disabled
            Automation::AutomationProperties::SetHelpText(SessionManagementToggle(), policyText);
        }

        // ── Accessibility: set AutomationProperties.Name so screen readers
        //    announce controls and pages correctly. Re-uses existing x:Uid
        //    .Text values from Resources.resw — no extra keys needed.
        Automation::AutomationProperties::SetName(
            WelcomePage(), RS_(L"FreOverlay_WelcomeTitle/Text"));
        Automation::AutomationProperties::SetName(
            SettingsPage(), RS_(L"FreOverlay_SettingsTitle/Text"));
        Automation::AutomationProperties::SetName(
            AutoDetectToggle(), RS_(L"FreOverlay_AutoDetectLabel/Text"));
        Automation::AutomationProperties::SetName(
            AutoErrorToggle(), RS_(L"FreOverlay_AutoErrorLabel/Text"));
        Automation::AutomationProperties::SetName(
            SessionManagementToggle(), RS_(L"FreOverlay_SessionLabel/Text"));
        Automation::AutomationProperties::SetName(
            AgentComboBox(), RS_(L"FreOverlay_AgentLabel/Text"));
        Automation::AutomationProperties::SetName(
            PanePositionComboBox(), RS_(L"FreOverlay_PanePositionLabel/Text"));

        // Give the SavingProgressRing a localized accessible Name so
        // Narrator announces "Setting up Intelligent Terminal, busy"
        // when focus lands on it during a save/install (and the same
        // readout on Caps+Tab mid-install). _SetSavingState defers
        // the ring.Focus() call via Dispatcher().RunAsync(Low) so it
        // fires after IsActive(true) and the visibility change have
        // been laid out — the announcement combines this Name with
        // the "busy" state from the active spinner in a single
        // readout. Without this Name, Narrator would just read
        // "ProgressRing".
        Automation::AutomationProperties::SetName(
            SavingProgressRing(), RS_(L"FreOverlay_SettingUp"));
    }

    // ── Agent selection changed ─────────────────────────────────────────

    void FreOverlay::_OnAgentSelectionChanged(const IInspectable& /*sender*/,
                                              const winrt::Windows::UI::Xaml::Controls::SelectionChangedEventArgs& /*args*/)
    {
        // Show Node.js install hint for Claude/Codex (they use npx adapters)
        if (const auto selected = AgentComboBox().SelectedItem())
        {
            if (const auto entry = selected.try_as<winrt::TerminalApp::FreAgentEntry>())
            {
                const auto id = entry.Id();
                const bool needsNode = (id == L"claude" || id == L"codex");
                AgentInstallHintRow().Visibility(needsNode ? Visibility::Visible : Visibility::Collapsed);
            }
        }
    }

    void FreOverlay::_OnSessionManagementToggled(const IInspectable& /*sender*/,
                                                  const RoutedEventArgs& /*args*/)
    {
        // Guard: event can fire during InitializeComponent before controls exist
        auto toggle = SessionManagementToggle();
        // Hide/show the whole hint row (icon + text), not just the text — the
        // monochrome FontIcon lives in the same StackPanel and would otherwise
        // be left dangling when the toggle is off.
        auto row = SessionManagementHintRow();
        if (toggle && row)
        {
            row.Visibility(toggle.IsOn() ? Visibility::Visible : Visibility::Collapsed);
        }
    }

    // ── Detection → suggestion dependency ───────────────────────────────

    void FreOverlay::_OnAutoDetectToggled(const IInspectable& /*sender*/,
                                          const RoutedEventArgs& /*args*/)
    {
        _UpdateSuggestionEnabledState();

        // Hide/show the whole hint row (icon + text) — the (i) glyph would
        // otherwise dangle when detection is off and the side-effect described
        // by the hint no longer applies. Mirrors SessionManagementHintRow.
        auto toggle = AutoDetectToggle();
        auto row = AutoDetectShellIntegrationHintRow();
        if (toggle && row)
        {
            row.Visibility(toggle.IsOn() ? Visibility::Visible : Visibility::Collapsed);
        }
    }

    void FreOverlay::_UpdateSuggestionEnabledState()
    {
        // Guard: Toggled can fire during InitializeComponent before the
        // sibling control exists.
        auto detect = AutoDetectToggle();
        auto suggest = AutoErrorToggle();
        if (!detect || !suggest)
        {
            return;
        }

        const bool detectionOn = detect.IsOn();
        const bool autoFixLocked = _settings && _settings.GlobalSettings().IsAutoFixPolicyLocked();

        // Master-detail: detection off ⇒ turn the suggestion off and disable it
        // (can't configure a suggestion you can't detect).
        // Detection on ⇒ re-enable it; its On/Off is the stored preference
        // (set on init), so re-enabling doesn't force it on. The auto-fix GPO
        // can still lock it off.
        if (!detectionOn)
        {
            suggest.IsOn(false);
        }
        suggest.IsEnabled(detectionOn && !autoFixLocked);
    }

    // ── Page navigation ─────────────────────────────────────────────────

    void FreOverlay::_OnNextButtonClick(const IInspectable& /*sender*/,
                                        const RoutedEventArgs& /*args*/)
    {
        WelcomePage().Visibility(Visibility::Collapsed);
        SettingsPage().Visibility(Visibility::Visible);

        // Focus the Save button so Enter triggers it on the Settings page.
        Dispatcher().RunAsync(winrt::Windows::UI::Core::CoreDispatcherPriority::Low,
            [weak = get_weak()]() {
                if (auto self = weak.get())
                {
                    self->SaveButton().Focus(FocusState::Programmatic);
                }
            });
    }

    // ── WinGet install helper ───────────────────────────────────────────

    IAsyncOperation<bool> FreOverlay::_WingetInstallAsync(winrt::hstring packageId)
    {
        // Copy packageId before switching threads (coroutine parameter safety)
        auto id = std::wstring{ packageId };

        co_await winrt::resume_background();

        auto cmdline = fmt::format(
            L"winget install --id {} --exact --silent "
            L"--source winget "
            L"--accept-source-agreements --accept-package-agreements "
            L"--disable-interactivity",
            id);

        // Create a pipe to capture winget's combined stdout+stderr for
        // diagnostic logging. The pipe is inheritable so the child
        // process writes directly to it.
        SECURITY_ATTRIBUTES sa{};
        sa.nLength = sizeof(sa);
        sa.bInheritHandle = TRUE;
        HANDLE hReadPipe = nullptr, hWritePipe = nullptr;
        const bool hasPipe = CreatePipe(&hReadPipe, &hWritePipe, &sa, 0);
        if (hasPipe)
        {
            // Prevent the read end from being inherited by the child.
            SetHandleInformation(hReadPipe, HANDLE_FLAG_INHERIT, 0);
        }

        STARTUPINFOW si{};
        si.cb = sizeof(si);
        si.dwFlags = STARTF_USESHOWWINDOW;
        si.wShowWindow = SW_HIDE;
        if (hasPipe)
        {
            si.dwFlags |= STARTF_USESTDHANDLES;
            si.hStdOutput = hWritePipe;
            si.hStdError = hWritePipe;
            si.hStdInput = nullptr;
        }
        PROCESS_INFORMATION pi{};

        auto success = CreateProcessW(
            nullptr,
            cmdline.data(),
            nullptr, nullptr, hasPipe ? TRUE : FALSE,
            CREATE_NO_WINDOW,
            nullptr, nullptr, &si, &pi);

        // Close the write end in the parent so ReadFile sees EOF
        // when the child exits.
        if (hWritePipe)
        {
            CloseHandle(hWritePipe);
            hWritePipe = nullptr;
        }

        if (!success)
        {
            _agentPaneLog("[FRE] winget CreateProcess failed: GetLastError=" + std::to_string(GetLastError()));
            if (hReadPipe) CloseHandle(hReadPipe);
            co_return false;
        }

        // Wait for the child process first, then drain any remaining
        // pipe output. This avoids the synchronous ReadFile blocking
        // indefinitely if winget spawns child processes that inherit
        // the pipe handle and outlive winget itself.
        WaitForSingleObject(pi.hProcess, 300000); // 5 min timeout

        // Drain pipe output (non-blocking — child has exited, so the
        // write end is closed and ReadFile will see EOF promptly).
        // Keep only the last ~500 bytes to cap memory usage.
        static constexpr size_t kMaxOutput = 500;
        std::string output;
        if (hasPipe && hReadPipe)
        {
            char buf[512];
            DWORD bytesRead = 0;
            while (ReadFile(hReadPipe, buf, sizeof(buf) - 1, &bytesRead, nullptr) && bytesRead > 0)
            {
                buf[bytesRead] = '\0';
                output += buf;
                // Keep only the tail
                if (output.size() > kMaxOutput * 2)
                    output = output.substr(output.size() - kMaxOutput);
            }
            CloseHandle(hReadPipe);
            hReadPipe = nullptr;
        }

        DWORD exitCode = 1;
        GetExitCodeProcess(pi.hProcess, &exitCode);
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);

        // Log the result — truncate output to avoid unbounded log growth.
        if (exitCode != 0)
        {
            // Trim trailing whitespace
            while (!output.empty() && (output.back() == '\n' || output.back() == '\r' || output.back() == ' '))
                output.pop_back();
            // Cap at 500 chars
            if (output.size() > 500)
                output = output.substr(output.size() - 500);
            _agentPaneLog("[FRE] winget exit=" + std::to_string(exitCode) + " output: " + output);
        }

        co_return exitCode == 0;
    }


    // ── Hooks install helper ────────────────────────────────────────────

    IAsyncOperation<bool> FreOverlay::_InstallHooksAsync(winrt::hstring agentId)
    {
        auto id = std::wstring{ agentId };

        co_await winrt::resume_background();

        namespace Wta = ::Microsoft::Terminal::WtaProcess;

        const auto wtaPath = Wta::ResolveWtaExePath();
        // Extend PATH so freshly-installed CLIs (e.g. copilot via winget)
        // are discoverable by the hooks installer.
        auto envBlock = Wta::BuildExtendedPathEnvBlock();
        auto args = L"hooks install --cli " + id;
        co_return Wta::RunWtaAndWait(wtaPath, args, 60'000,
                                     envBlock.empty() ? nullptr : envBlock.data());
    }

    // ── Save + install flow ─────────────────────────────────────────────

    // Surface a single blocking problem in the bottom-left error area and
    // apply its remediation. Only one problem is shown at a time so the layout
    // stays compact; each problem links to step-by-step manual-setup docs.
    void FreOverlay::_ShowProblem(FreProblemKind kind)
    {
        // Base doc; prerequisites and shell integration deep-link to a section.
        static constexpr std::wstring_view baseUrl{ L"https://aka.ms/intelligent-terminal-dependency" };

        std::wstring url{ baseUrl };

        // RS_ requires string literals (the resource keys are extracted at
        // build time), so set the message per-branch rather than via a
        // variable key.
        switch (kind)
        {
        case FreProblemKind::WingetMissing:
            ErrorText().Text(RS_(L"FreOverlay_InstallErrorWingetMissing"));
            url += L"#1-winget-windows-package-manager";
            break;
        case FreProblemKind::CopilotInstall:
            ErrorText().Text(RS_(L"FreOverlay_InstallErrorCopilot"));
            url += L"#31-github-copilot-cli";
            break;
        case FreProblemKind::NodeInstall:
            ErrorText().Text(RS_(L"FreOverlay_InstallErrorNode"));
            url += L"#2-nodejs-lts--shared-prerequisite";
            break;
        case FreProblemKind::ShellIntegrationExecutionPolicy:
            ErrorText().Text(RS_(L"FreOverlay_InstallErrorShellIntegrationExecutionPolicy"));
            url += L"#4-powershell-shell-integration";
            // Same remediation as generic shell-integration failure: turn
            // off error detection so the user can save and continue. Once
            // they fix execution policy they can re-enable it from Settings.
            AutoDetectToggle().IsOn(false);
            _UpdateSuggestionEnabledState();
            if (_settings)
            {
                _settings.GlobalSettings().AutoErrorDetectionEnabled(false);
                _settings.GlobalSettings().AutoFixEnabled(false);
            }
            break;
        case FreProblemKind::ShellIntegration:
            ErrorText().Text(RS_(L"FreOverlay_InstallErrorShellIntegration"));
            url += L"#4-powershell-shell-integration";
            // Remediation: turn off error detection (and its dependent
            // suggestion) so the user can save and continue without it.
            AutoDetectToggle().IsOn(false);
            _UpdateSuggestionEnabledState();
            if (_settings)
            {
                _settings.GlobalSettings().AutoErrorDetectionEnabled(false);
                _settings.GlobalSettings().AutoFixEnabled(false);
            }
            break;
        case FreProblemKind::Hooks:
            ErrorText().Text(RS_(L"FreOverlay_InstallErrorHooks"));
            url += L"#36-agent-hooks-for-session-management";
            // Remediation: turn off session management so the user can save and
            // continue without it.
            SessionManagementToggle().IsOn(false);
            break;
        }

        ErrorHelpRun().Text(RS_(L"FreOverlay_ErrorHelpLink"));
        ErrorHelpLink().NavigateUri(Uri{ winrt::hstring{ url } });
        ErrorPanel().Visibility(Visibility::Visible);

        // Refresh the agent dropdown so its status labels reflect what actually
        // got installed during this attempt. A prerequisite may have succeeded
        // before a later step failed (e.g. Copilot installed but hooks failed),
        // so flip "(will install)" → "(installed)" for anything now on PATH.
        _PopulateAgentComboBox();

        // Narrator: order matters. Fire the error notification FIRST,
        // BEFORE any focus transitions, so the assertive announcement
        // is queued before the Save-button focus event that
        // _SetSavingState(false) emits below. Without this ordering,
        // some Narrator versions announce "Save button" first and the
        // actual error message sounds like an afterthought.
        //
        // The ErrorText carries LiveSetting="Assertive" in XAML, but
        // live regions don't fire reliably for Text changes that
        // happen while the hosting element is still Collapsed (we set
        // the text above before flipping Visibility). Uses SaveButton
        // as the peer source (matches the FRE welcome pattern in
        // TerminalPage::_ShowFreOverlay) because UserControl peers
        // don't propagate notifications to Narrator reliably.
        if (auto peer = Automation::Peers::FrameworkElementAutomationPeer::FromElement(SaveButton()))
        {
            peer.RaiseNotificationEvent(
                Automation::Peers::AutomationNotificationKind::Other,
                Automation::Peers::AutomationNotificationProcessing::ImportantMostRecent,
                ErrorText().Text(),
                L"FreInstallErrorAnnouncement");
        }

        // Re-enable editing so the user can adjust selections and retry.
        // _SetSavingState(false) parks focus on SaveButton as its safe
        // default — fine for the success path (where the overlay
        // immediately collapses) but suboptimal here: a Narrator user
        // would be told the error and then find their focus on the
        // generic Save button, with no clear cue that they're "on the
        // error" or what they can do about it. Override the focus to
        // the help-link inside the ErrorPanel — it's the only
        // actionable element in the error area, and pressing Enter
        // there opens the manual-fix docs (the natural next action
        // after hearing the error). The user can Shift+Tab back to
        // SaveButton if they want to retry instead.
        _SetSavingState(false);
        ErrorHelpLink().Focus(FocusState::Programmatic);
    }

    IAsyncAction FreOverlay::_SaveAndInstallAsync()
    {
        auto weak = get_weak();

        // 1. Read selections on the UI thread
        winrt::hstring agentId;
        if (const auto selected = AgentComboBox().SelectedItem())
        {
            if (const auto entry = selected.try_as<winrt::TerminalApp::FreAgentEntry>())
            {
                agentId = entry.Id();
            }
        }

        if (_settings)
        {
            const auto& globals = _settings.GlobalSettings();
            globals.AcpAgent(agentId);
            globals.DelegateAgent(agentId);
            globals.AutoErrorDetectionEnabled(AutoDetectToggle().IsOn());
            globals.AutoFixEnabled(AutoErrorToggle().IsOn());

            const auto posIdx = PanePositionComboBox().SelectedIndex();
            switch (posIdx)
            {
            case 1: globals.AgentPanePosition(L"right"); break;
            case 2: globals.AgentPanePosition(L"left"); break;
            case 3: globals.AgentPanePosition(L"top"); break;
            default: globals.AgentPanePosition(L"bottom"); break;
            }
        }

        // 2. Enter the "saving" state: disable the form, raise the
        // SavingOverlay (with spinner + "Setting up..."), disable the
        // Save button. Hide any previous error.
        _SetSavingState(true);
        ErrorPanel().Visibility(Visibility::Collapsed);

        // 3. Install prerequisites if needed (blocking — cannot proceed without these)
        const bool needsCopilot = (agentId == L"copilot") && !_IsAgentInstalled(L"copilot");
        const bool needsNode = (agentId == L"claude" || agentId == L"codex") && !_IsNodeInstalled();

        _agentPaneLog("[FRE] Save: agent=" + winrt::to_string(agentId)
            + " needsCopilot=" + (needsCopilot ? "y" : "n")
            + " needsNode=" + (needsNode ? "y" : "n")
            + " detect=" + (AutoDetectToggle().IsOn() ? "on" : "off")
            + " suggest=" + (AutoErrorToggle().IsOn() ? "on" : "off")
            + " hooks=" + (SessionManagementToggle().IsOn() ? "on" : "off"));

        // If any bootstrap step needs winget, make sure winget itself is
        // available before kicking off the install — otherwise the user
        // gets a generic "install failed" error that wrongly points at
        // the package's docs instead of the winget setup docs.
        if (needsCopilot || needsNode)
        {
            if (!_IsWingetInstalled())
            {
                _agentPaneLog("[FRE] winget not found on PATH");
                _ShowProblem(FreProblemKind::WingetMissing);
                co_return;
            }
        }

        if (needsCopilot)
        {
            _agentPaneLog("[FRE] Installing GitHub.Copilot via winget");
            bool ok = co_await _WingetInstallAsync(L"GitHub.Copilot");
            // Helper internally does co_await winrt::resume_background(),
            // so the continuation may resume on a thread-pool thread.
            // Hop back to the UI thread before any XAML access (the
            // _ShowProblem call below touches ErrorText / ErrorPanel /
            // toggles); without this, RPC_E_WRONG_THREAD is thrown and
            // silently swallowed by IAsyncAction, leaving the
            // SavingOverlay stuck.
            co_await winrt::resume_foreground(Dispatcher());
            auto self = weak.get();
            if (!self) co_return;
            _agentPaneLog("[FRE] Copilot install: " + std::string(ok ? "ok" : "FAILED"));
            if (!ok)
            {
                _ShowProblem(FreProblemKind::CopilotInstall);
                co_return;
            }
        }
        if (needsNode)
        {
            _agentPaneLog("[FRE] Installing Node.js via winget");
            bool ok = co_await _WingetInstallAsync(L"OpenJS.NodeJS.LTS");
            // See note above for the Copilot install — same threading
            // concern applies here.
            co_await winrt::resume_foreground(Dispatcher());
            auto self = weak.get();
            if (!self) co_return;
            _agentPaneLog("[FRE] Node.js install: " + std::string(ok ? "ok" : "FAILED"));
            if (!ok)
            {
                _ShowProblem(FreProblemKind::NodeInstall);
                co_return;
            }
        }

        // After installing prerequisites, refresh the current process's
        // PATH from the Windows registry so SearchPathW (used by
        // _DetectAgentCli, Settings UI, etc.) can find freshly-installed
        // CLIs without restarting Terminal.
        if (needsCopilot || needsNode)
        {
            _agentPaneLog("[FRE] Refreshing process PATH from registry");
            try
            {
                ::Microsoft::Terminal::WtaProcess::RefreshProcessPath();

                // Verify WinGet\Links is now on PATH
                wchar_t localAppData[MAX_PATH]{};
                GetEnvironmentVariableW(L"LOCALAPPDATA", localAppData, MAX_PATH);
                if (localAppData[0])
                {
                    auto wingetLinks = std::wstring(localAppData) + L"\\Microsoft\\WinGet\\Links";
                    wchar_t pathBuf[32767]{};
                    GetEnvironmentVariableW(L"PATH", pathBuf, 32767);
                    std::wstring path{ pathBuf };
                    bool hasLinks = (path.find(wingetLinks) != std::wstring::npos);
                    _agentPaneLog("[FRE] PATH after refresh: WinGet\\Links " + std::string(hasLinks ? "present" : "MISSING"));
                }
            }
            catch (...)
            {
                _agentPaneLog("[FRE] RefreshProcessPath threw an exception");
                LOG_CAUGHT_EXCEPTION();
            }
        }

        // 4+5. Install hooks and shell integration. Run both, collect any
        // failures, then surface only the highest-priority one (see
        // _ShowProblem). Lower-priority failures are left enabled so the next
        // Save retries them.
        bool hooksFailed = false;
        bool shellIntegFailed = false;
        bool shellIntegEpBlocked = false;

        // 4. Hooks — skip if GPO blocks it or settings unavailable.
        if (SessionManagementToggle().IsOn() &&
            _settings &&
            !_settings.GlobalSettings().IsAgentSessionHooksPolicyLocked())
        {
            auto self = weak.get();
            if (!self) co_return;

            _agentPaneLog("[FRE] Installing hooks for " + winrt::to_string(agentId));
            bool hooksOk = co_await _InstallHooksAsync(agentId);
            // Helper internally does co_await winrt::resume_background(),
            // so the continuation may resume on a thread-pool thread.
            // Hop back to the UI thread before the subsequent
            // AutoDetectToggle().IsOn() read and any later _ShowProblem
            // call. Without this, XAML access from the thread pool
            // throws RPC_E_WRONG_THREAD, which IAsyncAction swallows —
            // the SavingOverlay would then be stuck with no error
            // surfaced.
            co_await winrt::resume_foreground(Dispatcher());
            self = weak.get();
            if (!self) co_return;

            _agentPaneLog("[FRE] Hooks install: " + std::string(hooksOk ? "ok" : "FAILED"));
            if (!hooksOk)
            {
                hooksFailed = true;
            }
        }

        // 5. Shell integration — only when error detection is enabled.
        if (AutoDetectToggle().IsOn())
        {
            auto self = weak.get();
            if (!self) co_return;

            _agentPaneLog("[FRE] Installing shell integration");
            co_await winrt::resume_background();
            namespace SI = ::Microsoft::Terminal::ShellIntegration;
            const auto pwsh7Result = SI::InstallForTarget(SI::Target::Pwsh);
            const auto windowsPsResult = SI::InstallForTarget(SI::Target::WindowsPowerShell);

            {
                std::string detail = "[FRE] Shell integration: pwsh7=";
                detail += pwsh7Result.success ? "ok" : "FAILED";
                if (!pwsh7Result.success && !pwsh7Result.errorMessage.empty())
                    detail += " (" + winrt::to_string(winrt::hstring{ pwsh7Result.errorMessage }) + ")";
                detail += " winPs=";
                detail += windowsPsResult.success ? "ok" : "FAILED";
                if (!windowsPsResult.success && !windowsPsResult.errorMessage.empty())
                    detail += " (" + winrt::to_string(winrt::hstring{ windowsPsResult.errorMessage }) + ")";
                _agentPaneLog(detail);
            }

            if (!pwsh7Result.success || !windowsPsResult.success)
            {
                shellIntegFailed = true;
                // If either host's failure was specifically the execution
                // policy, surface the policy-specific message instead of the
                // generic write-failed one. The user needs different
                // remediation (Set-ExecutionPolicy / GPO) vs. a transient
                // file write failure.
                if (pwsh7Result.executionPolicyBlocked || windowsPsResult.executionPolicyBlocked)
                {
                    shellIntegEpBlocked = true;
                }
            }
        }

        // Surface only the highest-priority failure. Shell integration outranks
        // hooks; the unshown failure stays enabled and is retried on next Save.
        if (hooksFailed || shellIntegFailed)
        {
            _agentPaneLog("[FRE] Showing problem: "
                + std::string(shellIntegFailed ? "ShellIntegration" : "Hooks"));
            co_await winrt::resume_foreground(Dispatcher());
            auto self = weak.get();
            if (!self) co_return;

            _ShowProblem(shellIntegEpBlocked ? FreProblemKind::ShellIntegrationExecutionPolicy
                                             : shellIntegFailed ? FreProblemKind::ShellIntegration
                                                                : FreProblemKind::Hooks);
            co_return;
        }

        // 6. Resume UI thread before touching controls / raising events
        co_await winrt::resume_foreground(Dispatcher());
        {
            auto self = weak.get();
            if (!self) co_return;

            // Refresh the agent dropdown so any agent we just installed (e.g.
            // Copilot via winget) now shows "(installed)" instead of
            // "(will install)" — confirms the install actually landed.
            _PopulateAgentComboBox();

            _agentPaneLog("[FRE] Completed — raising Completed event");
            // Restore the editable state before raising Completed so that
            // if anything keeps the overlay alive a moment longer, it
            // doesn't appear stuck in the "saving" visual.
            _SetSavingState(false);
            Completed.raise(*this, nullptr);
        }
    }

    // ── Button handlers ─────────────────────────────────────────────────

    void FreOverlay::_OnSaveButtonClick(const IInspectable& /*sender*/,
                                        const RoutedEventArgs& /*args*/)
    {
        _SaveAndInstallAsync();
    }

    void FreOverlay::_OnCloseButtonClick(const IInspectable& /*sender*/,
                                         const RoutedEventArgs& /*args*/)
    {
        Completed.raise(*this, nullptr);
    }

    // ── No-op: kept for IDL compatibility ───────────────────────────────

    void FreOverlay::ResetDragOffset()
    {
    }

    // ── Saving state ────────────────────────────────────────────────────

    // Toggle the overlay between "saving / installing" and "idle / editable".
    //
    // - The settings ScrollViewer is disabled as a group while saving.
    //   IsEnabled on an ancestor propagates an "effectively disabled"
    //   state to descendants (it ANDs with each child's own IsEnabled)
    //   without clobbering the per-control IsEnabled values, so
    //   policy-driven disables (locked toggles, etc.) survive when we
    //   restore. Crucially, IsEnabled blocks keyboard input too —
    //   unlike IsHitTestVisible, which is pointer-only and would leave
    //   Tab / Space / arrows working on the form mid-install.
    // - The SavingOverlay (a semi-opaque Border sitting in the same
    //   Grid cell as the form, z-stacked on top) gives the visual: a
    //   centered ProgressRing + "Setting up..." status text. Its
    //   Background also catches any stray pointer input the disabled
    //   form might still surface.
    // - The Save button is gated separately so an Enter keypress can't
    //   re-fire the click while we're already saving.
    void FreOverlay::_SetSavingState(bool saving)
    {
        _agentPaneLog(std::string("[FRE] saving state: ") + (saving ? "ON" : "OFF"));

        // Guard against being called before InitializeComponent has populated
        // the named XAML elements — matches the pattern used elsewhere in
        // this file (see _UpdateSuggestionEnabledState, _OnAutoDetectToggled).
        auto scroller = SettingsFormScroller();
        auto overlay = SavingOverlay();
        auto ring = SavingProgressRing();
        auto save = SaveButton();
        if (!scroller || !overlay || !ring || !save)
        {
            return;
        }

        // Saving-state transition note. Order intentionally accepts
        // two focus changes in close succession on entry:
        //   (1) save.IsEnabled(false) below evicts focus from the
        //       SaveButton (the user just clicked Save, so focus was
        //       on it). XAML auto-moves focus to the next available
        //       tab stop; with the form also disabled, that's
        //       effectively "nowhere".
        //   (2) The deferred Dispatcher().RunAsync(Low) further down
        //       moves focus to the ProgressRing once layout settles
        //       (~one frame later — calling Focus() inline right
        //       after overlay.Visibility(Visible) silently fails
        //       because the ring isn't in the live visual tree yet).
        // The RaiseNotificationEvent ("Setting up...") fires
        // synchronously with ImportantMostRecent priority and
        // preempts the brief between-state focus eviction in
        // Narrator, so the user hears the operation name rather
        // than noise. We deliberately do NOT defer save.IsEnabled
        // alongside the focus call: that would create a window
        // where the user could re-click Save mid-install.
        //
        // On the way back (saving=false), we re-enable scroller and
        // save before calling save.Focus, so the focus target is
        // already enabled when XAML lands it.
        if (saving)
        {
            overlay.Visibility(Visibility::Visible);
            ring.IsActive(true);
            scroller.IsEnabled(false);
            save.IsEnabled(false);

            // Move focus to the ProgressRing AFTER the synchronous
            // layout work completes. Calling ring.Focus() right after
            // overlay.Visibility(Visible) silently fails — the ring
            // isn't in the live visual tree yet, Focus() returns false
            // (we don't see it because the return value is discarded),
            // and focus stays on the SaveButton the user just clicked.
            // Mirror the deferred-focus pattern used in
            // TerminalPage::_ShowFreOverlay (line ~955) for the FRE
            // NextButton: dispatch at Low priority so the focus call
            // runs after the visibility change has been laid out.
            //
            // Guard against the fast-success / fast-error race: on a
            // very quick install the synchronous flow can call
            // _SetSavingState(false) — collapsing the overlay — before
            // this deferred lambda fires. Re-check Visibility inside
            // the lambda so we don't pull focus back to a hidden ring.
            Dispatcher().RunAsync(
                winrt::Windows::UI::Core::CoreDispatcherPriority::Low,
                [weak = get_weak()]() {
                    auto self = weak.get();
                    if (!self) { return; }
                    auto o = self->SavingOverlay();
                    if (!o || o.Visibility() != Visibility::Visible) { return; }
                    if (auto r = self->SavingProgressRing())
                    {
                        r.Focus(FocusState::Programmatic);
                    }
                });

            // Narrator: the deferred focus above will eventually fire a
            // focus event with the ProgressRing's Name ("Setting up
            // Intelligent Terminal", set in Initialize via SetName) +
            // its "busy" state. RaiseNotificationEvent ensures the
            // user hears something immediately, before that deferred
            // focus lands. Together: an early notification on entry,
            // and a meaningful Caps+Tab readout (or focus-changed
            // announcement on re-entry) once focus is parked on the
            // ring. Uses SaveButton as the peer source (matches the
            // FRE welcome pattern in TerminalPage::_ShowFreOverlay)
            // because UserControl peers don't propagate notifications
            // to Narrator reliably; a concrete focusable Control does.
            if (auto peer = Automation::Peers::FrameworkElementAutomationPeer::FromElement(SaveButton()))
            {
                peer.RaiseNotificationEvent(
                    Automation::Peers::AutomationNotificationKind::Other,
                    Automation::Peers::AutomationNotificationProcessing::ImportantMostRecent,
                    RS_(L"FreOverlay_SettingUp"),
                    L"FreSavingAnnouncement");
            }
        }
        else
        {
            scroller.IsEnabled(true);
            save.IsEnabled(true);
            overlay.Visibility(Visibility::Collapsed);
            ring.IsActive(false);
            // Park focus on Save so a keyboard user (typically after an
            // error, where the form is re-enabled but ErrorPanel now
            // shows) can press Enter to retry without a mouse trip.
            save.Focus(FocusState::Programmatic);
        }
    }
}
