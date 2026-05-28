// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "AIAgentsViewModel.h"
#include "AIAgentsViewModel.g.cpp"
#include "AcpModelEntry.g.cpp"
#include "AgentEntry.g.cpp"
#include "EnumEntry.h"
#include "../inc/AgentRegistry.h"
#include "../inc/AgentHooksStatus.h"
#include "../inc/WtaProcess.h"

#include <json/json.h>

using namespace winrt::Windows::Foundation;
using namespace winrt::Windows::Foundation::Collections;
using namespace winrt::Microsoft::Terminal::Settings::Model;

namespace winrt::Microsoft::Terminal::Settings::Editor::implementation
{
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
        const auto str = winrt::to_string(command);
        const auto pos = str.find(' ');
        auto token = (pos != std::string::npos) ? str.substr(0, pos) : str;
        auto slash = token.rfind('\\');
        if (slash == std::string::npos) slash = token.rfind('/');
        if (slash != std::string::npos) token = token.substr(slash + 1);
        for (const auto* ext : { ".exe", ".cmd", ".bat" })
        {
            if (token.size() > strlen(ext) && token.substr(token.size() - strlen(ext)) == ext)
            {
                token = token.substr(0, token.size() - strlen(ext));
                break;
            }
        }
        return winrt::to_hstring(token);
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
        const auto settingsId = isBuiltIn
            ? winrt::hstring{ L"custom:" + std::wstring_view{ bareId } }
            : bareId;
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

    void AIAgentsViewModel::_RebuildAcpModelListFromCache()
    {
        if (!_acpModelList) return;

        const auto cached = Model::AcpRuntimeState::Current().AvailableModels();
        const uint32_t newSize = cached ? cached.Size() : 0;

        // Mirror the agent's advertised list 1:1 — each ACP agent
        // already publishes its own "use the default" entry (claude
        // calls it `default`, copilot `auto`), so synthesizing one
        // here would just duplicate it.
        _acpModelList.Clear();
        for (uint32_t i = 0; i < newSize; ++i)
        {
            const auto m = cached.GetAt(i);
            _acpModelList.Append(winrt::make<AcpModelEntry>(
                m.Id(),
                m.DisplayName(),
                m.Description()));
        }

        // Reconcile a *stale* persisted id with the authoritative list.
        // Only fires when the user has actively configured a specific model
        // (non-empty) that this agent doesn't advertise — e.g. switching
        // agents leaves a leftover id. In that case rewrite to the agent's
        // advertised default ("default" / "auto") so the dropdown and the
        // runtime --acp-model value stay in sync.
        //
        // We *don't* reconcile when the persisted id is empty: empty is a
        // legitimate "use whatever default the agent picks" sentinel that
        // the user never typed in, and silently writing "default" into
        // settings.json would be surprising. The visual fallback for that
        // case lives in CurrentAcpModelEntry().
        if (newSize > 0)
        {
            const auto current = _GlobalSettings.AcpModel();
            if (!current.empty())
            {
                bool matched = false;
                for (uint32_t i = 0; i < newSize; ++i)
                {
                    if (_acpModelList.GetAt(i).Id() == current)
                    {
                        matched = true;
                        break;
                    }
                }
                if (!matched)
                {
                    winrt::hstring fallbackId{};
                    for (uint32_t i = 0; i < newSize; ++i)
                    {
                        const auto id = _acpModelList.GetAt(i).Id();
                        if (id == L"default" || id == L"auto")
                        {
                            fallbackId = id;
                            break;
                        }
                    }
                    if (fallbackId.empty())
                    {
                        fallbackId = _acpModelList.GetAt(0).Id();
                    }
                    if (_GlobalSettings.AcpModel() != fallbackId)
                    {
                        _GlobalSettings.AcpModel(fallbackId);
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
        const auto current = _GlobalSettings.AcpModel();
        for (uint32_t i = 0; i < _acpModelList.Size(); ++i)
        {
            const auto entry = _acpModelList.GetAt(i);
            if (entry.Id() == current) return entry;
        }
        // Visual-only fallback for the unconfigured case: the user has
        // never picked a model (empty persisted id), so we surface the
        // agent's advertised default entry as the selected item instead
        // of letting the ComboBox render dim PlaceholderText. We *don't*
        // write to settings here — leaving the empty id in place keeps
        // wta's "use the agent's own default" semantics intact.
        // The stale-id case (non-empty + no match) is handled at the
        // data layer by _RebuildAcpModelListFromCache.
        if (current.empty() && _acpModelList.Size() > 0)
        {
            for (uint32_t i = 0; i < _acpModelList.Size(); ++i)
            {
                const auto entry = _acpModelList.GetAt(i);
                const auto id = entry.Id();
                if (id == L"default" || id == L"auto") return entry;
            }
            return _acpModelList.GetAt(0);
        }
        // Empty list (probe hasn't run yet) → ComboBox shows PlaceholderText
        // briefly until the agent's model list arrives.
        return nullptr;
    }

    void AIAgentsViewModel::CurrentAcpModelEntry(const Editor::AcpModelEntry& value)
    {
        if (!value)
        {
            return;
        }
        if (_GlobalSettings.AcpModel() != value.Id())
        {
            _GlobalSettings.AcpModel(value.Id());
            _NotifyChanges(L"AcpModel", L"CurrentAcpModelEntry");
        }
    }

    bool AIAgentsViewModel::ShowAcpModel()
    {
        if (_isAddingCustomAcpAgent) return false;
        if (_StartsWithCustom(_GlobalSettings.AcpAgent())) return false;
        return _IsKnownAgent(_GlobalSettings.AcpAgent());
    }

    bool AIAgentsViewModel::ShowDelegateModel()
    {
        if (_isAddingCustomDelegateAgent) return false;
        if (_StartsWithCustom(_GlobalSettings.DelegateAgent())) return false;
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
            // Drop the previous agent's model id and cached list — they
            // don't apply to the new agent. The probe below repopulates
            // the cache.
            _GlobalSettings.AcpModel(L"");
            Model::AcpRuntimeState::Current().SetAvailableModels(
                winrt::single_threaded_vector<Model::AcpModelInfo>().GetView(),
                L"");
            _TriggerAcpModelProbe();
            _NotifyChanges(L"CurrentAcpAgent",
                           L"IsAddingCustomAcpAgent",
                           L"IsCustomAcpAgentSelected",
                           L"ShowAcpModel",
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
        _GlobalSettings.AcpCustomCommand(_customAcpCommand);

        const bool isBuiltIn = _IsKnownAgent(bareId);
        const auto settingsId = isBuiltIn
            ? winrt::hstring{ L"custom:" + std::wstring_view{ bareId } }
            : bareId;
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
        // Same cache reset as the built-in dropdown path above.
        _GlobalSettings.AcpModel(L"");
        Model::AcpRuntimeState::Current().SetAvailableModels(
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
        _GlobalSettings.DelegateCustomCommand(_customDelegateCommand);

        const bool isBuiltIn = _IsKnownAgent(bareId);
        const auto settingsId = isBuiltIn
            ? winrt::hstring{ L"custom:" + std::wstring_view{ bareId } }
            : bareId;
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

    // ── AutoFix ──────────────────────────────────────────────────────────

    bool AIAgentsViewModel::AutoFixEnabled() const
    {
        return _GlobalSettings.EffectiveAutoFixEnabled();
    }

    void AIAgentsViewModel::AutoFixEnabled(bool value)
    {
        // Reject writes when policy blocks autofix.
        if (_GlobalSettings.IsAutoFixPolicyLocked())
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
    // portion of FormatCliStatusLine. Returns empty when the CLI has no
    // hook state on disk (row is hidden) OR when it's fully installed
    // (row is shown without a subtitle).
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
            _showCopilotHookRow = false;
            _showClaudeHookRow = false;
            _showGeminiHookRow = false;
            _copilotHooksSubtitle = {};
            _claudeHooksSubtitle = {};
            _geminiHooksSubtitle = {};
        }
        else
        {
            const auto* copilot = FindCli(*report, "copilot");
            const auto* claude = FindCli(*report, "claude");
            const auto* gemini = FindCli(*report, "gemini");

            _copilotCliDetected = copilot && copilot->binaryOnPath;
            _claudeCliDetected = claude && claude->binaryOnPath;
            _geminiCliDetected = gemini && gemini->binaryOnPath;

            const auto hasState = [](const CliStatus* cli) {
                return cli && (cli->marketplaceRegistered || cli->pluginInstalled);
            };
            _showCopilotHookRow = hasState(copilot);
            _showClaudeHookRow = hasState(claude);
            _showGeminiHookRow = hasState(gemini);

            _copilotHooksSubtitle = _ComputeHooksSubtitle(copilot);
            _claudeHooksSubtitle = _ComputeHooksSubtitle(claude);
            _geminiHooksSubtitle = _ComputeHooksSubtitle(gemini);
        }

        _NotifyChanges(L"IsCopilotCliDetected",
                       L"IsClaudeCliDetected",
                       L"IsGeminiCliDetected",
                       L"IsAnyAgentCliDetected",
                       L"CanInstallAgentHooks",
                       L"ShowCopilotHookRow",
                       L"ShowClaudeHookRow",
                       L"ShowGeminiHookRow",
                       L"CopilotHooksSubtitle",
                       L"ClaudeHooksSubtitle",
                       L"GeminiHooksSubtitle",
                       L"ShowCopilotHooksSubtitle",
                       L"ShowClaudeHooksSubtitle",
                       L"ShowGeminiHooksSubtitle");
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
            return L"npx -y @zed-industries/claude-code-acp";
        }
        if (lower == "codex")
        {
            return L"npx -y @zed-industries/codex-acp";
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
            if (!acpModel.empty())
            {
                cmd += L" --model ";
                cmd += std::wstring_view{ acpModel };
            }
        }

        return cmd;
    }

    void AIAgentsViewModel::_TriggerAcpModelProbe()
    {
        const auto cmdline = _ResolveEffectiveAcpAgentCmdline();
        if (cmdline.empty())
        {
            return;
        }

        // Bump generation BEFORE flipping the flag so any in-flight
        // probe (which captured the old value) drops its result on
        // the generation check.
        ++_acpProbeGeneration;
        _acpProbing = true;
        _RebuildAcpModelListFromCache();
        _RunAcpModelProbeAsync(cmdline, _acpProbeGeneration);
    }

    winrt::fire_and_forget AIAgentsViewModel::_RunAcpModelProbeAsync(std::wstring agentCmdline, uint64_t generation)
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
            Model::AcpRuntimeState::Current().SetAvailableModels(view, currentId);
        }
        else
        {
            _RebuildAcpModelListFromCache();
        }
    }
}
