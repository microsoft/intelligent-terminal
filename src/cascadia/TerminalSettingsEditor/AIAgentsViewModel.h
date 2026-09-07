// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "AIAgentsViewModel.g.h"
#include "AcpModelEntry.g.h"
#include "AgentEntry.g.h"
#include "CustomModelProviderEntry.g.h"
#include "ViewModelHelpers.h"
#include "Utils.h"
#include "../inc/AgentRegistry.h"
#include "../inc/CustomModelCredential.h"
#include "../inc/CustomModelProviderUtils.h"

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
        winrt::hstring CustomCommand() const { return _customCommand; }
        winrt::Windows::UI::Xaml::Visibility RemoveButtonVisibility() const noexcept
        {
            return _remove ?
                       winrt::Windows::UI::Xaml::Visibility::Visible :
                       winrt::Windows::UI::Xaml::Visibility::Collapsed;
        }
        void Remove();

        void SetAddNew(bool value) { _isAddNew = value; }
        void SetCustomCommand(winrt::hstring value) { _customCommand = std::move(value); }
        void SetRemove(std::function<void()> remove) { _remove = std::move(remove); }

    private:
        winrt::hstring _id;
        winrt::hstring _displayName;
        bool _isInstalled;
        bool _isAddNew{ false };
        winrt::hstring _customCommand;
        std::function<void()> _remove;
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

    struct CustomModelProviderEntry :
        CustomModelProviderEntryT<CustomModelProviderEntry>,
        ViewModelHelper<CustomModelProviderEntry>
    {
        CustomModelProviderEntry(
            Model::CustomModelProvider provider,
            std::function<void()> remove);

        using ViewModelHelper<CustomModelProviderEntry>::PropertyChanged;

        winrt::hstring Id() const { return _provider.Id(); }
        winrt::hstring BaseUrl() const { return _provider.BaseUrl(); }
        winrt::hstring ModelsDisplayText() const;
        bool IsApiKeyMissing() const noexcept { return _isApiKeyMissing; }
        winrt::hstring RemovalErrorMessage() const { return _removalErrorMessage; }
        bool HasRemovalError() const noexcept { return !_removalErrorMessage.empty(); }
        void Remove();

        Model::CustomModelProvider Provider() const { return _provider; }

    private:
        Model::CustomModelProvider _provider;
        std::function<void()> _remove;
        bool _isApiKeyMissing{ false };
        winrt::hstring _removalErrorMessage;
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
        void CancelCustomDelegateAgent();

        bool ShowAcpModel();
        winrt::Windows::Foundation::Collections::IObservableVector<Editor::AcpModelEntry> AcpModelList() const { return _acpModelList; }
        // Probe in flight counts as "present" so the ComboBox stays
        // visible (PlaceholderText="Default") instead of flashing the
        // free-form textbox during the probe window.
        bool HasAcpModelList() const;
        bool ShowAcpModelTextBox() const { return !HasAcpModelList(); }
        Editor::AcpModelEntry CurrentAcpModelEntry();
        void CurrentAcpModelEntry(const Editor::AcpModelEntry& value);
        PERMANENT_OBSERVABLE_PROJECTED_SETTING(_GlobalSettings, AcpModel);
        winrt::Windows::Foundation::Collections::IObservableVector<Editor::CustomModelProviderEntry> CustomModelProviders() const { return _customModelProviders; }
        bool ShowCustomModelProvidersExpander() const { return _isAddingCustomModelProvider || _customModelProviders.Size() != 0; }
        bool IsCustomModelProvidersExpanded() const { return _isCustomModelProvidersExpanded; }
        void IsCustomModelProvidersExpanded(bool value);
        bool IsAddingCustomModelProvider() const { return _isAddingCustomModelProvider; }
        winrt::hstring NewCustomModelProviderBaseUrl() const { return _newCustomModelProviderBaseUrl; }
        void NewCustomModelProviderBaseUrl(const winrt::hstring& value);
        winrt::hstring NewCustomModelId() const { return _newCustomModelId; }
        void NewCustomModelId(const winrt::hstring& value);
        winrt::hstring NewCustomModelProviderApiKey() const { return _newCustomModelProviderApiKey; }
        void NewCustomModelProviderApiKey(const winrt::hstring& value);
        bool CanSaveCustomModelProvider() const { return _HasNonWhitespace(_newCustomModelProviderBaseUrl) && _HasNonWhitespace(_newCustomModelId); }
        winrt::hstring CustomModelProviderUnsupportedMessage();
        void AddCustomModelProvider();
        void SaveCustomModelProvider();
        void CancelCustomModelProvider();
        bool ShowDelegateModel();
        PERMANENT_OBSERVABLE_PROJECTED_SETTING(_GlobalSettings, DelegateModel);
        winrt::Windows::Foundation::Collections::IObservableVector<Editor::EnumEntry> AutoErrorHandlingList() const { return _autoErrorHandlingList; }
        winrt::Windows::Foundation::IInspectable CurrentAutoErrorHandling();
        void CurrentAutoErrorHandling(const winrt::Windows::Foundation::IInspectable& value);
        bool AgentSessionManagementEnabled() const;
        void AgentSessionManagementEnabled(bool value);
        bool HasAgentSessionManagementEnabled() const;
        bool CanConfigureAgentSessionManagement() const;
        PERMANENT_OBSERVABLE_PROJECTED_SETTING(_GlobalSettings, ShowTokenUsageAndCost);

        bool AgentPaneYoloMode() const;
        void AgentPaneYoloMode(bool value);
        bool HasAgentPaneYoloMode() const;
        bool ShowOpenCodeYoloWarning() const;
        bool ShowGeminiYoloInfo() const;

        // GPO policy lock indicators
        bool IsAgentPolicyLocked() const { return _GlobalSettings.IsAgentPolicyLocked(); }
        bool IsCustomAgentPolicyLocked() const { return _GlobalSettings.IsCustomAgentPolicyLocked(); }
        bool IsAutoErrorHandlingPolicyRestricted() const { return _GlobalSettings.IsAutoFixPolicyLocked(); }
        bool IsAgentSessionHooksPolicyLocked() const { return _GlobalSettings.IsAgentSessionHooksPolicyLocked(); }
        bool IsYoloModePolicyLocked() const { return _GlobalSettings.IsYoloModePolicyLocked(); }

        winrt::Windows::Foundation::Collections::IObservableVector<winrt::Microsoft::Terminal::Settings::Editor::EnumEntry> AgentPanePositionList();
        winrt::Windows::Foundation::IInspectable CurrentAgentPanePosition();
        void CurrentAgentPanePosition(const winrt::Windows::Foundation::IInspectable& value);

        til::typed_event<Editor::AIAgentsViewModel, Model::ShellIntegrationTarget> InitShellIntegrationRequested;

    private:
        Model::GlobalAppSettings _GlobalSettings;
        winrt::Windows::Foundation::Collections::IObservableVector<Editor::AgentEntry> _acpAgentList;
        winrt::Windows::Foundation::Collections::IObservableVector<Editor::AgentEntry> _delegateAgentList;
        winrt::Windows::Foundation::Collections::IObservableVector<Editor::AcpModelEntry> _acpModelList;
        winrt::Windows::Foundation::Collections::IObservableVector<Editor::CustomModelProviderEntry> _customModelProviders;
        std::vector<Model::CustomModelProvider> _originalCustomModelProviders;

        winrt::Windows::Foundation::Collections::IObservableVector<winrt::Microsoft::Terminal::Settings::Editor::EnumEntry> _agentPanePositionList;
        winrt::Windows::Foundation::Collections::IMap<winrt::hstring, winrt::Microsoft::Terminal::Settings::Editor::EnumEntry> _agentPanePositionMap;
        winrt::Windows::Foundation::Collections::IObservableVector<Editor::EnumEntry> _autoErrorHandlingList;

        bool _isAddingCustomAcpAgent{ false };
        bool _isAddingCustomDelegateAgent{ false };
        bool _isCustomModelProvidersExpanded{ false };
        bool _isAddingCustomModelProvider{ false };
        winrt::hstring _customAcpCommand;
        winrt::hstring _customDelegateCommand;
        winrt::hstring _editingCustomAcpAgentId;
        winrt::hstring _editingCustomDelegateAgentId;
        winrt::hstring _newCustomModelProviderBaseUrl;
        winrt::hstring _newCustomModelId;
        winrt::hstring _newCustomModelProviderApiKey;

        winrt::event_token _acpRuntimeChangedToken{};
        void _RebuildAcpModelListFromCache();
        void _LoadCustomModelProviders();
        void _CommitCustomModelProviders();
        void _RemoveCustomModelProvider(const winrt::hstring& id);
        static bool _HasNonWhitespace(std::wstring_view value) noexcept;
        static winrt::hstring _TrimWhitespace(std::wstring_view value);

        // ── ACP model probe ──
        // A background `wta probe-models --agent <cmd>` invocation that
        // populates the dropdown after the user picks a new agent in
        // Settings, without waiting for the agent pane to be rebuilt.
        // See `_TriggerAcpModelProbe` in the .cpp for the full flow.
        bool _acpProbing{ false };
        // Generation counter: bumped each time _TriggerAcpModelProbe
        // fires. An in-flight probe checks this before publishing its
        // result and bails if a newer trigger has superseded it (user
        // picked a different agent while we were still talking to the
        // previous one).
        uint64_t _acpProbeGeneration{ 0 };
        void _TriggerAcpModelProbe();
        winrt::fire_and_forget _RunAcpModelProbeAsync(winrt::hstring agentId, std::wstring agentCmdline, uint64_t generation, uint64_t cacheRevision);
        // Mirror of TerminalPage::_ResolveEffectiveAgentCliPath. Kept
        // here (rather than in inc/) because the Settings UI sits in
        // a separate project and can't include TerminalApp headers.
        std::wstring _ResolveEffectiveAcpAgentCmdline() const;

        static bool _IsAgentInstalled(const wchar_t* name);
        static bool _IsKnownAgent(const winrt::hstring& id);
        static winrt::hstring _DeriveId(const winrt::hstring& command);
        Editor::AgentEntry _CreateCustomAgentEntry(
            const winrt::hstring& settingsId,
            const winrt::hstring& displayName,
            const winrt::hstring& customCommand,
            bool isAcpAgent);
        static bool _CustomCommandMatchesId(
            const winrt::hstring& command,
            const winrt::hstring& settingsId);
        static winrt::Windows::Foundation::Collections::IVector<winrt::hstring> _NormalizeCustomCommands(
            const winrt::Windows::Foundation::Collections::IVector<winrt::hstring>& commands);
        static winrt::Windows::Foundation::Collections::IVector<winrt::hstring> _UpdateCustomCommands(
            const winrt::Windows::Foundation::Collections::IVector<winrt::hstring>& commands,
            const winrt::hstring& originalId,
            const winrt::hstring& command);
        static winrt::Windows::Foundation::Collections::IVector<winrt::hstring> _RemoveCustomCommand(
            const winrt::Windows::Foundation::Collections::IVector<winrt::hstring>& commands,
            const winrt::hstring& settingsId);
        static winrt::hstring _FindCustomCommand(
            const winrt::Windows::Foundation::Collections::IVector<winrt::hstring>& commands,
            const winrt::hstring& settingsId);
        void _DeleteCustomAcpAgent(const winrt::hstring& settingsId);
        void _DeleteCustomDelegateAgent(const winrt::hstring& settingsId);
        bool _IsSelectedAcpAgentAvailable() const;
        ::Microsoft::Terminal::Settings::Model::AgentRegistry::YoloSettingsNotice _YoloSettingsNotice() const;
        Editor::AgentEntry _FindEntryById(
            const winrt::Windows::Foundation::Collections::IObservableVector<Editor::AgentEntry>& list,
            const winrt::hstring& id) const;
        Editor::AgentEntry _FindReplacementAgent(
            const winrt::Windows::Foundation::Collections::IObservableVector<Editor::AgentEntry>& list,
            const winrt::hstring& preferredId) const;
        void _AppendAddNewEntry(
            winrt::Windows::Foundation::Collections::IObservableVector<Editor::AgentEntry>& list);
        void _MaybeAppendCustomEntry(
            winrt::Windows::Foundation::Collections::IObservableVector<Editor::AgentEntry>& list,
            const winrt::hstring& customCommand,
            bool isAcpAgent);
        void _RebuildCustomEntries(
            winrt::Windows::Foundation::Collections::IObservableVector<Editor::AgentEntry>& list,
            const winrt::Windows::Foundation::Collections::IVector<winrt::hstring>& commands,
            bool isAcpAgent);

    };
};

namespace winrt::Microsoft::Terminal::Settings::Editor::factory_implementation
{
    BASIC_FACTORY(AIAgentsViewModel);
    BASIC_FACTORY(AgentEntry);
    BASIC_FACTORY(AcpModelEntry);
}
