// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "AIAgentsViewModel.g.h"
#include "AcpModelEntry.g.h"
#include "AgentEntry.g.h"
#include "ViewModelHelpers.h"
#include "Utils.h"
#include "../inc/AgentHooksStatus.h"

namespace winrt::Microsoft::Terminal::Settings::Editor::implementation
{
    struct AgentEntry : AgentEntryT<AgentEntry>
    {
        AgentEntry(winrt::hstring id, winrt::hstring displayName, bool isInstalled);

        winrt::hstring Id() const { return _id; }
        winrt::hstring DisplayName() const { return _displayName; }
        winrt::hstring DisplayLabel() const;
        bool IsInstalled() const { return _isInstalled; }
        bool IsAddNew() const { return _isAddNew; }

        void SetAddNew(bool value) { _isAddNew = value; }

    private:
        winrt::hstring _id;
        winrt::hstring _displayName;
        bool _isInstalled;
        bool _isAddNew{ false };
    };

    struct AcpModelEntry : AcpModelEntryT<AcpModelEntry>
    {
        AcpModelEntry(winrt::hstring id, winrt::hstring displayName, winrt::hstring description) :
            _id{ std::move(id) },
            _displayName{ std::move(displayName) },
            _description{ std::move(description) }
        {
        }

        winrt::hstring Id() const { return _id; }
        winrt::hstring DisplayName() const { return _displayName; }
        winrt::hstring Description() const { return _description; }

    private:
        winrt::hstring _id;
        winrt::hstring _displayName;
        winrt::hstring _description;
    };

    struct AIAgentsViewModel : AIAgentsViewModelT<AIAgentsViewModel>, ViewModelHelper<AIAgentsViewModel>
    {
    public:
        AIAgentsViewModel(Model::GlobalAppSettings globalSettings);
        ~AIAgentsViewModel();

        using ViewModelHelper<AIAgentsViewModel>::PropertyChanged;

        winrt::Windows::Foundation::Collections::IObservableVector<Editor::AgentEntry> AcpAgentList() const { return _acpAgentList; }
        winrt::Windows::Foundation::Collections::IObservableVector<Editor::AgentEntry> DelegateAgentList() const { return _delegateAgentList; }

        Editor::AgentEntry CurrentAcpAgent();
        void CurrentAcpAgent(const Editor::AgentEntry& value);
        Editor::AgentEntry CurrentDelegateAgent();
        void CurrentDelegateAgent(const Editor::AgentEntry& value);

        // Custom agent preview
        bool IsCustomAcpAgentSelected();
        winrt::hstring CustomAcpCommandPreview();
        void EditCustomAcpAgent();
        bool IsCustomDelegateAgentSelected();
        winrt::hstring CustomDelegateCommandPreview();
        void EditCustomDelegateAgent();

        // Edit mode
        bool IsAddingCustomAcpAgent() const { return _isAddingCustomAcpAgent; }
        bool IsAddingCustomDelegateAgent() const { return _isAddingCustomDelegateAgent; }

        winrt::hstring CustomAcpCommand() const { return _customAcpCommand; }
        void CustomAcpCommand(const winrt::hstring& value);
        winrt::hstring CustomDelegateCommand() const { return _customDelegateCommand; }
        void CustomDelegateCommand(const winrt::hstring& value);

        void SaveCustomAcpAgent();
        void SaveCustomDelegateAgent();
        void CancelCustomAcpAgent();
        void DeleteCustomAcpAgent();
        void CancelCustomDelegateAgent();
        void DeleteCustomDelegateAgent();

        bool ShowAcpModel();
        winrt::Windows::Foundation::Collections::IObservableVector<Editor::AcpModelEntry> AcpModelList() const { return _acpModelList; }
        bool HasAcpModelList() const { return _acpModelList && _acpModelList.Size() > 0; }
        bool ShowAcpModelTextBox() const { return !HasAcpModelList(); }
        Editor::AcpModelEntry CurrentAcpModelEntry();
        void CurrentAcpModelEntry(const Editor::AcpModelEntry& value);
        PERMANENT_OBSERVABLE_PROJECTED_SETTING(_GlobalSettings, AcpModel);
        bool ShowDelegateModel();
        PERMANENT_OBSERVABLE_PROJECTED_SETTING(_GlobalSettings, DelegateModel);
        bool AutoFixEnabled() const;
        void AutoFixEnabled(bool value);
        bool HasAutoFixEnabled() const;

        winrt::Windows::Foundation::Collections::IObservableVector<winrt::Microsoft::Terminal::Settings::Editor::EnumEntry> AgentPanePositionList();
        winrt::Windows::Foundation::IInspectable CurrentAgentPanePosition();
        void CurrentAgentPanePosition(const winrt::Windows::Foundation::IInspectable& value);

        til::typed_event<Editor::AIAgentsViewModel, Model::ShellIntegrationTarget> InitShellIntegrationRequested;

        // ── Agent Hooks ──────────────────────────────────────────────────
        bool IsCopilotCliDetected() const noexcept { return _copilotCliDetected; }
        bool IsClaudeCliDetected() const noexcept { return _claudeCliDetected; }
        bool IsGeminiCliDetected() const noexcept { return _geminiCliDetected; }
        bool IsAnyAgentCliDetected() const noexcept
        {
            return _copilotCliDetected || _claudeCliDetected || _geminiCliDetected;
        }
        winrt::hstring CopilotHooksStatusText() const { return _copilotHooksStatus; }
        winrt::hstring ClaudeHooksStatusText() const { return _claudeHooksStatus; }
        winrt::hstring GeminiHooksStatusText() const { return _geminiHooksStatus; }
        bool IsInstallingAgentHooks() const noexcept { return _installingAgentHooks; }
        winrt::hstring AgentHooksInstallSummary() const { return _agentHooksInstallSummary; }

        void RefreshAgentHooksStatus();
        void InstallAgentHooks();

    private:
        Model::GlobalAppSettings _GlobalSettings;
        winrt::Windows::Foundation::Collections::IObservableVector<Editor::AgentEntry> _acpAgentList;
        winrt::Windows::Foundation::Collections::IObservableVector<Editor::AgentEntry> _delegateAgentList;
        winrt::Windows::Foundation::Collections::IObservableVector<Editor::AcpModelEntry> _acpModelList;

        winrt::Windows::Foundation::Collections::IObservableVector<winrt::Microsoft::Terminal::Settings::Editor::EnumEntry> _agentPanePositionList;
        winrt::Windows::Foundation::Collections::IMap<winrt::hstring, winrt::Microsoft::Terminal::Settings::Editor::EnumEntry> _agentPanePositionMap;

        bool _isAddingCustomAcpAgent{ false };
        bool _isAddingCustomDelegateAgent{ false };
        winrt::hstring _customAcpCommand;
        winrt::hstring _customDelegateCommand;

        winrt::event_token _acpRuntimeChangedToken{};
        void _RebuildAcpModelListFromCache();

        static bool _IsAgentInstalled(const wchar_t* name);
        static bool _IsKnownAgent(const winrt::hstring& id);
        static winrt::hstring _DeriveId(const winrt::hstring& command);
        Editor::AgentEntry _FindEntryById(
            const winrt::Windows::Foundation::Collections::IObservableVector<Editor::AgentEntry>& list,
            const winrt::hstring& id) const;
        void _AppendAddNewEntry(
            winrt::Windows::Foundation::Collections::IObservableVector<Editor::AgentEntry>& list);
        void _MaybeAppendCustomEntry(
            winrt::Windows::Foundation::Collections::IObservableVector<Editor::AgentEntry>& list,
            const winrt::hstring& customCommand,
            const winrt::hstring& currentAgentId);

        // Agent Hooks state
        bool _copilotCliDetected{ false };
        bool _claudeCliDetected{ false };
        bool _geminiCliDetected{ false };
        winrt::hstring _copilotHooksStatus;
        winrt::hstring _claudeHooksStatus;
        winrt::hstring _geminiHooksStatus;
        bool _installingAgentHooks{ false };
        bool _refreshingAgentHooks{ false };
        winrt::hstring _agentHooksInstallSummary;

        static std::wstring _ResolveWtaExePath();
        static std::string _RunWtaCaptureStdout(const std::wstring& wtaPath,
                                                const std::wstring& argsAfterExe,
                                                DWORD timeoutMs);
        void _ApplyStatusReport(const std::optional<::Microsoft::Terminal::AgentHooks::StatusReport>& report);
        winrt::fire_and_forget _RefreshAgentHooksStatusAsync();
        winrt::fire_and_forget _RunHooksInstallerAsync();
    };
};

namespace winrt::Microsoft::Terminal::Settings::Editor::factory_implementation
{
    BASIC_FACTORY(AIAgentsViewModel);
    BASIC_FACTORY(AgentEntry);
    BASIC_FACTORY(AcpModelEntry);
}
