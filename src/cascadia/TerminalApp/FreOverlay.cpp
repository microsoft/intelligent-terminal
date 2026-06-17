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
#include "ShellIntegrationSweep.h"
#include "WindowsPackageManagerFactory.h"

#include <winrt/Windows.UI.Xaml.Documents.h>
#include <mutex>

using namespace winrt::Windows::Foundation;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;
using namespace winrt::Windows::UI::Xaml::Documents;
namespace Automation = winrt::Windows::UI::Xaml::Automation;

namespace winrt::TerminalApp::implementation
{
    // ── Static prewarm state (single-flight per process) ────────────
    // See FreOverlay.h for the design contract. Definitions live here
    // because C++ requires out-of-line definitions for non-inline static
    // class members.
    std::mutex FreOverlay::s_prewarmMutex;
    winrt::Windows::Foundation::IAsyncAction FreOverlay::s_prewarmAction{ nullptr };

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

        // ── Pre-warm winget source cache ───────────────────────────────
        // While the user reads the Welcome + Settings pages (typically
        // 5-30s), pre-warm winget's source manifest cache so the on-Save
        // install skips the slow refresh step. Best-effort, no error UI.
        // Save will await any in-flight prewarm before its own winget call
        // to keep the two operations serialised.
        _MaybeStartPrewarm(
            /*copilotMissing*/ !_IsAgentInstalled(L"copilot"),
            /*nodeMissing*/ !_IsNodeInstalled());
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

    // ── WinGet source pre-warm ──────────────────────────────────────────
    //
    // Kick off `winget source update --name winget` in the background as
    // soon as the FRE overlay is shown, so that the on-Save `winget install`
    // sees a warm source manifest cache and skips the 3-20s refresh step.
    // Gated on whether the install would actually run (Copilot or Node
    // missing) AND winget being available. Single-flight per process —
    // reentrant Initialize() calls and multi-window FRE coalesce onto
    // one running prewarm. The Save handler awaits s_prewarmAction before
    // its own winget call (see _SaveAndInstallAsync); in practice the
    // two winget operations never run concurrently. Exception: if
    // _RunPrewarmAsync hits its 120s timeout, it returns while the
    // underlying `winget source update` may still be running in the
    // background. We accept this tradeoff because killing winget
    // mid-write risks corrupting its source DB, and 120s timeouts are
    // very rare in practice. A future migration of the prewarm to the
    // COM `RefreshPackageCatalogAsync` API would eliminate this race
    // entirely.

    void FreOverlay::_MaybeStartPrewarm(bool copilotMissing, bool nodeMissing)
    {
        // Gate: nothing to pre-warm if no winget install step will run.
        if (!copilotMissing && !nodeMissing)
        {
            return;
        }
        if (!_IsWingetInstalled())
        {
            return;
        }

        // Single-flight: first caller wins; later callers find the
        // existing IAsyncAction in the slot and bail out.
        std::lock_guard<std::mutex> lock{ s_prewarmMutex };
        if (s_prewarmAction)
        {
            return;
        }
        // _RunPrewarmAsync starts on the calling thread, hops to background
        // at its first co_await, and returns the IAsyncAction handle here
        // for storage and later co_await by Save.
        s_prewarmAction = _RunPrewarmAsync();
    }

    winrt::Windows::Foundation::IAsyncAction FreOverlay::_RunPrewarmAsync()
    {
        // Hop to background — must never block the UI thread.
        co_await winrt::resume_background();

        try
        {
            _agentPaneLog("[FRE] Pre-warm: winget source update --name winget");

            STARTUPINFOW si{};
            si.cb = sizeof(si);
            si.dwFlags = STARTF_USESHOWWINDOW;
            si.wShowWindow = SW_HIDE;
            PROCESS_INFORMATION pi{};

            // CreateProcessW requires a *writable* cmdline buffer (it may
            // mutate the string in-place when parsing). `--disable-interactivity`
            // prevents any prompt (e.g. source first-run agreement) from
            // hanging the hidden child process forever.
            wchar_t cmdline[] = L"winget source update --name winget --disable-interactivity";
            if (!CreateProcessW(nullptr, cmdline, nullptr, nullptr, FALSE,
                                CREATE_NO_WINDOW, nullptr, nullptr, &si, &pi))
            {
                _agentPaneLog("[FRE] Pre-warm: CreateProcess failed err="
                              + std::to_string(GetLastError()));
                co_return;
            }

            // Wait up to 120s. Corporate proxies / cold caches can push
            // honest cases past 30s, so we err on the side of patience.
            // We deliberately do NOT TerminateProcess on timeout: killing
            // winget mid-write can corrupt its source DB. The Save handler
            // awaits this whole coroutine, which only completes after
            // WaitForSingleObject returns, so even a slow prewarm cannot
            // collide with the eventual install.
            const DWORD wait = WaitForSingleObject(pi.hProcess, 120000);
            DWORD exitCode = 0;
            GetExitCodeProcess(pi.hProcess, &exitCode);
            CloseHandle(pi.hProcess);
            CloseHandle(pi.hThread);

            _agentPaneLog(wait == WAIT_TIMEOUT
                              ? "[FRE] Pre-warm: still running after 120s (proceeding)"
                              : "[FRE] Pre-warm: completed exit=" + std::to_string(exitCode));
        }
        catch (...)
        {
            // Pre-warm is strictly best-effort; never let an exception
            // escape into the IAsyncAction promise. Save's co_await on
            // s_prewarmAction also has its own try/catch as belt-and-
            // suspenders, but this is the primary guard.
            LOG_CAUGHT_EXCEPTION();
        }
    }

    // ── WinGet install helper ───────────────────────────────────────────
    //
    // Installs a package via the WinGet COM/WinRT API
    // (`Microsoft.Management.Deployment.PackageManager`) instead of
    // shelling out to `winget.exe`.
    //
    // Why this matters:
    //
    // The CLI path doesn't work for us. winget.exe, when launched from
    // a packaged GUI parent (which IT is), runs through an App Execution
    // Alias activation that breaks stdio inheritance — child writes
    // nothing to whatever pipe/file we redirect to. We verified across
    // 6 spawn variants (NUL stdin, pipe stdin, GetStdHandle stdin,
    // combined vs split pipes, plus `cmd.exe /c "winget … > tempfile 2>&1"`)
    // that the captured output is consistently 0 bytes for real failures.
    // This is a Microsoft design limitation, not a winget bug:
    // packaged-from-packaged stdio inheritance is documented as unsupported
    // (see https://github.com/microsoft/winget-cli/issues/504).
    //
    // The COM API bypasses the alias activation entirely — it calls
    // AppInstaller's out-of-proc COM server directly via CoCreateInstance
    // (CLSCTX_LOCAL_SERVER, no child process spawn). We get back a
    // structured InstallResult with InstallResultStatus, ExtendedErrorCode
    // (the same HRESULT that the CLI would have printed), and
    // InstallerErrorCode — far better diagnostics than the CLI ever gave
    // us, and reliably available from packaged context.

    namespace
    {
        using namespace winrt::Microsoft::Management::Deployment;

        // Enum-to-string helpers — log values are human-readable instead
        // of bare ints, so anyone reading the log can grep the winmd /
        // PackageManager.idl directly without an enum reference table.

        constexpr const char* ConnectStatusName(ConnectResultStatus s) noexcept
        {
            switch (s)
            {
            case ConnectResultStatus::Ok: return "Ok";
            case ConnectResultStatus::CatalogError: return "CatalogError";
            case ConnectResultStatus::SourceAgreementsNotAccepted: return "SourceAgreementsNotAccepted";
            default: return "Unknown";
            }
        }

        constexpr const char* FindStatusName(FindPackagesResultStatus s) noexcept
        {
            switch (s)
            {
            case FindPackagesResultStatus::Ok: return "Ok";
            case FindPackagesResultStatus::BlockedByPolicy: return "BlockedByPolicy";
            case FindPackagesResultStatus::CatalogError: return "CatalogError";
            case FindPackagesResultStatus::InvalidOptions: return "InvalidOptions";
            case FindPackagesResultStatus::InternalError: return "InternalError";
            default: return "Unknown";
            }
        }

        constexpr const char* InstallStatusName(InstallResultStatus s) noexcept
        {
            switch (s)
            {
            case InstallResultStatus::Ok: return "Ok";
            case InstallResultStatus::BlockedByPolicy: return "BlockedByPolicy";
            case InstallResultStatus::CatalogError: return "CatalogError";
            case InstallResultStatus::InternalError: return "InternalError";
            case InstallResultStatus::InvalidOptions: return "InvalidOptions";
            case InstallResultStatus::DownloadError: return "DownloadError";
            case InstallResultStatus::InstallError: return "InstallError";
            case InstallResultStatus::ManifestError: return "ManifestError";
            case InstallResultStatus::NoApplicableInstallers: return "NoApplicableInstallers";
            case InstallResultStatus::NoApplicableUpgrade: return "NoApplicableUpgrade";
            case InstallResultStatus::PackageAgreementsNotAccepted: return "PackageAgreementsNotAccepted";
            default: return "Unknown";
            }
        }

        // Copy winget's own diagnostic logs from AppInstaller's DiagOutputDir
        // into our per-version `winget\` subfolder, so the bug-report zip
        // picks them up alongside `terminal-agent-pane.log`.
        //
        // Why this matters: our [FRE] log only records the final
        // InstallResultStatus + HRESULT + InstallerErrorCode. The winget
        // COM API internally writes a much more detailed trace (HTTP
        // request URLs, retry attempts, hash/signature verification,
        // installer-exec arguments, MSI verbose output) to its own log
        // files. When `winget install GitHub.Copilot` fails on a user's
        // box, that detailed trace is what tells us *why* — without it,
        // bug reports come down to "install failed, here's a generic
        // HRESULT". Colocating the logs gives us triage-grade diagnostics.
        //
        // Filtering: anything `.log` modified at or after `since` in the
        // DiagOutputDir is copied. We avoid filename-prefix assumptions
        // (winget has historically used `WinGet-*.log`, but a future
        // release could add `WinGetCOM-*.log` or similar and we'd
        // silently miss it).
        //
        // Defensive size caps: skip any single file larger than
        // kPerFileCapBytes and abort the loop once kTotalCapBytes have
        // been copied. Without these, a stale clock, an aggressive
        // verbose-MSI run, or unrelated concurrent winget activity could
        // dump tens of MB into our log folder.
        //
        // DiagOutputDir path is `Microsoft.DesktopAppInstaller_8wekyb3d8bbwe\`
        // hardcoded. This package family name has been stable for 5+
        // years; if it ever moves, the helper logs "DiagOutputDir not
        // found" and returns — no exception bubbles to the install
        // coroutine.
        //
        // Timeout caveat: when the install path hits our 20-min hard
        // cap, winget's underlying installer may still be running when
        // we copy its log. The captured file may therefore be
        // truncated (missing the very last entries). We do not delay
        // copy on timeout because winget can keep writing for an
        // arbitrary additional time — a bounded sleep would not
        // reliably get the "final" log, just the "slightly less
        // truncated" one. Engineers needing the absolutely final
        // contents can grab the source files from DiagOutputDir
        // directly post-mortem.
        //
        // noexcept-from-caller: every failure mode (env var missing,
        // ACL deny, disk full, file lock race) is swallowed with a log
        // line. The install flow never sees an exception from here.
        static void _CopyWingetLogsSince(std::filesystem::file_time_type since) noexcept
        {
            try
            {
                // Defensive caps. Per-file cap protects against a single
                // verbose-MSI log eating our disk; total cap is the
                // ceiling across all files in this capture.
                constexpr std::uintmax_t kPerFileCapBytes = 25ULL * 1024ULL * 1024ULL;  // 25 MB
                constexpr std::uintmax_t kTotalCapBytes = 50ULL * 1024ULL * 1024ULL;    // 50 MB

                wchar_t localAppData[MAX_PATH]{};
                const DWORD lenWritten =
                    GetEnvironmentVariableW(L"LOCALAPPDATA", localAppData, MAX_PATH);
                // Treat both "missing" (0) and "would have truncated"
                // (>= MAX_PATH) as "give up" — a multi-thousand-char
                // %LOCALAPPDATA% is unusual enough that capturing logs
                // for that user isn't worth the extra alloc dance.
                if (lenWritten == 0 || lenWritten >= MAX_PATH)
                {
                    return;
                }
                const std::filesystem::path diagDir =
                    std::filesystem::path{ localAppData } /
                    L"Packages" /
                    L"Microsoft.DesktopAppInstaller_8wekyb3d8bbwe" /
                    L"LocalState" /
                    L"DiagOutputDir";

                std::error_code ec;
                if (!std::filesystem::exists(diagDir, ec) || ec)
                {
                    _agentPaneLog("[FRE] winget DiagOutputDir not present, skipping log capture");
                    return;
                }

                const auto destDir = ::IntelligentTerminal::LogDirVersioned() / L"winget";
                std::filesystem::create_directories(destDir, ec);
                if (ec)
                {
                    return;
                }

                int copied = 0;
                int skipped = 0;
                std::uintmax_t totalBytes = 0;

                // Iterate explicitly with `increment(ec)` instead of
                // range-for: the latter's `operator++` can throw on
                // filesystem races (file deleted mid-scan, antivirus
                // contention), which would unwind out of our noexcept
                // contract via the outer catch. Explicit increment lets
                // us treat every iteration step as a soft failure that
                // skips the entry and continues.
                std::filesystem::directory_iterator it{ diagDir, ec };
                const std::filesystem::directory_iterator end{};
                if (ec)
                {
                    _agentPaneLog(fmt::format(
                        "[FRE] winget log capture: failed to open DiagOutputDir ({})",
                        ec.message()));
                    return;
                }
                while (it != end)
                {
                    const auto entryPath = it->path();
                    bool entryHandled = false;

                    if (it->is_regular_file(ec) && !ec &&
                        entryPath.extension() == L".log")
                    {
                        const auto mtime = std::filesystem::last_write_time(entryPath, ec);
                        const auto fileSize = !ec ? std::filesystem::file_size(entryPath, ec) : 0;
                        if (!ec && mtime >= since)
                        {
                            if (fileSize > kPerFileCapBytes)
                            {
                                _agentPaneLog(fmt::format(
                                    "[FRE] winget log capture: skipping {} (size {} > per-file cap {})",
                                    winrt::to_string(entryPath.filename().wstring()),
                                    fileSize,
                                    kPerFileCapBytes));
                                ++skipped;
                                entryHandled = true;
                            }
                            else if (totalBytes + fileSize > kTotalCapBytes)
                            {
                                _agentPaneLog(fmt::format(
                                    "[FRE] winget log capture: total cap {} reached after {} files; stopping",
                                    kTotalCapBytes,
                                    copied));
                                break;
                            }
                            else
                            {
                                std::filesystem::copy_file(
                                    entryPath,
                                    destDir / entryPath.filename(),
                                    std::filesystem::copy_options::overwrite_existing,
                                    ec);
                                if (ec)
                                {
                                    ++skipped;
                                }
                                else
                                {
                                    ++copied;
                                    totalBytes += fileSize;
                                }
                                entryHandled = true;
                            }
                        }
                    }
                    (void)entryHandled;
                    ec.clear();

                    it.increment(ec);
                    if (ec)
                    {
                        // Soft-stop on iterator failure — better to
                        // report partial capture than to throw.
                        _agentPaneLog(fmt::format(
                            "[FRE] winget log capture: iterator error ({}); stopping early",
                            ec.message()));
                        break;
                    }
                }

                _agentPaneLog(fmt::format(
                    "[FRE] winget log capture: copied={} skipped={} bytes={} dest={}",
                    copied,
                    skipped,
                    totalBytes,
                    winrt::to_string(destDir.wstring())));
            }
            catch (...)
            {
                LOG_CAUGHT_EXCEPTION();
            }
        }
    }

    IAsyncOperation<int32_t> FreOverlay::_WingetInstallAsync(winrt::hstring packageId)
    {
        using namespace winrt::Microsoft::Management::Deployment;
        using Kind = FreWingetFailureKind;

        // Capture a weak reference so writes to _lastWinget* are safe even
        // if the overlay is destroyed mid-await (e.g. user dismissed the
        // window during a long install).
        auto weak = get_weak();

        // Snapshot the install start time before any winget work. Used as
        // the cutoff for the post-install DiagOutputDir log capture so we
        // pick up every log winget produced for this attempt — including
        // long-running install logs that started 10+ minutes ago — and not
        // logs from unrelated prior winget activity.
        const auto installStartTime = std::filesystem::file_time_type::clock::now();

        // Helper: persist diagnostic state to the instance (so the caller
        // can read it after our co_return), capture any fresh winget logs
        // from AppInstaller's DiagOutputDir on failure paths, and return
        // the encoded kind. Called from EVERY co_return — the
        // success/failure branch lives inside.
        //
        // Why only-on-failure for the log copy: the bug-report use case
        // for the colocated winget logs is "install failed, we need
        // triage details". Successful installs don't need the extra
        // logs, and copying them anyway would (a) waste a few MB of
        // disk per FRE run for no benefit, and (b) silently include any
        // unrelated concurrent winget activity captured by the mtime
        // window into the bug-report zip (e.g. URLs to private package
        // sources the user happened to be browsing in another shell).
        auto finish = [&weak, installStartTime](Kind k, int32_t hr, uint32_t installerErr) {
            if (auto self = weak.get())
            {
                self->_lastWingetHr = hr;
                self->_lastWingetInstallerErrorCode = installerErr;
            }
            if (k != Kind::Success)
            {
                _CopyWingetLogsSince(installStartTime);
            }
            return static_cast<int32_t>(k);
        };

        // Copy packageId before switching threads (coroutine parameter safety)
        auto id = winrt::hstring{ packageId };

        // Local diagnostic state. Written to the instance fields only via
        // `finish(...)` immediately before each co_return, so the caller
        // always sees consistent (kind, hr, installerErr) tuples and a
        // stale value never leaks across calls.
        int32_t hr = 0;
        uint32_t installerErr = 0;

        co_await winrt::resume_background();

        try
        {
            // ── 1. Activate the out-of-proc PackageManager COM server ──
            const PackageManager pm = WindowsPackageManagerFactory::CreatePackageManager();

            // ── 2. Connect to the winget catalog ──
            // Mirror the pattern used by `TerminalPage._FindPackageAsync`:
            // up to 3 attempts to absorb transient connection flakes.
            // Set AcceptSourceAgreements(true) — equivalent to the CLI's
            // --accept-source-agreements; without this, first-time winget
            // users (no prior agreement acceptance recorded) would hit
            // SourceAgreementsNotAccepted and be unable to install.
            auto catalogRef = pm.GetPredefinedPackageCatalog(PredefinedPackageCatalog::OpenWindowsCatalog);
            catalogRef.AcceptSourceAgreements(true);

            ConnectResult connectResult{ nullptr };
            for (int attempt = 0; attempt < 3; ++attempt)
            {
                connectResult = catalogRef.Connect();
                if (connectResult.Status() == ConnectResultStatus::Ok)
                {
                    break;
                }
            }
            if (connectResult.Status() != ConnectResultStatus::Ok)
            {
                _agentPaneLog(fmt::format(
                    "[FRE] winget catalog connect failed: {} (status={})",
                    ConnectStatusName(connectResult.Status()),
                    static_cast<int>(connectResult.Status())));
                // CatalogError during connect almost always means we
                // couldn't reach the catalog server (DNS / TLS / proxy /
                // firewall). The 1.8 contract doesn't expose
                // ConnectResult.ExtendedErrorCode so we can't whitelist
                // here — treat the catalog-error case as Network and
                // anything else (SourceAgreementsNotAccepted, future
                // statuses) as Generic.
                co_return finish(connectResult.Status() == ConnectResultStatus::CatalogError
                                     ? Kind::Network
                                     : Kind::Generic,
                                 hr, installerErr);
            }

            // ── 3. Find the package by exact ID ──
            auto filter = WindowsPackageManagerFactory::CreatePackageMatchFilter();
            filter.Field(PackageMatchField::Id);
            filter.Option(PackageFieldMatchOption::Equals);
            filter.Value(id);

            auto findOpts = WindowsPackageManagerFactory::CreateFindPackagesOptions();
            findOpts.Filters().Append(filter);
            findOpts.ResultLimit(1);

            const auto findResult = co_await connectResult.PackageCatalog().FindPackagesAsync(findOpts);

            if (findResult.Status() != FindPackagesResultStatus::Ok)
            {
                _agentPaneLog(fmt::format(
                    "[FRE] winget FindPackages failed: {} (status={})",
                    FindStatusName(findResult.Status()),
                    static_cast<int>(findResult.Status())));
                co_return finish(findResult.Status() == FindPackagesResultStatus::BlockedByPolicy
                                     ? Kind::BlockedByPolicy
                                     : Kind::Generic,
                                 hr, installerErr);
            }
            if (findResult.Matches().Size() == 0)
            {
                _agentPaneLog("[FRE] winget package not found: " + winrt::to_string(id));
                co_return finish(Kind::PackageNotFound, hr, installerErr);
            }

            const CatalogPackage package = findResult.Matches().GetAt(0).CatalogPackage();

            // ── 4. Configure install options and kick off install ──
            auto installOpts = WindowsPackageManagerFactory::CreateInstallOptions();
            installOpts.AcceptPackageAgreements(true);
            installOpts.PackageInstallMode(PackageInstallMode::Silent);
            installOpts.PackageInstallScope(PackageInstallScope::Any);

            const auto installOp = pm.InstallPackageAsync(package, installOpts);

            // ── 5. Bounded wait for install to complete ──
            // The COM API has no built-in timeout. Without one, a stuck
            // broker / unreachable installer would freeze the FRE Save
            // flow indefinitely. We allow up to 20 min (observed cold
            // installs are ~5-6 min, so 20 min covers the P99 with a
            // ~3x safety margin); at the 5 min mark we log a heads-up
            // so anyone tailing the log can tell "still running" apart
            // from "deadlocked".
            constexpr DWORD kInstallSoftWarnMs = 5 * 60 * 1000;  // 5 min
            constexpr DWORD kInstallHardCapMs = 20 * 60 * 1000;  // 20 min
            const auto startTick = GetTickCount64();
            bool warnedSoft = false;
            while (installOp.Status() == winrt::Windows::Foundation::AsyncStatus::Started)
            {
                const auto elapsed = GetTickCount64() - startTick;
                if (!warnedSoft && elapsed > kInstallSoftWarnMs)
                {
                    _agentPaneLog("[FRE] winget install: still running after 5 min, will hard-cancel at 20 min");
                    warnedSoft = true;
                }
                if (elapsed > kInstallHardCapMs)
                {
                    // Cancel is best-effort — if the installer's already
                    // running, it may keep going in the background. We
                    // surface that nuance in the user-facing Timeout
                    // message (see FreOverlay_InstallError_Timeout).
                    _agentPaneLog("[FRE] winget install: hard timeout after 20 min, cancelling");
                    installOp.Cancel();
                    co_return finish(Kind::Timeout, hr, installerErr);
                }
                co_await winrt::resume_after(std::chrono::milliseconds(500));
            }
            const auto installResult = installOp.GetResults();

            const auto status = installResult.Status();
            const auto exHr = installResult.ExtendedErrorCode();
            const auto rawInstallerErr = installResult.InstallerErrorCode();
            hr = static_cast<int32_t>(exHr);
            installerErr = rawInstallerErr;

            if (status != InstallResultStatus::Ok)
            {
                _agentPaneLog(fmt::format(
                    "[FRE] winget install failed: {} (status={}) hr=0x{:08X} installerErr={}",
                    InstallStatusName(status),
                    static_cast<int>(status),
                    static_cast<uint32_t>(exHr),
                    rawInstallerErr));

                Kind kind = Kind::Generic;
                switch (status)
                {
                case InstallResultStatus::BlockedByPolicy:
                    kind = Kind::BlockedByPolicy;
                    break;
                case InstallResultStatus::NoApplicableInstallers:
                    kind = Kind::NoCompatibleInstaller;
                    break;
                case InstallResultStatus::DownloadError:
                    // Network whitelist gates the user-facing "check your
                    // VPN" message — anything else (hash/cert/disk/AV)
                    // falls back to Generic so we don't send the user
                    // chasing the wrong problem.
                    kind = _IsNetworkLikeHResult(hr)
                               ? Kind::Network
                               : Kind::Generic;
                    break;
                case InstallResultStatus::InstallError:
                    kind = Kind::InstallerFailed;
                    break;
                // CatalogError / InternalError / ManifestError /
                // InvalidOptions / NoApplicableUpgrade /
                // PackageAgreementsNotAccepted / unknown future values
                // → Generic. CatalogError is a special case: it often
                // has a winget-specific or network HRESULT attached
                // (e.g. APPINSTALLER_CLI_ERROR_BLOCKED_BY_POLICY if the
                // catalog is GP-blocked, or a WinINet DNS code if the
                // source server is unreachable), so route through the
                // full classifier instead of just the network check.
                case InstallResultStatus::CatalogError:
                    kind = _ClassifyWingetHResult(hr);
                    break;
                default:
                    kind = Kind::Generic;
                    break;
                }
                co_return finish(kind, hr, installerErr);
            }

            // Surface RebootRequired so the caller / log readers can see
            // it. GitHub.Copilot never sets this; some MSI-style packages
            // (e.g. Node.js LTS) theoretically might. We still return
            // Success because the install itself succeeded — the caller's
            // post-install steps (PATH refresh, hook install) may or may
            // not work fully until reboot, but that's the caller's call
            // and we shouldn't fail an otherwise-successful install.
            if (installResult.RebootRequired())
            {
                _agentPaneLog("[FRE] winget install: ok (reboot required)");
            }

            co_return finish(Kind::Success, 0, 0);
        }
        catch (const winrt::hresult_error& e)
        {
            hr = static_cast<int32_t>(e.code().value);
            _agentPaneLog(fmt::format(
                "[FRE] winget exception: hr=0x{:08X} msg={}",
                static_cast<uint32_t>(hr),
                winrt::to_string(e.message())));
            // _ClassifyWingetHResult recognizes APPINSTALLER_CLI_ERROR_*
            // codes (e.g. 0x8A15003A == BLOCKED_BY_POLICY), so a GP
            // block surfaces as Kind::BlockedByPolicy with the
            // actionable "contact IT admin" message instead of the
            // generic "(error code 0x8A15003A)" fallback.
            co_return finish(_ClassifyWingetHResult(hr), hr, installerErr);
        }
        catch (...)
        {
            LOG_CAUGHT_EXCEPTION();
            co_return finish(Kind::Generic, hr, installerErr);
        }
    }

    // Conservative network-class HRESULT whitelist. We list the specific
    // WinINet / WinHTTP / Winsock codes that genuinely indicate a network
    // problem (DNS failure, connection refused/timed out, TLS handshake
    // failure, etc.). We deliberately do NOT include:
    //  * HTTP-status HRESULTs (0x80190xxx) — HTTP 404 / 403 / 5xx aren't
    //    "check your VPN" situations, they mean the request reached the
    //    server.
    //  * RPC_E_* — those are COM/service activation failures, not network.
    //  * Whole facility ranges — too easy to misclassify edge cases.
    //
    // Names in trailing comments are the macros from winhttp.h / wininet.h
    // / winsock2.h, kept here so we don't need to pull those headers in.
    bool FreOverlay::_IsNetworkLikeHResult(int32_t hr) noexcept
    {
        switch (static_cast<uint32_t>(hr))
        {
        // FACILITY_INTERNET (12xxx range) — WinINet & WinHTTP share these
        case 0x80072EE2: // ERROR_INTERNET_TIMEOUT / ERROR_WINHTTP_TIMEOUT       (12002)
        case 0x80072EE7: // ERROR_INTERNET_NAME_NOT_RESOLVED                    (12007)
        case 0x80072EFD: // ERROR_INTERNET_CANNOT_CONNECT                       (12029)
        case 0x80072EFE: // ERROR_INTERNET_CONNECTION_ABORTED                   (12030)
        case 0x80072EFF: // ERROR_INTERNET_CONNECTION_RESET                     (12031)
        case 0x80072F8F: // ERROR_INTERNET_SECURITY_CHANNEL_ERROR (TLS)         (12175)
        // FACILITY_WIN32 (Winsock 100xx, mapped via HRESULT_FROM_WIN32)
        case 0x80072742: // WSAENETDOWN          (10050)
        case 0x80072743: // WSAENETUNREACH       (10051)
        case 0x80072744: // WSAENETRESET         (10052)
        case 0x80072745: // WSAECONNABORTED      (10053)
        case 0x80072746: // WSAECONNRESET        (10054)
        case 0x8007274C: // WSAETIMEDOUT         (10060)
        case 0x8007274D: // WSAECONNREFUSED      (10061)
        case 0x80072751: // WSAEHOSTUNREACH      (10065)
        case 0x80072AF9: // WSAHOST_NOT_FOUND    (11001)
        case 0x80072AFC: // WSANO_DATA           (11004)
            return true;
        default:
            return false;
        }
    }

    // Map a raw HRESULT to the most-specific FreWingetFailureKind we can
    // infer. Used in two places:
    //  * the catch block of _WingetInstallAsync, where winget COM throws
    //    APPINSTALLER_CLI_ERROR_* codes directly (this is how policy
    //    blocks surface — winget throws 0x8A15003A *before* it ever
    //    returns an InstallResult);
    //  * the CatalogError install-status branch, where the structured
    //    Status is generic but the ExtendedErrorCode tells us why.
    //
    // The match order matters. APPINSTALLER_CLI_ERROR_* codes are
    // checked first because their meaning is unambiguous; the network
    // whitelist comes last as a transport-level fallback.
    //
    // Code names come from
    // https://github.com/microsoft/winget-cli/blob/master/src/AppInstallerSharedLib/Public/AppInstallerErrors.h
    // and are kept here as comments so we don't need to take a header
    // dependency on the winget-cli repo.
    FreOverlay::FreWingetFailureKind FreOverlay::_ClassifyWingetHResult(int32_t hr) noexcept
    {
        using Kind = FreWingetFailureKind;
        switch (static_cast<uint32_t>(hr))
        {
        // BlockedByPolicy family — group policy disabled winget or a
        // specific source/feature. Triggered by setting
        // HKLM\SOFTWARE\Policies\Microsoft\Windows\AppInstaller\EnableAppInstaller
        // (and friends) to 0.
        case 0x8A15003A: // APPINSTALLER_CLI_ERROR_BLOCKED_BY_POLICY
        case 0x8A15001B: // APPINSTALLER_CLI_ERROR_MSSTORE_BLOCKED_BY_POLICY
        case 0x8A15001C: // APPINSTALLER_CLI_ERROR_MSSTORE_APP_BLOCKED_BY_POLICY
        case 0x8A15001D: // APPINSTALLER_CLI_ERROR_EXPERIMENTAL_FEATURE_DISABLED
        case 0x8A15010F: // APPINSTALLER_CLI_ERROR_INSTALL_BLOCKED_BY_POLICY (install-phase variant of 0x8A15003A)
            return Kind::BlockedByPolicy;

        // Network / download failure codes that winget itself attaches
        // (separate from the generic WinINet/Winsock whitelist below).
        // INSTALL_NO_NETWORK is winget self-diagnosing "no network";
        // DOWNLOAD_FAILED is the install-phase wrapper around any
        // transport error during package download.
        case 0x8A150008: // APPINSTALLER_CLI_ERROR_DOWNLOAD_FAILED
        case 0x8A150107: // APPINSTALLER_CLI_ERROR_INSTALL_NO_NETWORK
            return Kind::Network;

        // Manifest was found but no installer entry matches this
        // machine's OS / architecture / scope. Usually surfaces as
        // InstallResultStatus::NoApplicableInstallers, but cover the
        // exception form for older winget versions / unusual flows.
        case 0x8A150010: // APPINSTALLER_CLI_ERROR_NO_APPLICABLE_INSTALLER
            return Kind::NoCompatibleInstaller;

        // No manifest with the requested package ID exists in any
        // configured source. Usually surfaces as
        // findResult.Matches().Size() == 0, but defensive coverage for
        // the exception form.
        case 0x8A150014: // APPINSTALLER_CLI_ERROR_NO_APPLICATIONS_FOUND
            return Kind::PackageNotFound;
        }

        // No winget-specific match — fall back to the transport-level
        // network whitelist (DNS / connect / TLS), then Generic.
        if (_IsNetworkLikeHResult(hr))
        {
            return Kind::Network;
        }
        return Kind::Generic;
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
        case FreProblemKind::ShellIntegrationExecutionPolicy:
            ErrorText().Text(RS_(L"FreOverlay_InstallErrorShellIntegrationExecutionPolicy"));
            url += L"#41-powershell";
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
            url += L"#4-shell-integration";
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

        _FinalizeProblemDisplay(url);
    }

    // Render a winget install failure with package + failure-kind specific
    // text. The mapping from FreWingetFailureKind → resource template is the
    // user-facing half of the COM API rewrite: structured InstallResultStatus
    // values become actionable, distinct messages instead of the previous
    // one-size-fits-all "check your network and try again". The help-link
    // URL is keyed on the package so it still deep-links to the right
    // manual-setup section.
    void FreOverlay::_ShowWingetProblem(FreWingetPackage package,
                                        FreWingetFailureKind kind,
                                        int32_t hr,
                                        uint32_t installerErrorCode)
    {
        static constexpr std::wstring_view baseUrl{ L"https://aka.ms/intelligent-terminal-dependency" };
        std::wstring url{ baseUrl };

        // Per-package: URL anchor + display name. The display name is a
        // localized resource (Copilot's name doesn't translate, but
        // "Node.js (LTS)" might be punctuated differently in some
        // locales — keep it loc-controlled either way).
        winrt::hstring packageName;
        switch (package)
        {
        case FreWingetPackage::Copilot:
            url += L"#31-github-copilot-cli";
            packageName = RS_(L"FreOverlay_PackageDisplayName_Copilot");
            break;
        case FreWingetPackage::Node:
            url += L"#2-nodejs-lts--shared-prerequisite";
            packageName = RS_(L"FreOverlay_PackageDisplayName_Node");
            break;
        }

        // Pre-format the numeric codes in C++ instead of relying on
        // resource-side format specs (`{1:08X}` is not portable across
        // every resource consumer, and pre-formatting also guarantees
        // ASCII hex digits regardless of the user's locale digit shape).
        const winrt::hstring hrStr{ fmt::format(L"0x{:08X}", static_cast<uint32_t>(hr)) };
        const winrt::hstring installerStr{ std::to_wstring(installerErrorCode) };

        // RS_fmt requires literal keys (extracted at build time), so the
        // template selection is a switch rather than a key-lookup.
        std::wstring text;
        switch (kind)
        {
        case FreWingetFailureKind::Network:
            text = RS_fmt(L"FreOverlay_InstallError_Network", packageName);
            break;
        case FreWingetFailureKind::BlockedByPolicy:
            text = RS_fmt(L"FreOverlay_InstallError_BlockedByPolicy", packageName);
            break;
        case FreWingetFailureKind::PackageNotFound:
            text = RS_fmt(L"FreOverlay_InstallError_PackageNotFound", packageName);
            break;
        case FreWingetFailureKind::NoCompatibleInstaller:
            text = RS_fmt(L"FreOverlay_InstallError_NoCompatibleInstaller", packageName);
            break;
        case FreWingetFailureKind::InstallerFailed:
            // If the installer reported a specific exit code, surface it.
            // Otherwise (winget said InstallError but InstallerErrorCode
            // is 0 — installer crashed without an exit code, winget
            // didn't capture one, etc.), claiming "reported error
            // (code 0)" would mislead the user. Fall back to the
            // Generic template with the HRESULT, or GenericNoCode if
            // we also lack an HRESULT.
            if (installerErrorCode != 0)
            {
                text = RS_fmt(L"FreOverlay_InstallError_InstallerFailed", packageName, installerStr);
            }
            else if (hr != 0)
            {
                text = RS_fmt(L"FreOverlay_InstallError_Generic", packageName, hrStr);
            }
            else
            {
                text = RS_fmt(L"FreOverlay_InstallError_GenericNoCode", packageName);
            }
            break;
        case FreWingetFailureKind::Timeout:
            text = RS_fmt(L"FreOverlay_InstallError_Timeout", packageName);
            break;
        case FreWingetFailureKind::Success:
            // Caller shouldn't invoke us on Success — fall through to
            // the no-code Generic template so a bug here surfaces a
            // readable message instead of "error code 0x00000000".
        case FreWingetFailureKind::Generic:
        default:
            // When hr == 0 we have no actionable error code to show
            // (e.g. catalog connect / package search failed before any
            // installer ran). Use the no-code template so users don't
            // see the misleading "(error code 0x00000000)".
            if (hr == 0)
            {
                text = RS_fmt(L"FreOverlay_InstallError_GenericNoCode", packageName);
            }
            else
            {
                text = RS_fmt(L"FreOverlay_InstallError_Generic", packageName, hrStr);
            }
            break;
        }
        ErrorText().Text(winrt::hstring{ text });

        _FinalizeProblemDisplay(url);
    }

    void FreOverlay::_FinalizeProblemDisplay(const std::wstring& url)
    {
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
        // Capture the dispatcher while we're definitely on the UI thread.
        // After any subsequent `co_await` that resumes on a background
        // thread (e.g. _WingetInstallAsync, _InstallHooksAsync), calling
        // `Dispatcher()` directly would implicitly dereference `this` —
        // which is UB if the FRE overlay was destroyed mid-await (user
        // closed the tab / window / quit the app during a long winget
        // install). The captured value is a ref-counted CoreDispatcher,
        // independent of `this`'s lifetime.
        const auto dispatcher = Dispatcher();

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
        //
        // Note: `_IsWingetInstalled()` checks for `winget.exe` on PATH,
        // which is the CLI's App Execution Alias. `_WingetInstallAsync`
        // itself now uses the WinGet COM API and does not strictly
        // require the alias to be on PATH (only AppInstaller / the COM
        // server). In practice the alias and the COM server are
        // installed/uninstalled together with AppInstaller, so this
        // check still correctly distinguishes "winget environment
        // present" from "winget environment absent" in 99%+ of cases.
        // The edge case (alias disabled while AppInstaller present) is
        // rare enough that incorrectly showing WingetMissing is
        // acceptable; the user docs still apply.
        if (needsCopilot || needsNode)
        {
            if (!_IsWingetInstalled())
            {
                _agentPaneLog("[FRE] winget not found on PATH");
                _ShowProblem(FreProblemKind::WingetMissing);
                co_return;
            }

            // ── Await any in-flight pre-warm before kicking off install ──
            // The Initialize() handler may have started a `winget source
            // update` in the background. WinGet's intra-process
            // coordination across concurrent operations is not a
            // guaranteed contract — we serialise here to avoid two
            // winget instances stepping on each other. Snapshot the
            // action under the mutex (Initialize may still be racing to
            // assign it), then co_await OUTSIDE the lock (holding a
            // std::mutex across a suspension point is undefined behaviour).
            winrt::Windows::Foundation::IAsyncAction pending{ nullptr };
            {
                std::lock_guard<std::mutex> lock{ s_prewarmMutex };
                pending = s_prewarmAction;
            }
            if (pending &&
                pending.Status() != winrt::Windows::Foundation::AsyncStatus::Completed)
            {
                _agentPaneLog("[FRE] Save: waiting for pre-warm to finish");
                try
                {
                    co_await pending;
                }
                catch (...)
                {
                    // Pre-warm failure is non-fatal; install will just
                    // pay the source-refresh cost itself.
                    LOG_CAUGHT_EXCEPTION();
                }
                _agentPaneLog("[FRE] Save: pre-warm done, proceeding with install");
            }
        }

        if (needsCopilot)
        {
            _agentPaneLog("[FRE] Installing GitHub.Copilot via winget");
            const auto kindInt = co_await _WingetInstallAsync(L"GitHub.Copilot");
            // Helper internally does co_await winrt::resume_background(),
            // so the continuation may resume on a thread-pool thread.
            // Hop back to the UI thread before any XAML access (the
            // _ShowWingetProblem call below touches ErrorText / ErrorPanel
            // / toggles); without this, RPC_E_WRONG_THREAD is thrown and
            // silently swallowed by IAsyncAction, leaving the
            // SavingOverlay stuck.
            co_await winrt::resume_foreground(dispatcher);
            auto self = weak.get();
            if (!self) co_return;
            const auto kind = static_cast<FreWingetFailureKind>(kindInt);
            _agentPaneLog("[FRE] Copilot install: " +
                          std::string(kind == FreWingetFailureKind::Success ? "ok" : "FAILED"));
            if (kind != FreWingetFailureKind::Success)
            {
                // _lastWingetHr / _lastWingetInstallerErrorCode were
                // populated by _WingetInstallAsync on this same instance;
                // safe to read here because the Copilot install awaited
                // above is the only writer in this sequential chain.
                _ShowWingetProblem(FreWingetPackage::Copilot,
                                   kind,
                                   _lastWingetHr,
                                   _lastWingetInstallerErrorCode);
                co_return;
            }
        }
        if (needsNode)
        {
            _agentPaneLog("[FRE] Installing Node.js via winget");
            const auto kindInt = co_await _WingetInstallAsync(L"OpenJS.NodeJS.LTS");
            // See note above for the Copilot install — same threading
            // concern applies here.
            co_await winrt::resume_foreground(dispatcher);
            auto self = weak.get();
            if (!self) co_return;
            const auto kind = static_cast<FreWingetFailureKind>(kindInt);
            _agentPaneLog("[FRE] Node.js install: " +
                          std::string(kind == FreWingetFailureKind::Success ? "ok" : "FAILED"));
            if (kind != FreWingetFailureKind::Success)
            {
                _ShowWingetProblem(FreWingetPackage::Node,
                                   kind,
                                   _lastWingetHr,
                                   _lastWingetInstallerErrorCode);
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
            co_await winrt::resume_foreground(dispatcher);
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

            // Snapshot WSL distros AND non-WSL shell presence on the UI
            // thread BEFORE resuming on a background thread —
            // _settings.AllProfiles() is an observable vector and
            // iterating it concurrently with a settings reload is unsafe.
            const auto wslDistros = ShellIntegrationSweep::SnapshotWslDistroNames(_settings);
            const auto shellPresence = ShellIntegrationSweep::SnapshotShellPresence(_settings);

            co_await winrt::resume_background();
            // Profile-gated install: a user keeping only "Developer
            // PowerShell for VS" (Windows PowerShell) and no pwsh
            // profile must not get a pwsh integration block written.
            // RunInstall reports a skipped shell as
            // success-already-installed so the FRE failure verdict
            // (below) doesn't flag a missing shell as a failure.
            const auto results = ShellIntegrationSweep::RunInstall(shellPresence, wslDistros);
            const auto& pwsh7Result = results.pwsh;
            const auto& windowsPsResult = results.windowsPowerShell;
            const auto& bashResult = results.bash;
            const auto& wslResults = results.wsl;

            {
                std::string detail = "[FRE] Shell integration: pwsh7=";
                detail += pwsh7Result.success ? "ok" : "FAILED";
                if (!pwsh7Result.success && !pwsh7Result.errorMessage.empty())
                    detail += " (" + winrt::to_string(winrt::hstring{ pwsh7Result.errorMessage }) + ")";
                detail += " winPs=";
                detail += windowsPsResult.success ? "ok" : "FAILED";
                if (!windowsPsResult.success && !windowsPsResult.errorMessage.empty())
                    detail += " (" + winrt::to_string(winrt::hstring{ windowsPsResult.errorMessage }) + ")";
                detail += " bash=";
                detail += bashResult.success ? "ok" : "FAILED";
                if (!bashResult.success && !bashResult.errorMessage.empty())
                    detail += " (" + winrt::to_string(winrt::hstring{ bashResult.errorMessage }) + ")";
                for (const auto& [distName, r] : wslResults)
                {
                    detail += " wsl(" + winrt::to_string(winrt::hstring{ distName }) + ")=";
                    detail += r.success ? "ok" : "FAILED";
                    if (!r.success && !r.errorMessage.empty())
                        detail += " (" + winrt::to_string(winrt::hstring{ r.errorMessage }) + ")";
                }
                _agentPaneLog(detail);
            }

            // Shell integration is treated as failed when EITHER
            // PowerShell host's install fails. Both hosts are part of
            // the user's primary shell family and the install is
            // best-effort idempotent — if pwsh7 isn't installed,
            // InstallForTarget returns success-with-empty-error
            // because the file write succeeds harmlessly; only a real
            // write failure or an execution-policy block reaches here.
            // Bash and WSL failures are NOT counted here: users
            // without Git Bash or without (running) WSL would
            // otherwise see false-alarm errors on every FRE / Save.
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
            co_await winrt::resume_foreground(dispatcher);
            auto self = weak.get();
            if (!self) co_return;

            _ShowProblem(shellIntegEpBlocked ? FreProblemKind::ShellIntegrationExecutionPolicy
                                             : shellIntegFailed ? FreProblemKind::ShellIntegration
                                                                : FreProblemKind::Hooks);
            co_return;
        }

        // 6. Resume UI thread before touching controls / raising events
        co_await winrt::resume_foreground(dispatcher);
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
