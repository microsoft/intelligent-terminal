// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "AIAgentsViewModel.h"
#include "AIAgentsViewModel.g.cpp"
#include "AcpModelEntry.g.cpp"
#include "AgentEntry.g.cpp"
#include "CustomModelProviderEntry.g.cpp"
#include "EnumEntry.h"
#include "../inc/AgentRegistry.h"
#include "../inc/AgentHooksStatus.h"
#include "../inc/CustomAgentId.h"
#include "../inc/WtaProcess.h"

#include <json/json.h>

using namespace winrt::Windows::Foundation;
using namespace winrt::Windows::Foundation::Collections;
using namespace winrt::Microsoft::Terminal::Settings::Model;

namespace winrt::Microsoft::Terminal::Settings::Editor::implementation
{
    CustomModelProviderEntry::CustomModelProviderEntry(
        Model::CustomModelProvider provider,
        std::function<void()> remove) :
        _provider{ std::move(provider) },
        _remove{ std::move(remove) }
    {
    }

    void CustomModelProviderEntry::Remove()
    {
        try
        {
            ::Microsoft::Terminal::CustomModels::RemoveApiKey(_provider.ApiKeyCredential());
        }
        catch (...)
        {
            const auto hr = wil::ResultFromCaughtException();
            LOG_HR(hr);
        }
        _remove();
    }

    // ── AgentEntry ───────────────────────────────────────────────────────

    AgentEntry::AgentEntry(winrt::hstring id, winrt::hstring displayName, bool isInstalled) :
        _id{ std::move(id) },
        _displayName{ std::move(displayName) },
        _isInstalled{ isInstalled }
    {
    }

    winrt::hstring AgentEntry::DisplayLabel() const
    {
        if (_isAddNew) return L"+ Add New...";
        if (_isInstalled) return _displayName;
        return _displayName + L" (not installed)";
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    bool AIAgentsViewModel::_IsAgentInstalled(const wchar_t* name)
    {
        wchar_t buf[MAX_PATH];
        if (SearchPathW(nullptr, name, L".exe", MAX_PATH, buf, nullptr) > 0) return true;
        const auto cmdName = std::wstring(name) + L".cmd";
        if (SearchPathW(nullptr, cmdName.c_str(), nullptr, MAX_PATH, buf, nullptr) > 0) return true;
        return false;
    }

    bool AIAgentsViewModel::_IsKnownAgent(const winrt::hstring& id)
    {
        namespace Reg = ::Microsoft::Terminal::Settings::Model::AgentRegistry;
        for (const auto& a : Reg::BuiltinAcpAgents)
        {
            if (id == a.id) return true;
        }
        for (const auto& a : Reg::BuiltinDelegateAgents)
        {
            if (id == a.id) return true;
        }
        return false;
    }

    static bool _StartsWithCustom(const winrt::hstring& id)
    {
        return winrt::to_string(id).starts_with("custom:");
    }

    winrt::hstring AIAgentsViewModel::_DeriveId(const winrt::hstring& command)
    {
        // Delegate to the header-only helper shared with the unit tests.
        return ::Microsoft::Terminal::Settings::Model::DeriveCustomAgentId(
            std::wstring_view{ command });
    }

    void AIAgentsViewModel::_AppendAddNewEntry(IObservableVector<Editor::AgentEntry>& list)
    {
        auto entry = winrt::make_self<AgentEntry>(L"__add_new__", L"+ Add New...", true);
        entry->SetAddNew(true);
        list.Append(*entry);
    }

    void AIAgentsViewModel::_MaybeAppendCustomEntry(
        IObservableVector<Editor::AgentEntry>& list,
        const winrt::hstring& customCommand,
        const winrt::hstring& currentAgentId)
    {
        if (customCommand.empty() || !_StartsWithCustom(currentAgentId)) return;

        const auto bareId = _DeriveId(customCommand);
        const bool isBuiltIn = _IsKnownAgent(bareId);
        // Mirror SaveCustom*: the saved id always carries "custom:".
        const auto settingsId = winrt::hstring{ L"custom:" + std::wstring_view{ bareId } };
        const auto displayName = isBuiltIn
            ? winrt::hstring{ std::wstring_view{ bareId } + L" (custom)" }
            : bareId;

        // Don't add duplicate
        for (uint32_t i = 0; i < list.Size(); ++i)
        {
            if (list.GetAt(i).Id() == settingsId) return;
        }
        list.Append(winrt::make<AgentEntry>(settingsId, displayName, true));
    }

    // ── ViewModel ────────────────────────────────────────────────────────

    AIAgentsViewModel::AIAgentsViewModel(Model::GlobalAppSettings globalSettings) :
        _GlobalSettings{ globalSettings }
    {
        namespace Reg = ::Microsoft::Terminal::Settings::Model::AgentRegistry;

        // Refresh PATH from the Windows registry so SearchPathW can find
        // CLIs installed after Terminal launched (e.g. WinGet\Links).
        try
        {
            ::Microsoft::Terminal::WtaProcess::RefreshProcessPath();
        }
        catch (...)
        {
            LOG_CAUGHT_EXCEPTION();
        }

        // ACP-capable agents — use GPO-filtered list so only policy-allowed
        // agents appear in the dropdown. Also skip agents whose CLI isn't
        // installed — the dropdown only offers choices the user can actually
        // launch.
        const auto filteredAcp = Reg::FilteredAcpAgents();
        std::vector<Editor::AgentEntry> acpEntries;
        for (const auto& a : filteredAcp)
        {
            if (!_IsAgentInstalled(std::wstring{ a.id }.c_str()))
            {
                continue;
            }
            acpEntries.push_back(winrt::make<AgentEntry>(
                winrt::hstring{ a.id },
                winrt::hstring{ a.displayName },
                true));
        }
        _acpAgentList = winrt::single_threaded_observable_vector(std::move(acpEntries));
        // Only show custom entry and "Add New" if custom agents are allowed by policy.
        if (!_GlobalSettings.IsCustomAgentPolicyLocked())
        {
            _MaybeAppendCustomEntry(_acpAgentList, _GlobalSettings.AcpCustomCommand(), _GlobalSettings.AcpAgent());
            _AppendAddNewEntry(_acpAgentList);
        }

        // ACP-advertised model list. Populated by TerminalPage::OnAgentStatusChanged
        // whenever wta pushes a fresh agent_status event. We hold an
        // observable vector here and re-snapshot it whenever the runtime
        // cache fires Changed — that's how the dropdown stays in sync after
        // the user switches agents (cache cleared) or wta reconnects with a
        // new model list.
        _acpModelList = winrt::single_threaded_observable_vector<Editor::AcpModelEntry>();
        _customModelProviders = winrt::single_threaded_observable_vector<Editor::CustomModelProviderEntry>();
        _LoadCustomModelProviders();
        _RebuildAcpModelListFromCache();
        _acpRuntimeChangedToken = Model::AcpRuntimeState::Current().Changed(
            [weakThis = get_weak()](const auto&, const auto&) {
                if (auto self = weakThis.get())
                {
                    self->_RebuildAcpModelListFromCache();
                }
            });

        // Delegate agents — same GPO-filtered + install-filter rule.
        const auto filteredDelegate = Reg::FilteredDelegateAgents();
        std::vector<Editor::AgentEntry> delegateEntries;
        for (const auto& a : filteredDelegate)
        {
            if (!_IsAgentInstalled(std::wstring{ a.id }.c_str()))
            {
                continue;
            }
            delegateEntries.push_back(winrt::make<AgentEntry>(
                winrt::hstring{ a.id },
                winrt::hstring{ a.displayName },
                true));
        }
        _delegateAgentList = winrt::single_threaded_observable_vector(std::move(delegateEntries));
        if (!_GlobalSettings.IsCustomAgentPolicyLocked())
        {
            _MaybeAppendCustomEntry(_delegateAgentList, _GlobalSettings.DelegateCustomCommand(), _GlobalSettings.DelegateAgent());
            _AppendAddNewEntry(_delegateAgentList);
        }

        // Pane position list
        _agentPanePositionMap = winrt::single_threaded_map<winrt::hstring, Editor::EnumEntry>();
        std::vector<Editor::EnumEntry> posEntries;
        const std::pair<winrt::hstring, std::wstring_view> positions[] = {
            { RS_(L"AIAgents_PanePosition_Bottom"), L"bottom" },
            { RS_(L"AIAgents_PanePosition_Right"), L"right" },
            { RS_(L"AIAgents_PanePosition_Top"), L"top" },
            { RS_(L"AIAgents_PanePosition_Left"), L"left" },
        };
        for (const auto& [displayName, value] : positions)
        {
            auto entry = winrt::make<implementation::EnumEntry>(
                displayName,
                winrt::box_value(winrt::hstring{ value }));
            posEntries.emplace_back(entry);
            _agentPanePositionMap.Insert(winrt::hstring{ value }, entry);
        }
        _agentPanePositionList = winrt::single_threaded_observable_vector<Editor::EnumEntry>(std::move(posEntries));

        // Populate the Agent Hooks section's per-CLI detection + install
        // state so the UI displays meaningful labels on first paint. The
        // actual status query shells out to `wta hooks status --json`
        // off the UI thread; seed a placeholder until it returns so the
        // user sees something other than empty rows.
        // Rows are hidden until the first status query returns; the only
        // thing the user sees in the expander before that is the Install
        // row (always present) and the help text.
        RefreshAgentHooksStatus();
    }

    AIAgentsViewModel::~AIAgentsViewModel()
    {
        if (_acpRuntimeChangedToken.value)
        {
            Model::AcpRuntimeState::Current().Changed(_acpRuntimeChangedToken);
        }
    }

    void AIAgentsViewModel::_LoadCustomModelProviders()
    {
        auto providers = _GlobalSettings.CustomModelProviders();

        auto weakThis = get_weak();
        for (const auto& provider : providers)
        {
            const auto id = provider.Id();
            _customModelProviders.Append(winrt::make<CustomModelProviderEntry>(
                provider,
                [weakThis, id]() {
                    if (const auto self = weakThis.get())
                    {
                        self->_RemoveCustomModelProvider(id);
                    }
                }));
        }
    }

    void AIAgentsViewModel::_CommitCustomModelProviders()
    {
        auto providers = winrt::single_threaded_vector<Model::CustomModelProvider>();
        for (const auto& entry : _customModelProviders)
        {
            providers.Append(winrt::get_self<CustomModelProviderEntry>(entry)->Provider());
        }
        _GlobalSettings.CustomModelProviders(providers);

        // An agent launched with BYOK may advertise the injected model in its
        // native catalog. Invalidate that snapshot when providers change so a
        // removed model cannot survive as a stale runtime entry.
        const auto agentId = _GlobalSettings.EffectiveAcpAgent();
        Model::AcpRuntimeState::Current().SetAvailableModels(
            agentId,
            winrt::single_threaded_vector<Model::AcpModelInfo>().GetView(),
            L"");
        _TriggerAcpModelProbe();
        _NotifyChanges(L"CustomModelProviders");
    }

    void AIAgentsViewModel::_RemoveCustomModelProvider(const winrt::hstring& id)
    {
        for (uint32_t i = 0; i < _customModelProviders.Size(); ++i)
        {
            if (_customModelProviders.GetAt(i).Id() == id)
            {
                _customModelProviders.RemoveAt(i);
                break;
            }
        }

        std::wstring selectedProvider;
        std::wstring selectedModel;
        if (::Microsoft::Terminal::CustomModels::TryParseSelectionId(
                std::wstring_view{ _GlobalSettings.CustomModelSelection() },
                selectedProvider,
                selectedModel) &&
            selectedProvider == std::wstring_view{ id })
        {
            _GlobalSettings.CustomModelSelection(L"");
        }
        _CommitCustomModelProviders();
    }

    void AIAgentsViewModel::NewCustomModelProviderBaseUrl(const winrt::hstring& value)
    {
        if (_newCustomModelProviderBaseUrl != value)
        {
            _newCustomModelProviderBaseUrl = value;
            _NotifyChanges(L"NewCustomModelProviderBaseUrl", L"CanSaveCustomModelProvider");
        }
    }

    void AIAgentsViewModel::NewCustomModelId(const winrt::hstring& value)
    {
        if (_newCustomModelId != value)
        {
            _newCustomModelId = value;
            _NotifyChanges(L"NewCustomModelId", L"CanSaveCustomModelProvider");
        }
    }

    void AIAgentsViewModel::NewCustomModelApiKey(const winrt::hstring& value)
    {
        if (_newCustomModelApiKey != value)
        {
            _newCustomModelApiKey = value;
            _newCustomModelApiKeyError.clear();
            _NotifyChanges(L"NewCustomModelApiKey", L"NewCustomModelApiKeyError");
        }
    }

    void AIAgentsViewModel::IsCustomModelProvidersExpanded(const bool value)
    {
        if (_isCustomModelProvidersExpanded != value)
        {
            _isCustomModelProvidersExpanded = value;
            _NotifyChanges(L"IsCustomModelProvidersExpanded");
        }
    }

    void AIAgentsViewModel::AddCustomModelProvider()
    {
        IsCustomModelProvidersExpanded(true);
        _isAddingCustomModelProvider = true;
        _NotifyChanges(L"IsAddingCustomModelProvider");
    }

    bool AIAgentsViewModel::_HasNonWhitespace(const std::wstring_view value) noexcept
    {
        return std::ranges::any_of(value, [](const wchar_t ch) {
            return !std::iswspace(ch);
        });
    }

    winrt::hstring AIAgentsViewModel::_TrimWhitespace(const std::wstring_view value)
    {
        const auto isWhitespace = [](const wchar_t ch) {
            return std::iswspace(ch);
        };
        const auto first = std::ranges::find_if_not(value, isWhitespace);
        const auto last = std::ranges::find_if_not(value.rbegin(), value.rend(), isWhitespace).base();
        if (first >= last)
        {
            return {};
        }

        const auto offset = gsl::narrow_cast<size_t>(first - value.begin());
        const auto length = gsl::narrow_cast<size_t>(last - first);
        return winrt::hstring{ value.substr(offset, length) };
    }

    void AIAgentsViewModel::SaveCustomModelProvider()
    {
        const auto baseUrl = _TrimWhitespace(_newCustomModelProviderBaseUrl);
        const auto modelId = _TrimWhitespace(_newCustomModelId);
        if (baseUrl.empty() || modelId.empty())
        {
            return;
        }

        GUID guid{};
        THROW_IF_FAILED(CoCreateGuid(&guid));
        std::wstring idValue{ L"provider-" };
        idValue.append(winrt::to_hstring(guid));
        const auto id = winrt::hstring{ idValue };
        auto provider = Model::CustomModelProvider{
            id,
            baseUrl,
            baseUrl };
        provider.Location(::Microsoft::Terminal::CustomModels::ResolvedLocation(provider));
        provider.Models().Append(Model::CustomModel{ modelId, modelId });
        if (!_newCustomModelApiKey.empty())
        {
            try
            {
                provider.ApiKeyCredential(::Microsoft::Terminal::CustomModels::StoreApiKey(
                    {},
                    _newCustomModelApiKey));
                provider.ApiKeyRequired(true);
            }
            catch (...)
            {
                const auto hr = wil::ResultFromCaughtException();
                LOG_HR(hr);
                _newCustomModelApiKeyError = winrt::hresult_error{ hr }.message();
                _NotifyChanges(L"NewCustomModelApiKeyError");
                return;
            }
        }

        auto weakThis = get_weak();
        _customModelProviders.Append(winrt::make<CustomModelProviderEntry>(
            provider,
            [weakThis, id]() {
                if (const auto self = weakThis.get())
                {
                    self->_RemoveCustomModelProvider(id);
                }
            }));
        _CommitCustomModelProviders();
        CancelCustomModelProvider();
    }

    void AIAgentsViewModel::CancelCustomModelProvider()
    {
        _isAddingCustomModelProvider = false;
        _newCustomModelProviderBaseUrl.clear();
        _newCustomModelId.clear();
        _newCustomModelApiKey.clear();
        _newCustomModelApiKeyError.clear();
        _NotifyChanges(
            L"IsAddingCustomModelProvider",
            L"NewCustomModelProviderBaseUrl",
            L"NewCustomModelId",
            L"NewCustomModelApiKey",
            L"NewCustomModelApiKeyError",
            L"CanSaveCustomModelProvider");
    }

    void AIAgentsViewModel::_RebuildAcpModelListFromCache()
    {
        if (!_acpModelList) return;

        const auto agent = _GlobalSettings.EffectiveAcpAgent();
        const auto cached = Model::AcpRuntimeState::Current().AvailableModels(agent);
        const uint32_t newSize = cached ? cached.Size() : 0;

        // Mirror the agent's advertised list 1:1 — each ACP agent
        // already publishes its own "use the default" entry (claude
        // calls it `default`, copilot `auto`), so synthesizing one
        // here would just duplicate it.
        _acpModelList.Clear();
        namespace Reg = ::Microsoft::Terminal::Settings::Model::AgentRegistry;
        const bool supportsCustomModels = Reg::SupportsByok(std::wstring_view{ agent });
        for (uint32_t i = 0; i < newSize; ++i)
        {
            const auto m = cached.GetAt(i);
            _acpModelList.Append(winrt::make<AcpModelEntry>(
                m.Id(),
                m.DisplayName(),
                m.Description()));
        }
        if (supportsCustomModels)
        {
            for (const auto& provider : _GlobalSettings.CustomModelProviders())
            {
                for (const auto& model : provider.Models())
                {
                    const auto id = ::Microsoft::Terminal::CustomModels::SelectionId(
                        provider.Id(),
                        model.Id());
                    const auto location = ::Microsoft::Terminal::CustomModels::ResolvedLocation(provider);
                    bool alreadyPresent = false;
                    for (uint32_t i = _acpModelList.Size(); i > 0; --i)
                    {
                        const auto existingId = _acpModelList.GetAt(i - 1).Id();
                        if (existingId == id)
                        {
                            alreadyPresent = true;
                            break;
                        }
                        if (existingId == model.Id())
                        {
                            // A BYOK-configured agent may advertise the injected
                            // model as if it were a native model. Keep the BYOK
                            // entry so selecting it retains the provider metadata.
                            _acpModelList.RemoveAt(i - 1);
                        }
                    }
                    if (alreadyPresent)
                    {
                        continue;
                    }
                    const auto displayName = winrt::hstring{ RS_fmt(L"AIAgents_BYOKModelDisplayName", model.Id()) };
                    const auto description = winrt::hstring{ RS_fmt(L"AIAgents_OpenAICompatibleModelDescription", provider.Name(), location) };
                    _acpModelList.Append(winrt::make<AcpModelEntry>(
                        id,
                        displayName,
                        description));
                }
            }
        }

        // Reconcile a *stale* persisted id with the authoritative list.
        // Only fires when the user has actively configured a specific model
        // (non-empty) that this agent doesn't advertise — e.g. switching
        // agents leaves a leftover id. In that case reset to the empty
        // "agent default" sentinel rather than picking the agent's
        // "auto"/"default" entry: empty is the unambiguous "send no model
        // override" state and renders as the ComboBox's "Default"
        // placeholder, so we never silently mislabel a stale id as a real
        // model the user didn't choose.
        //
        // Empty is already the legitimate "use whatever default the agent
        // picks" sentinel, so the empty case needs no reconciliation.
        if (_acpModelList.Size() > 0)
        {
            const auto current = supportsCustomModels && !_GlobalSettings.CustomModelSelection().empty() ?
                                     _GlobalSettings.CustomModelSelection() :
                                     _GlobalSettings.AcpModel();
            if (!current.empty())
            {
                bool matched = false;
                for (uint32_t i = 0; i < _acpModelList.Size(); ++i)
                {
                    if (_acpModelList.GetAt(i).Id() == current)
                    {
                        matched = true;
                        break;
                    }
                }
                if (!matched)
                {
                    // Stale leftover id this agent doesn't advertise → reset
                    // to the empty "agent default" sentinel (send no model
                    // override), which renders as the "Default" placeholder.
                    if (supportsCustomModels && !_GlobalSettings.CustomModelSelection().empty())
                    {
                        _GlobalSettings.CustomModelSelection(L"");
                    }
                    else
                    {
                        _GlobalSettings.AcpModel(L"");
                    }
                }
            }
        }

        _NotifyChanges(L"AcpModelList",
                       L"HasAcpModelList",
                       L"ShowAcpModelTextBox",
                       L"AcpModel",
                       L"CurrentAcpModelEntry");
    }

    bool AIAgentsViewModel::HasAcpModelList() const
    {
        return _acpModelList && (_acpModelList.Size() > 0 || _acpProbing);
    }

    Editor::AgentEntry AIAgentsViewModel::_FindEntryById(
        const IObservableVector<Editor::AgentEntry>& list,
        const winrt::hstring& id) const
    {
        for (uint32_t i = 0; i < list.Size(); ++i)
        {
            const auto entry = list.GetAt(i);
            if (entry.Id() == id && !entry.IsAddNew()) return entry;
        }
        return nullptr;
    }

    // ── Custom agent preview & edit ──────────────────────────────────────

    bool AIAgentsViewModel::IsCustomAcpAgentSelected()
    {
        if (_isAddingCustomAcpAgent) return false;
        // If custom agents are blocked by GPO, treat as not selected even
        // if the raw setting still has a custom: value from before policy
        // was applied.
        if (_GlobalSettings.IsCustomAgentPolicyLocked()) return false;
        return _StartsWithCustom(_GlobalSettings.AcpAgent());
    }

    winrt::hstring AIAgentsViewModel::CustomAcpCommandPreview()
    {
        if (_GlobalSettings.IsCustomAgentPolicyLocked()) return winrt::hstring{};
        return _StartsWithCustom(_GlobalSettings.AcpAgent()) ? _GlobalSettings.AcpCustomCommand() : winrt::hstring{};
    }

    void AIAgentsViewModel::EditCustomAcpAgent()
    {
        if (_StartsWithCustom(_GlobalSettings.AcpAgent()))
        {
            _isAddingCustomAcpAgent = true;
            _customAcpCommand = _GlobalSettings.AcpCustomCommand();
            _NotifyChanges(L"IsAddingCustomAcpAgent", L"IsCustomAcpAgentSelected", L"CustomAcpCommand", L"ShowAcpModel");
        }
    }

    bool AIAgentsViewModel::IsCustomDelegateAgentSelected()
    {
        if (_isAddingCustomDelegateAgent) return false;
        return _StartsWithCustom(_GlobalSettings.DelegateAgent());
    }

    winrt::hstring AIAgentsViewModel::CustomDelegateCommandPreview()
    {
        return _StartsWithCustom(_GlobalSettings.DelegateAgent()) ? _GlobalSettings.DelegateCustomCommand() : winrt::hstring{};
    }

    void AIAgentsViewModel::EditCustomDelegateAgent()
    {
        if (_StartsWithCustom(_GlobalSettings.DelegateAgent()))
        {
            _isAddingCustomDelegateAgent = true;
            _customDelegateCommand = _GlobalSettings.DelegateCustomCommand();
            _NotifyChanges(L"IsAddingCustomDelegateAgent", L"IsCustomDelegateAgentSelected", L"CustomDelegateCommand", L"ShowDelegateModel");
        }
    }

    // ── ShowModel ────────────────────────────────────────────────────────

    Editor::AcpModelEntry AIAgentsViewModel::CurrentAcpModelEntry()
    {
        if (!_acpModelList) return nullptr;
        namespace Reg = ::Microsoft::Terminal::Settings::Model::AgentRegistry;
        const auto current =
            Reg::SupportsByok(std::wstring_view{ _GlobalSettings.EffectiveAcpAgent() }) &&
                    !_GlobalSettings.CustomModelSelection().empty() ?
                _GlobalSettings.CustomModelSelection() :
                _GlobalSettings.AcpModel();
        for (uint32_t i = 0; i < _acpModelList.Size(); ++i)
        {
            const auto entry = _acpModelList.GetAt(i);
            if (entry.Id() == current) return entry;
        }
        // Unconfigured case (empty persisted id): return null so the
        // ComboBox renders its "Default" PlaceholderText. This is the
        // distinct "agent default — send no model override" state and is
        // intentionally NOT mapped onto the agent's advertised
        // "auto"/"default" entry. That advertised entry is a real model in
        // the agent's support list (e.g. copilot's "auto" router) which,
        // when explicitly selected, gets forwarded via setSessionModel;
        // conflating the two would mislabel "no override" (which resolves
        // to the agent's own server-side default, e.g. claude-sonnet-4.6)
        // as the "auto" model. The stale-id case (non-empty + no match) is
        // reset to empty at the data layer by _RebuildAcpModelListFromCache,
        // so it also lands here and shows the placeholder.
        // Empty list (probe hasn't run yet) likewise → PlaceholderText.
        return nullptr;
    }

    void AIAgentsViewModel::CurrentAcpModelEntry(const Editor::AcpModelEntry& value)
    {
        if (!value)
        {
            return;
        }
        namespace Reg = ::Microsoft::Terminal::Settings::Model::AgentRegistry;
        const bool supportsByok = Reg::SupportsByok(std::wstring_view{ _GlobalSettings.EffectiveAcpAgent() });
        if (::Microsoft::Terminal::CustomModels::IsCustomSelection(value.Id()))
        {
            if (_GlobalSettings.CustomModelSelection() == value.Id() && _GlobalSettings.AcpModel().empty())
            {
                return;
            }
            _GlobalSettings.CustomModelSelection(value.Id());
            _GlobalSettings.AcpModel(L"");
        }
        else
        {
            if (_GlobalSettings.AcpModel() == value.Id() &&
                (!supportsByok || _GlobalSettings.CustomModelSelection().empty()))
            {
                return;
            }
            _GlobalSettings.AcpModel(value.Id());
            if (supportsByok)
            {
                _GlobalSettings.CustomModelSelection(L"");
            }
        }
        _NotifyChanges(L"AcpModel");
    }

    bool AIAgentsViewModel::ShowAcpModel()
    {
        // Show for every built-in agent AND for custom agents. The original
        // code hid the row for custom:* which then trapped users when a
        // previously-selected acpModel turned invalid (e.g. credentials
        // expired) — the stale value was invisible and unclearable.
        // HasAcpModelList / ShowAcpModelTextBox pick between the dropdown
        // (when the helper has published available_models via agent_status)
        // and the free-form textbox fallback.
        if (_isAddingCustomAcpAgent) return false;
        if (_StartsWithCustom(_GlobalSettings.AcpAgent())) return true;
        return _IsKnownAgent(_GlobalSettings.AcpAgent());
    }

    bool AIAgentsViewModel::ShowDelegateModel()
    {
        // Same rationale as ShowAcpModel: show the row for custom delegate
        // agents so a stale delegateModel value remains visible / clearable.
        if (_isAddingCustomDelegateAgent) return false;
        if (_StartsWithCustom(_GlobalSettings.DelegateAgent())) return true;
        return _IsKnownAgent(_GlobalSettings.DelegateAgent());
    }

    // ── Current agent getters/setters ────────────────────────────────────

    Editor::AgentEntry AIAgentsViewModel::CurrentAcpAgent()
    {
        if (_isAddingCustomAcpAgent)
        {
            const auto currentId = _GlobalSettings.AcpAgent();
            auto entry = _FindEntryById(_acpAgentList, currentId);
            if (entry) return entry;
            for (uint32_t i = 0; i < _acpAgentList.Size(); ++i)
            {
                if (_acpAgentList.GetAt(i).IsAddNew()) return _acpAgentList.GetAt(i);
            }
        }
        auto match = _FindEntryById(_acpAgentList, _GlobalSettings.AcpAgent());
        if (match) return match;

        // Saved agent is not in the filtered list (blocked by GPO or not
        // installed). Fall back to the first real entry so the ComboBox
        // always has a valid SelectedItem and doesn't freeze.
        for (uint32_t i = 0; i < _acpAgentList.Size(); ++i)
        {
            const auto entry = _acpAgentList.GetAt(i);
            if (!entry.IsAddNew()) return entry;
        }
        return nullptr;
    }

    void AIAgentsViewModel::CurrentAcpAgent(const Editor::AgentEntry& value)
    {
        if (!value) return;
        if (value.IsAddNew())
        {
            if (_isAddingCustomAcpAgent) return;
            _isAddingCustomAcpAgent = true;
            _customAcpCommand = L"";
            _NotifyChanges(L"IsAddingCustomAcpAgent", L"IsCustomAcpAgentSelected", L"CustomAcpCommand", L"ShowAcpModel");
            return;
        }
        auto idStr = winrt::to_string(value.Id());
        if (idStr.starts_with("custom:"))
        {
            if (_isAddingCustomAcpAgent && _GlobalSettings.AcpAgent() == value.Id()) return;
            _isAddingCustomAcpAgent = true;
            _customAcpCommand = _GlobalSettings.AcpCustomCommand();
            _GlobalSettings.AcpAgent(value.Id());
            _NotifyChanges(L"IsAddingCustomAcpAgent", L"IsCustomAcpAgentSelected", L"CustomAcpCommand", L"ShowAcpModel");
            return;
        }
        if (value.Id() != _GlobalSettings.AcpAgent())
        {
            _isAddingCustomAcpAgent = false;
            _GlobalSettings.AcpAgent(value.Id());
            // Native model ids are agent-specific; the shared BYOK selection
            // is stored separately and survives agent switches.
            _GlobalSettings.AcpModel(L"");
            _TriggerAcpModelProbe();
            _NotifyChanges(L"CurrentAcpAgent",
                           L"IsAddingCustomAcpAgent",
                           L"IsCustomAcpAgentSelected",
                           L"ShowAcpModel",
                           L"HasAcpModelList",
                           L"ShowAcpModelTextBox",
                           L"AcpModel");
        }
    }

    Editor::AgentEntry AIAgentsViewModel::CurrentDelegateAgent()
    {
        if (_isAddingCustomDelegateAgent)
        {
            const auto currentId = _GlobalSettings.DelegateAgent();
            auto entry = _FindEntryById(_delegateAgentList, currentId);
            if (entry) return entry;
            for (uint32_t i = 0; i < _delegateAgentList.Size(); ++i)
            {
                if (_delegateAgentList.GetAt(i).IsAddNew()) return _delegateAgentList.GetAt(i);
            }
        }
        auto match = _FindEntryById(_delegateAgentList, _GlobalSettings.DelegateAgent());
        if (match) return match;

        // Saved agent is not in the filtered list (blocked by GPO or not
        // installed). Fall back to the first real entry.
        for (uint32_t i = 0; i < _delegateAgentList.Size(); ++i)
        {
            const auto entry = _delegateAgentList.GetAt(i);
            if (!entry.IsAddNew()) return entry;
        }
        return nullptr;
    }

    void AIAgentsViewModel::CurrentDelegateAgent(const Editor::AgentEntry& value)
    {
        if (!value) return;
        if (value.IsAddNew())
        {
            if (_isAddingCustomDelegateAgent) return;
            _isAddingCustomDelegateAgent = true;
            _customDelegateCommand = L"";
            _NotifyChanges(L"IsAddingCustomDelegateAgent", L"IsCustomDelegateAgentSelected", L"CustomDelegateCommand", L"ShowDelegateModel");
            return;
        }
        auto idStr = winrt::to_string(value.Id());
        if (idStr.starts_with("custom:"))
        {
            if (_isAddingCustomDelegateAgent && _GlobalSettings.DelegateAgent() == value.Id()) return;
            _isAddingCustomDelegateAgent = true;
            _customDelegateCommand = _GlobalSettings.DelegateCustomCommand();
            _GlobalSettings.DelegateAgent(value.Id());
            _NotifyChanges(L"IsAddingCustomDelegateAgent", L"IsCustomDelegateAgentSelected", L"CustomDelegateCommand", L"ShowDelegateModel");
            return;
        }
        if (value.Id() != _GlobalSettings.DelegateAgent())
        {
            _isAddingCustomDelegateAgent = false;
            _GlobalSettings.DelegateAgent(value.Id());
            _NotifyChanges(L"CurrentDelegateAgent", L"IsAddingCustomDelegateAgent", L"IsCustomDelegateAgentSelected", L"ShowDelegateModel");
        }
    }

    void AIAgentsViewModel::CustomAcpCommand(const winrt::hstring& value)
    {
        _customAcpCommand = value;
        _NotifyChanges(L"CustomAcpCommand");
    }

    void AIAgentsViewModel::CustomDelegateCommand(const winrt::hstring& value)
    {
        _customDelegateCommand = value;
        _NotifyChanges(L"CustomDelegateCommand");
    }

    // ── Save / Delete / Cancel ───────────────────────────────────────────

    void AIAgentsViewModel::SaveCustomAcpAgent()
    {
        if (_GlobalSettings.IsCustomAgentPolicyLocked()) return;
        if (_customAcpCommand.empty()) return;
        const auto bareId = _DeriveId(_customAcpCommand);
        // Whitespace-only / quote-only commands derive to an empty id and
        // would otherwise be saved as a bare "custom:" entry, leaving the
        // UI with a blank, unusable custom agent. Reject before persisting.
        if (bareId.empty()) return;
        _GlobalSettings.AcpCustomCommand(_customAcpCommand);

        // Custom agents always carry the "custom:" discriminator — every
        // downstream consumer (EffectiveAcpAgent policy gate, command-line
        // resolver, custom-edit/delete UI gates) keys on this prefix.
        // Storing a bare id silently breaks all of them and makes the page
        // revert to the default agent on next load.
        const bool isBuiltIn = _IsKnownAgent(bareId);
        const auto settingsId = winrt::hstring{ L"custom:" + std::wstring_view{ bareId } };
        const auto displayName = isBuiltIn
            ? winrt::hstring{ std::wstring_view{ bareId } + L" (custom)" }
            : bareId;

        bool found = false;
        for (uint32_t i = 0; i < _acpAgentList.Size(); ++i)
        {
            if (_acpAgentList.GetAt(i).Id() == settingsId) { found = true; break; }
        }
        if (!found)
        {
            const auto addNewIdx = _acpAgentList.Size() - 1;
            _acpAgentList.InsertAt(addNewIdx, winrt::make<AgentEntry>(settingsId, displayName, true));
        }

        _isAddingCustomAcpAgent = false;
        _GlobalSettings.AcpAgent(settingsId);
        _GlobalSettings.AcpModel(L"");
        Model::AcpRuntimeState::Current().SetAvailableModels(
            settingsId,
            winrt::single_threaded_vector<Model::AcpModelInfo>().GetView(),
            L"");
        _TriggerAcpModelProbe();
        _NotifyChanges(L"CurrentAcpAgent", L"IsAddingCustomAcpAgent", L"IsCustomAcpAgentSelected", L"ShowAcpModel", L"CustomAcpCommandPreview", L"AcpModel");
    }

    void AIAgentsViewModel::SaveCustomDelegateAgent()
    {
        if (_GlobalSettings.IsCustomAgentPolicyLocked()) return;
        if (_customDelegateCommand.empty()) return;
        const auto bareId = _DeriveId(_customDelegateCommand);
        // See SaveCustomAcpAgent — reject empty derivations before persisting.
        if (bareId.empty()) return;
        _GlobalSettings.DelegateCustomCommand(_customDelegateCommand);

        // See SaveCustomAcpAgent — always carry the "custom:" prefix.
        const bool isBuiltIn = _IsKnownAgent(bareId);
        const auto settingsId = winrt::hstring{ L"custom:" + std::wstring_view{ bareId } };
        const auto displayName = isBuiltIn
            ? winrt::hstring{ std::wstring_view{ bareId } + L" (custom)" }
            : bareId;

        bool found = false;
        for (uint32_t i = 0; i < _delegateAgentList.Size(); ++i)
        {
            if (_delegateAgentList.GetAt(i).Id() == settingsId) { found = true; break; }
        }
        if (!found)
        {
            const auto addNewIdx = _delegateAgentList.Size() - 1;
            _delegateAgentList.InsertAt(addNewIdx, winrt::make<AgentEntry>(settingsId, displayName, true));
        }

        _isAddingCustomDelegateAgent = false;
        _GlobalSettings.DelegateAgent(settingsId);
        _NotifyChanges(L"CurrentDelegateAgent", L"IsAddingCustomDelegateAgent", L"IsCustomDelegateAgentSelected", L"ShowDelegateModel", L"CustomDelegateCommandPreview");
    }

    void AIAgentsViewModel::CancelCustomAcpAgent()
    {
        _isAddingCustomAcpAgent = false;
        _NotifyChanges(L"IsAddingCustomAcpAgent", L"IsCustomAcpAgentSelected", L"CurrentAcpAgent", L"ShowAcpModel");
    }

    void AIAgentsViewModel::CancelCustomDelegateAgent()
    {
        _isAddingCustomDelegateAgent = false;
        _NotifyChanges(L"IsAddingCustomDelegateAgent", L"IsCustomDelegateAgentSelected", L"CurrentDelegateAgent", L"ShowDelegateModel");
    }

    void AIAgentsViewModel::DeleteCustomAcpAgent()
    {
        auto idStr = winrt::to_string(_GlobalSettings.AcpAgent());
        if (idStr.starts_with("custom:"))
        {
            const auto bareId = winrt::to_hstring(idStr.substr(7));
            _GlobalSettings.AcpCustomCommand(L"");
            _isAddingCustomAcpAgent = false;
            _GlobalSettings.AcpAgent(bareId);
            // Remove custom entry from dropdown
            for (uint32_t i = 0; i < _acpAgentList.Size(); ++i)
            {
                if (winrt::to_string(_acpAgentList.GetAt(i).Id()) == idStr)
                {
                    _acpAgentList.RemoveAt(i);
                    break;
                }
            }
            _NotifyChanges(L"CurrentAcpAgent", L"IsAddingCustomAcpAgent", L"IsCustomAcpAgentSelected", L"ShowAcpModel");
        }
    }

    void AIAgentsViewModel::DeleteCustomDelegateAgent()
    {
        auto idStr = winrt::to_string(_GlobalSettings.DelegateAgent());
        if (idStr.starts_with("custom:"))
        {
            const auto bareId = winrt::to_hstring(idStr.substr(7));
            _GlobalSettings.DelegateCustomCommand(L"");
            _isAddingCustomDelegateAgent = false;
            _GlobalSettings.DelegateAgent(bareId);
            for (uint32_t i = 0; i < _delegateAgentList.Size(); ++i)
            {
                if (winrt::to_string(_delegateAgentList.GetAt(i).Id()) == idStr)
                {
                    _delegateAgentList.RemoveAt(i);
                    break;
                }
            }
            _NotifyChanges(L"CurrentDelegateAgent", L"IsAddingCustomDelegateAgent", L"IsCustomDelegateAgentSelected", L"ShowDelegateModel");
        }
    }

    // ── Auto error detection ───────────────────────────────────────────────

    bool AIAgentsViewModel::AutoErrorDetectionEnabled() const
    {
        return _GlobalSettings.EffectiveAutoErrorDetectionEnabled();
    }

    void AIAgentsViewModel::AutoErrorDetectionEnabled(bool value)
    {
        if (_GlobalSettings.AutoErrorDetectionEnabled() == value) return;
        _GlobalSettings.AutoErrorDetectionEnabled(value);
        // Master-detail: detection drives both the suggestion toggle's enabled
        // state (CanSuggestErrors) and its effective value (EffectiveAutoFix
        // Enabled flips to false when detection is off), so refresh both. The
        // stored autoFixEnabled preference is preserved, so re-enabling
        // detection restores the previous suggestion value rather than forcing
        // it on.
        _NotifyChanges(L"HasAutoErrorDetectionEnabled", L"AutoErrorDetectionEnabled",
                       L"CanSuggestErrors", L"AutoFixEnabled");
        // Shell integration installation is triggered on Save, not on toggle.
    }

    bool AIAgentsViewModel::HasAutoErrorDetectionEnabled() const
    {
        return _GlobalSettings.HasAutoErrorDetectionEnabled();
    }

    // ── AutoFix (auto-suggest) ─────────────────────────────────────────────

    bool AIAgentsViewModel::AutoFixEnabled() const
    {
        // Master-detail: suggestion follows detection. EffectiveAutoFixEnabled
        // returns false whenever detection is off (or GPO blocks autofix), so
        // the toggle reads Off when the master is off; when detection is on it
        // reflects the user's stored autoFixEnabled preference.
        return _GlobalSettings.EffectiveAutoFixEnabled();
    }

    void AIAgentsViewModel::AutoFixEnabled(bool value)
    {
        // Reject writes when policy blocks autofix or detection is off (the
        // toggle is disabled in those cases, but guard against races).
        if (_GlobalSettings.IsAutoFixPolicyLocked() ||
            !_GlobalSettings.EffectiveAutoErrorDetectionEnabled())
        {
            return;
        }
        if (_GlobalSettings.AutoFixEnabled() == value) return;
        _GlobalSettings.AutoFixEnabled(value);
        _NotifyChanges(L"HasAutoFixEnabled", L"AutoFixEnabled");
        // Shell integration installation is now triggered on Save, not on toggle.
    }

    bool AIAgentsViewModel::HasAutoFixEnabled() const
    {
        return _GlobalSettings.HasAutoFixEnabled();
    }

    bool AIAgentsViewModel::CanSuggestErrors() const
    {
        return !_GlobalSettings.IsAutoFixPolicyLocked() &&
               _GlobalSettings.EffectiveAutoErrorDetectionEnabled();
    }

    // ── Pane position ────────────────────────────────────────────────────

    IObservableVector<Editor::EnumEntry> AIAgentsViewModel::AgentPanePositionList()
    {
        return _agentPanePositionList;
    }

    winrt::Windows::Foundation::IInspectable AIAgentsViewModel::CurrentAgentPanePosition()
    {
        const auto pos = _GlobalSettings.AgentPanePosition();
        if (_agentPanePositionMap.HasKey(pos))
        {
            return winrt::box_value(_agentPanePositionMap.Lookup(pos));
        }
        return winrt::box_value(_agentPanePositionMap.Lookup(L"bottom"));
    }

    void AIAgentsViewModel::CurrentAgentPanePosition(const winrt::Windows::Foundation::IInspectable& value)
    {
        if (auto ee = value.try_as<Editor::EnumEntry>())
        {
            auto pos = winrt::unbox_value<winrt::hstring>(ee.EnumValue());
            if (_GlobalSettings.AgentPanePosition() != pos)
            {
                _GlobalSettings.AgentPanePosition(pos);
                _NotifyChanges(L"CurrentAgentPanePosition");
            }
        }
    }

    // ── Agent Hooks ──────────────────────────────────────────────────────
    //
    // Source of truth is `wta hooks status --json` (see Track 2 / wta's
    // agent_hooks_installer.rs). We spawn it on a background thread,
    // capture stdout, and feed the response into the pure parser at
    // src/cascadia/inc/AgentHooksStatus.h. Same JSON contract that
    // build/scripts/Verify-AgentHooks.ps1 consumes — so the Settings UI
    // and the verify script can never disagree about install state.
    //
    // The single primary "Install hooks" button still delegates to
    // `wta install-hooks`; afterwards we re-invoke the status query to
    // refresh the rows.

    // _ResolveWtaExePath and _RunWtaCaptureStdout moved to
    // src/cascadia/inc/WtaProcess.h for shared use.

    // "Fully installed" mirrors AgentHooks::FormatCliStatusLine's gating —
    // when every piece is in place we hide the subtitle so the row shows
    // just the CLI name + Remove button (clean state). Anything looser is
    // still a removable state on disk and is surfaced via the subtitle.
    static bool _IsHooksFullyInstalled(const ::Microsoft::Terminal::AgentHooks::CliStatus* cli)
    {
        return cli &&
               cli->marketplaceRegistered &&
               cli->marketplacePathValid &&
               cli->pluginInstalled &&
               cli->pluginEnabled;
    }

    // Build the descriptor text for the row's subtitle: the post-em-dash
    // portion of FormatCliStatusLine. Returns empty when hooks are fully
    // installed or the CLI is absent with no hook state.
    static winrt::hstring _ComputeHooksSubtitle(const ::Microsoft::Terminal::AgentHooks::CliStatus* cli)
    {
        if (!cli)
        {
            return {};
        }
        if (!cli->marketplaceRegistered && !cli->pluginInstalled)
        {
            return {};
        }
        if (_IsHooksFullyInstalled(cli))
        {
            return {};
        }

        std::wstring text = L"partially installed (";
        bool first = true;
        const auto append = [&](std::wstring_view tag) {
            if (!first)
            {
                text += L", ";
            }
            text += tag;
            first = false;
        };
        append(cli->marketplaceRegistered ? L"marketplace registered" : L"marketplace missing");
        append(cli->pluginInstalled ? L"plugin installed" : L"plugin missing");
        if (cli->pluginInstalled && !cli->pluginEnabled)
        {
            append(L"plugin disabled");
        }
        if (cli->marketplaceRegistered && !cli->marketplacePathValid)
        {
            append(L"marketplace path stale");
        }
        text += L")";
        if (cli->detectionFallback.has_value())
        {
            text += L" (filesystem fallback)";
        }
        return winrt::hstring{ text };
    }

    void AIAgentsViewModel::_ApplyStatusReport(const std::optional<::Microsoft::Terminal::AgentHooks::StatusReport>& report)
    {
        namespace AgentHooks = ::Microsoft::Terminal::AgentHooks;
        using AgentHooks::CliStatus;
        using AgentHooks::FindCli;

        if (!report.has_value())
        {
            // wta unavailable — collapse all rows; the Install action up top
            // still works (or fails loudly) so the user has a path forward.
            _copilotCliDetected = false;
            _claudeCliDetected = false;
            _geminiCliDetected = false;
            _codexCliDetected = false;
            _openCodeCliDetected = false;
            _showCopilotHookRow = false;
            _showClaudeHookRow = false;
            _showGeminiHookRow = false;
            _showCodexHookRow = false;
            _showOpenCodeHookRow = false;
            _openCodeHooksPresent = false;
            _copilotHooksSubtitle = {};
            _claudeHooksSubtitle = {};
            _geminiHooksSubtitle = {};
            _codexHooksSubtitle = {};
            _openCodeHooksSubtitle = {};
        }
        else
        {
            const auto* copilot = FindCli(*report, "copilot");
            const auto* claude = FindCli(*report, "claude");
            const auto* gemini = FindCli(*report, "gemini");
            const auto* codex = FindCli(*report, "codex");
            const auto* openCode = FindCli(*report, "opencode");

            _copilotCliDetected = copilot && copilot->binaryOnPath;
            _claudeCliDetected = claude && claude->binaryOnPath;
            _geminiCliDetected = gemini && gemini->binaryOnPath;
            _codexCliDetected = codex && codex->binaryOnPath;
            _openCodeCliDetected = openCode && openCode->binaryOnPath;

            _showCopilotHookRow = AgentHooks::HasHookState(copilot);
            _showClaudeHookRow = AgentHooks::HasHookState(claude);
            _showGeminiHookRow = AgentHooks::HasHookState(gemini);
            _showCodexHookRow = AgentHooks::HasHookState(codex);
            _openCodeHooksPresent = AgentHooks::HasHookState(openCode);
            _showOpenCodeHookRow = AgentHooks::ShouldShowDetectedOrConfiguredHookRow(openCode);

            _copilotHooksSubtitle = _ComputeHooksSubtitle(copilot);
            _claudeHooksSubtitle = _ComputeHooksSubtitle(claude);
            _geminiHooksSubtitle = _ComputeHooksSubtitle(gemini);
            _codexHooksSubtitle = _ComputeHooksSubtitle(codex);
            _openCodeHooksSubtitle = _ComputeHooksSubtitle(openCode);
        }

        _NotifyChanges(L"IsCopilotCliDetected",
                       L"IsClaudeCliDetected",
                       L"IsGeminiCliDetected",
                       L"IsCodexCliDetected",
                       L"IsOpenCodeCliDetected",
                       L"IsAnyAgentCliDetected",
                       L"CanInstallAgentHooks",
                       L"CanRemoveAgentHooks",
                       L"CanRemoveOpenCodeHooks",
                       L"ShowCopilotHookRow",
                       L"ShowClaudeHookRow",
                       L"ShowGeminiHookRow",
                       L"ShowCodexHookRow",
                       L"ShowOpenCodeHookRow",
                       L"CopilotHooksSubtitle",
                       L"ClaudeHooksSubtitle",
                       L"GeminiHooksSubtitle",
                       L"CodexHooksSubtitle",
                       L"OpenCodeHooksSubtitle",
                       L"ShowCopilotHooksSubtitle",
                       L"ShowClaudeHooksSubtitle",
                       L"ShowGeminiHooksSubtitle",
                       L"ShowCodexHooksSubtitle",
                       L"ShowOpenCodeHooksSubtitle");
    }

    void AIAgentsViewModel::RefreshAgentHooksStatus()
    {
        if (_refreshingAgentHooks)
        {
            return;
        }
        _refreshingAgentHooks = true;
        _RefreshAgentHooksStatusAsync();
    }

    winrt::fire_and_forget AIAgentsViewModel::_RefreshAgentHooksStatusAsync()
    {
        auto strongThis = get_strong();
        auto dispatcher = winrt::Windows::UI::Xaml::Window::Current().Dispatcher();

        co_await winrt::resume_background();

        const auto wtaPath = ::Microsoft::Terminal::WtaProcess::ResolveWtaExePath();
        const auto stdoutText = ::Microsoft::Terminal::WtaProcess::RunWtaCaptureStdout(wtaPath, L"hooks status --json", 30'000);
        auto report = ::Microsoft::Terminal::AgentHooks::ParseStatusJson(stdoutText);

        co_await wil::resume_foreground(dispatcher);

        _ApplyStatusReport(report);
        _refreshingAgentHooks = false;
    }

    void AIAgentsViewModel::InstallAllAgentHooks()
    {
        if (_installingAgentHooks || IsAgentSessionHooksPolicyLocked()) return;
        _installingAgentHooks = true;
        _agentHooksInstallSummary = RS_(L"AIAgents_HooksInstallingSummary");
        _NotifyChanges(L"IsInstallingAgentHooks", L"AgentHooksInstallSummary", L"HasAgentHooksInstallSummary");
        _RunHooksWtaAsync(L"hooks install");
    }

    void AIAgentsViewModel::RemoveCopilotHooks()
    {
        if (_installingAgentHooks) return;
        _installingAgentHooks = true;
        _agentHooksInstallSummary = RS_(L"AIAgents_HooksRemovingCopilotSummary");
        _NotifyChanges(L"IsInstallingAgentHooks", L"AgentHooksInstallSummary", L"HasAgentHooksInstallSummary");
        _RunHooksWtaAsync(L"hooks uninstall --cli copilot");
    }

    void AIAgentsViewModel::RemoveClaudeHooks()
    {
        if (_installingAgentHooks) return;
        _installingAgentHooks = true;
        _agentHooksInstallSummary = RS_(L"AIAgents_HooksRemovingClaudeSummary");
        _NotifyChanges(L"IsInstallingAgentHooks", L"AgentHooksInstallSummary", L"HasAgentHooksInstallSummary");
        _RunHooksWtaAsync(L"hooks uninstall --cli claude");
    }

    void AIAgentsViewModel::RemoveGeminiHooks()
    {
        if (_installingAgentHooks) return;
        _installingAgentHooks = true;
        _agentHooksInstallSummary = RS_(L"AIAgents_HooksRemovingGeminiSummary");
        _NotifyChanges(L"IsInstallingAgentHooks", L"AgentHooksInstallSummary", L"HasAgentHooksInstallSummary");
        _RunHooksWtaAsync(L"hooks uninstall --cli gemini");
    }

    void AIAgentsViewModel::RemoveCodexHooks()
    {
        if (_installingAgentHooks) return;
        _installingAgentHooks = true;
        _agentHooksInstallSummary = RS_(L"AIAgents_HooksRemovingCodexSummary");
        _NotifyChanges(L"IsInstallingAgentHooks", L"AgentHooksInstallSummary", L"HasAgentHooksInstallSummary");
        _RunHooksWtaAsync(L"hooks uninstall --cli codex");
    }

    void AIAgentsViewModel::RemoveOpenCodeHooks()
    {
        if (_installingAgentHooks) return;
        _installingAgentHooks = true;
        _agentHooksInstallSummary = RS_(L"AIAgents_HooksRemovingOpenCodeSummary");
        _NotifyChanges(L"IsInstallingAgentHooks", L"AgentHooksInstallSummary", L"HasAgentHooksInstallSummary");
        _RunHooksWtaAsync(L"hooks uninstall --cli opencode");
    }

    winrt::fire_and_forget AIAgentsViewModel::_RunHooksWtaAsync(std::wstring wtaArgs)
    {
        auto strongThis = get_strong();
        // Capture dispatcher synchronously while we're still on the calling
        // (UI) thread.
        auto dispatcher = winrt::Windows::UI::Xaml::Window::Current().Dispatcher();

        // Tailor the summary message to the action: callers pass either
        // `hooks install...` or `hooks uninstall...` and we surface a
        // matching success/failure line in the expander.
        const bool isUninstall = wtaArgs.find(L"uninstall") != std::wstring::npos;
        const std::wstring locateWtaFailedSummary{ RS_(L"AIAgents_HooksLocateWtaFailedSummary") };
        const std::wstring hooksRemovedSummary{ RS_(L"AIAgents_HooksRemovedSummary") };
        const std::wstring hooksInstalledSummary{ RS_(L"AIAgents_HooksInstalledSummary") };
        const std::wstring hooksRemovalFailedSummary{ RS_(L"AIAgents_HooksRemovalFailedSummary") };
        const std::wstring hooksInstallationFailedSummary{ RS_(L"AIAgents_HooksInstallationFailedSummary") };
        std::wstring summary;
        bool ok = false;

        co_await winrt::resume_background();

        const auto wtaPath = ::Microsoft::Terminal::WtaProcess::ResolveWtaExePath();
        if (wtaPath.empty())
        {
            summary = locateWtaFailedSummary;
        }
        else
        {
            ok = ::Microsoft::Terminal::WtaProcess::RunWtaAndWait(wtaPath, wtaArgs, 60'000);
            if (ok)
            {
                summary = isUninstall ? hooksRemovedSummary : hooksInstalledSummary;
            }
            else
            {
                summary = isUninstall ? hooksRemovalFailedSummary : hooksInstallationFailedSummary;
            }
        }

        co_await wil::resume_foreground(dispatcher);

        _installingAgentHooks = false;
        _agentHooksInstallSummary = winrt::hstring{ summary };
        _NotifyChanges(L"IsInstallingAgentHooks", L"AgentHooksInstallSummary", L"HasAgentHooksInstallSummary");
        // Refresh detection / install state regardless of success so the
        // status rows reflect what's now on disk.
        RefreshAgentHooksStatus();
        (void)ok;
    }

    // ACP model probe.
    //
    // After the user picks a new ACP agent in Settings, repopulate the
    // model dropdown without waiting for an agent pane rebuild —
    // pane-side `connection.Start()` only runs once the pane's
    // TermControl lays out, which requires the user to navigate to the
    // owning tab. Instead spawn `wta.exe probe-models --agent <cmdline>`,
    // which does an ACP handshake, prints `NewSessionResponse.models`
    // as JSON, and exits. `SetAvailableModels` fires the Changed event
    // which `_RebuildAcpModelListFromCache` is subscribed to.

    std::wstring AIAgentsViewModel::_ResolveEffectiveAcpAgentCmdline() const
    {
        // Mirror of TerminalPage::_ResolveEffectiveAgentCliPath — kept
        // here because the Settings UI project can't include TerminalApp
        // headers. Drift between the two is a real bug (probe would
        // hit a different agent than the pane will eventually launch).
        // Use the policy-aware getter so probes respect GPO.
        const auto acpAgent = _GlobalSettings.EffectiveAcpAgent();
        if (acpAgent.empty())
        {
            return {};
        }

        if (winrt::to_string(acpAgent).starts_with("custom:"))
        {
            const auto customCmd = _GlobalSettings.AcpCustomCommand();
            if (!customCmd.empty())
            {
                return std::wstring{ customCmd };
            }
        }

        const auto lower = winrt::to_string(acpAgent);

        if (lower == "claude")
        {
            return L"npx -y @agentclientprotocol/claude-agent-acp";
        }
        if (lower == "codex")
        {
            return L"npx -y @agentclientprotocol/codex-acp@1.1.0";
        }
        if (lower == "opencode")
        {
            return L"opencode acp";
        }

        std::wstring cmd{ acpAgent };
        if (lower == "copilot")
        {
            cmd += L" --acp --stdio";
        }
        else if (lower == "gemini")
        {
            cmd += L" --experimental-acp";
        }

        if (lower == "copilot" || lower == "gemini")
        {
            const auto acpModel = _GlobalSettings.AcpModel();
            if (!acpModel.empty() && !::Microsoft::Terminal::CustomModels::IsCustomSelection(acpModel))
            {
                cmd += L" --model ";
                cmd += std::wstring_view{ acpModel };
            }
        }

        return cmd;
    }

    void AIAgentsViewModel::_TriggerAcpModelProbe()
    {
        const auto agentId = _GlobalSettings.EffectiveAcpAgent();
        const auto cmdline = _ResolveEffectiveAcpAgentCmdline();
        if (agentId.empty() || cmdline.empty())
        {
            return;
        }

        // Bump generation BEFORE flipping the flag so any in-flight
        // probe (which captured the old value) drops its result on
        // the generation check.
        ++_acpProbeGeneration;
        _acpProbing = true;
        _RebuildAcpModelListFromCache();
        const auto cacheRevision = Model::AcpRuntimeState::Current().Revision(agentId);
        _RunAcpModelProbeAsync(agentId, cmdline, _acpProbeGeneration, cacheRevision);
    }

    winrt::fire_and_forget AIAgentsViewModel::_RunAcpModelProbeAsync(winrt::hstring agentId, std::wstring agentCmdline, uint64_t generation, uint64_t cacheRevision)
    {
        auto strongThis = get_strong();
        auto dispatcher = winrt::Windows::UI::Xaml::Window::Current().Dispatcher();

        co_await winrt::resume_background();

        const auto wtaPath = ::Microsoft::Terminal::WtaProcess::ResolveWtaExePath();
        std::string stdoutText;
        if (!wtaPath.empty())
        {
            // Quote-escape internal `"` per Windows CRT rules.
            std::wstring escaped = agentCmdline;
            for (size_t pos = 0; (pos = escaped.find(L'"', pos)) != std::wstring::npos; pos += 2)
            {
                escaped.replace(pos, 1, L"\"\"");
            }
            const std::wstring args = L"probe-models --agent \"" + escaped + L"\"";
            // 40s ceiling matches probe.rs's internal limits (npx
            // initialize 25s + new_session 10s + slack). Cached
            // adapters return in <2s.
            stdoutText = ::Microsoft::Terminal::WtaProcess::RunWtaCaptureStdout(wtaPath, args, 40'000);
        }

        std::vector<Model::AcpModelInfo> parsed;
        winrt::hstring currentId;
        bool parseOk = false;
        if (!stdoutText.empty())
        {
            Json::Value root;
            Json::CharReaderBuilder rb;
            const std::unique_ptr<Json::CharReader> reader{ rb.newCharReader() };
            std::string errs;
            if (reader->parse(stdoutText.data(),
                              stdoutText.data() + stdoutText.size(),
                              &root,
                              &errs) &&
                root.isObject())
            {
                parseOk = true;
                if (const auto& models = root["available_models"]; models.isArray())
                {
                    parsed.reserve(models.size());
                    for (const auto& m : models)
                    {
                        if (!m.isObject()) continue;
                        const auto id = m.get("id", "").asString();
                        const auto name = m.get("name", "").asString();
                        const auto desc = m.isMember("description") && m["description"].isString()
                            ? m["description"].asString()
                            : std::string{};
                        if (id.empty()) continue;
                        parsed.emplace_back(
                            winrt::to_hstring(id),
                            winrt::to_hstring(name),
                            winrt::to_hstring(desc));
                    }
                }
                if (root.isMember("current_model_id") && root["current_model_id"].isString())
                {
                    currentId = winrt::to_hstring(root["current_model_id"].asString());
                }
            }
        }

        co_await wil::resume_foreground(dispatcher);

        // Drop stale results — a newer probe is already in flight
        // for a different agent and we'd clobber its eventual write.
        if (generation != _acpProbeGeneration)
        {
            co_return;
        }

        _acpProbing = false;

        if (parseOk)
        {
            auto view = winrt::single_threaded_vector(std::move(parsed)).GetView();
            if (!Model::AcpRuntimeState::Current().TrySetAvailableModels(agentId, cacheRevision, view, currentId))
            {
                _RebuildAcpModelListFromCache();
            }
        }
        else
        {
            _RebuildAcpModelListFromCache();
        }
    }
}
