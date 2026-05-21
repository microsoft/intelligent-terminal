// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "FreOverlay.h"
#include "FreAgentEntry.g.cpp"
#include "FreOverlay.g.cpp"

#include "../inc/AgentRegistry.h"
#include "../inc/WtaProcess.h"
#include "../inc/ShellIntegration.h"

using namespace winrt::Windows::Foundation;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;

namespace winrt::TerminalApp::implementation
{
    FreOverlay::FreOverlay()
    {
        InitializeComponent();
    }

    // ── Detection helpers ───────────────────────────────────────────────

    bool FreOverlay::_IsAgentInstalled(const wchar_t* name)
    {
        wchar_t buf[MAX_PATH];
        if (SearchPathW(nullptr, name, L".exe", MAX_PATH, buf, nullptr) > 0)
            return true;
        const auto cmdName = std::wstring(name) + L".cmd";
        if (SearchPathW(nullptr, cmdName.c_str(), nullptr, MAX_PATH, buf, nullptr) > 0)
            return true;
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

    // ── Initialize ──────────────────────────────────────────────────────

    void FreOverlay::Initialize(const winrt::Microsoft::Terminal::Settings::Model::CascadiaSettings& settings)
    {
        _settings = settings;
        const auto& globals = _settings.GlobalSettings();
        namespace Reg = ::Microsoft::Terminal::Settings::Model::AgentRegistry;

        // Set subtitle Run texts (can't use x:Uid for <Run> inside <Hyperlink>)
        WelcomeSubtitlePrefix().Text(RS_(L"FreOverlay_WelcomeSubtitlePrefix"));
        WelcomeSubtitleLink().Text(RS_(L"FreOverlay_WelcomeSubtitleLink"));
        SettingsSubtitlePrefix().Text(RS_(L"FreOverlay_SettingsSubtitlePrefix"));
        SettingsSubtitleLink().Text(RS_(L"FreOverlay_SettingsSubtitleLink"));

        // Set toggle On/Off labels
        AutoErrorToggle().OnContent(winrt::box_value(RS_(L"FreOverlay_ToggleOn")));
        AutoErrorToggle().OffContent(winrt::box_value(RS_(L"FreOverlay_ToggleOff")));
        SessionManagementToggle().OnContent(winrt::box_value(RS_(L"FreOverlay_ToggleOn")));
        SessionManagementToggle().OffContent(winrt::box_value(RS_(L"FreOverlay_ToggleOff")));

        // Populate agent ComboBox: Copilot (always) + detected agents
        auto items = AgentComboBox().Items();
        items.Clear();
        int32_t selectedIndex = 0;
        int32_t idx = 0;
        const auto currentAgent = globals.AcpAgent();

        for (const auto& a : Reg::BuiltinAcpAgents)
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

            if (a.id == currentAgent)
            {
                selectedIndex = idx;
            }
            idx++;
        }

        if (items.Size() > 0)
        {
            AgentComboBox().SelectedIndex(selectedIndex);
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

        // Set toggles from current settings
        AutoErrorToggle().IsOn(globals.AutoFixEnabled());
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
                AgentInstallHint().Visibility(needsNode ? Visibility::Visible : Visibility::Collapsed);
            }
        }
    }

    void FreOverlay::_OnSessionManagementToggled(const IInspectable& /*sender*/,
                                                  const RoutedEventArgs& /*args*/)
    {
        // Guard: event can fire during InitializeComponent before controls exist
        auto toggle = SessionManagementToggle();
        auto hint = SessionManagementHint();
        if (toggle && hint)
        {
            hint.Visibility(toggle.IsOn() ? Visibility::Visible : Visibility::Collapsed);
        }
    }

    // ── Page navigation ─────────────────────────────────────────────────

    void FreOverlay::_OnNextButtonClick(const IInspectable& /*sender*/,
                                        const RoutedEventArgs& /*args*/)
    {
        WelcomePage().Visibility(Visibility::Collapsed);
        SettingsPage().Visibility(Visibility::Visible);
    }

    // ── Winget install helper ───────────────────────────────────────────

    IAsyncOperation<bool> FreOverlay::_WingetInstallAsync(winrt::hstring packageId)
    {
        // Copy packageId before switching threads (coroutine parameter safety)
        auto id = std::wstring{ packageId };

        co_await winrt::resume_background();

        auto cmdline = fmt::format(
            L"winget install --id {} --exact --silent "
            L"--accept-source-agreements --accept-package-agreements "
            L"--disable-interactivity",
            id);

        STARTUPINFOW si{};
        si.cb = sizeof(si);
        si.dwFlags = STARTF_USESHOWWINDOW;
        si.wShowWindow = SW_HIDE;
        PROCESS_INFORMATION pi{};

        auto success = CreateProcessW(
            nullptr,
            cmdline.data(),
            nullptr, nullptr, FALSE,
            CREATE_NO_WINDOW,
            nullptr, nullptr, &si, &pi);

        if (!success)
        {
            co_return false;
        }

        WaitForSingleObject(pi.hProcess, 300000); // 5 min timeout
        DWORD exitCode = 1;
        GetExitCodeProcess(pi.hProcess, &exitCode);
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
        co_return exitCode == 0;
    }


    // ── Hooks install helper ────────────────────────────────────────────

    IAsyncAction FreOverlay::_InstallHooksAsync(winrt::hstring agentId)
    {
        auto id = std::wstring{ agentId };

        co_await winrt::resume_background();

        namespace Wta = ::Microsoft::Terminal::WtaProcess;

        const auto wtaPath = Wta::ResolveWtaExePath();
        // Extend PATH so freshly-installed CLIs (e.g. copilot via winget)
        // are discoverable by the hooks installer.
        auto envBlock = Wta::BuildExtendedPathEnvBlock();
        auto args = L"hooks install --cli " + id;
        Wta::RunWtaAndWait(wtaPath, args, 60'000,
                           envBlock.empty() ? nullptr : envBlock.data());
    }

    // ── Save + install flow ─────────────────────────────────────────────

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

        // 2. Disable button, hide previous error
        SaveButton().Content(winrt::box_value(RS_(L"FreOverlay_SettingUp")));
        SaveButton().IsEnabled(false);
        ErrorText().Visibility(Visibility::Collapsed);

        // 3. Install prerequisites if needed (blocking — cannot proceed without these)
        const bool needsCopilot = (agentId == L"copilot") && !_IsAgentInstalled(L"copilot");
        const bool needsNode = (agentId == L"claude" || agentId == L"codex") && !_IsNodeInstalled();

        if (needsCopilot)
        {
            bool ok = co_await _WingetInstallAsync(L"GitHub.Copilot");
            auto self = weak.get();
            if (!self) co_return;
            if (!ok)
            {
                ErrorText().Text(RS_(L"FreOverlay_InstallErrorCopilot"));
                ErrorText().Visibility(Visibility::Visible);
                SaveButton().Content(winrt::box_value(RS_(L"FreOverlay_SaveButton/Content")));
                SaveButton().IsEnabled(true);
                co_return;
            }
        }
        if (needsNode)
        {
            bool ok = co_await _WingetInstallAsync(L"OpenJS.NodeJS.LTS");
            auto self = weak.get();
            if (!self) co_return;
            if (!ok)
            {
                ErrorText().Text(RS_(L"FreOverlay_InstallErrorNode"));
                ErrorText().Visibility(Visibility::Visible);
                SaveButton().Content(winrt::box_value(RS_(L"FreOverlay_SaveButton/Content")));
                SaveButton().IsEnabled(true);
                co_return;
            }
        }

        // 4. Install hooks if session management is enabled
        {
            auto self = weak.get();
            if (!self) co_return;

            if (SessionManagementToggle().IsOn())
            {
                co_await _InstallHooksAsync(agentId);
            }
        }

        // 5. Install shell integration for autofix (if enabled)
        {
            auto self = weak.get();
            if (!self) co_return;

            bool doShellInteg = AutoErrorToggle().IsOn();

            if (doShellInteg)
            {
                co_await winrt::resume_background();
                namespace SI = ::Microsoft::Terminal::ShellIntegration;
                SI::InstallForTarget(SI::Target::Pwsh);
                SI::InstallForTarget(SI::Target::WindowsPowerShell);
            }
        }

        // 6. Resume UI thread before touching controls / raising events
        co_await winrt::resume_foreground(Dispatcher());
        {
            auto self = weak.get();
            if (!self) co_return;

            SaveButton().Content(winrt::box_value(RS_(L"FreOverlay_SaveButton/Content")));
            SaveButton().IsEnabled(true);
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
}
