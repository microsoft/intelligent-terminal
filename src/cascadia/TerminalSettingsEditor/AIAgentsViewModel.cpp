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
        static constexpr std::wstring_view knownIds[] = { L"copilot", L"gemini", L"claude", L"codex" };
        for (const auto& known : knownIds)
        {
            if (id == known) return true;
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

        // ACP-capable agents (shared list — see inc/AgentRegistry.h).
        // Skip agents whose CLI isn't installed — the dropdown only offers
        // choices the user can actually launch. If the persisted setting
        // names a missing agent, the SelectedItem fallback in
        // CurrentAcpAgent picks the "Add New" entry.
        std::vector<Editor::AgentEntry> acpEntries;
        for (const auto& a : Reg::BuiltinAcpAgents)
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
        _MaybeAppendCustomEntry(_acpAgentList, _GlobalSettings.AcpCustomCommand(), _GlobalSettings.AcpAgent());
        _AppendAddNewEntry(_acpAgentList);

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

        // Delegate agents (shared list — see inc/AgentRegistry.h).
        // Same install-filter rule as the ACP list above.
        std::vector<Editor::AgentEntry> delegateEntries;
        for (const auto& a : Reg::BuiltinDelegateAgents)
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
        _MaybeAppendCustomEntry(_delegateAgentList, _GlobalSettings.DelegateCustomCommand(), _GlobalSettings.DelegateAgent());
        _AppendAddNewEntry(_delegateAgentList);

        // Pane position list
        _agentPanePositionMap = winrt::single_threaded_map<winrt::hstring, Editor::EnumEntry>();
        std::vector<Editor::EnumEntry> posEntries;
        static constexpr std::pair<std::wstring_view, std::wstring_view> positions[] = {
            { L"Bottom", L"bottom" },
            { L"Right", L"right" },
            { L"Top", L"top" },
            { L"Left", L"left" },
        };
        for (const auto& [displayName, value] : positions)
        {
            auto entry = winrt::make<implementation::EnumEntry>(
                winrt::hstring{ displayName },
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
        const winrt::hstring detecting{ L"Detecting…" };
        _copilotHooksStatus = detecting;
        _claudeHooksStatus = detecting;
        _geminiHooksStatus = detecting;
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

        // Replace contents in-place so x:Bind observers stay attached.
        _acpModelList.Clear();
        for (uint32_t i = 0; i < newSize; ++i)
        {
            const auto m = cached.GetAt(i);
            _acpModelList.Append(winrt::make<AcpModelEntry>(
                m.Id(),
                m.DisplayName(),
                m.Description()));
        }

        _NotifyChanges(L"AcpModelList",
                       L"HasAcpModelList",
                       L"ShowAcpModelTextBox",
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
        return _StartsWithCustom(_GlobalSettings.AcpAgent());
    }

    winrt::hstring AIAgentsViewModel::CustomAcpCommandPreview()
    {
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
        const auto current = _GlobalSettings.AcpModel();
        if (!_acpModelList)
        {
            return nullptr;
        }
        for (uint32_t i = 0; i < _acpModelList.Size(); ++i)
        {
            const auto entry = _acpModelList.GetAt(i);
            if (entry.Id() == current)
            {
                return entry;
            }
        }
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
        return _FindEntryById(_acpAgentList, _GlobalSettings.AcpAgent());
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
            // Stale model list belongs to the previous agent. Clear the
            // process-wide cache so the dropdown empties immediately; wta
            // will repopulate it after the new agent's NewSessionResponse.
            // Also clear the bound model id so the next agent starts on its
            // default rather than carrying the previous agent's selection.
            _GlobalSettings.AcpModel(L"");
            Model::AcpRuntimeState::Current().SetAvailableModels(
                winrt::single_threaded_vector<Model::AcpModelInfo>().GetView(),
                L"");
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
        return _FindEntryById(_delegateAgentList, _GlobalSettings.DelegateAgent());
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
        _NotifyChanges(L"CurrentAcpAgent", L"IsAddingCustomAcpAgent", L"IsCustomAcpAgentSelected", L"ShowAcpModel", L"CustomAcpCommandPreview");
    }

    void AIAgentsViewModel::SaveCustomDelegateAgent()
    {
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
        return _GlobalSettings.AutoFixEnabled();
    }

    void AIAgentsViewModel::AutoFixEnabled(bool value)
    {
        if (_GlobalSettings.AutoFixEnabled() == value) return;
        _GlobalSettings.AutoFixEnabled(value);
        _NotifyChanges(L"HasAutoFixEnabled", L"AutoFixEnabled");
        if (value)
        {
            InitShellIntegrationRequested.raise(*this, ShellIntegrationTarget::Pwsh);
            InitShellIntegrationRequested.raise(*this, ShellIntegrationTarget::WindowsPowerShell);
        }
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

    std::wstring AIAgentsViewModel::_ResolveWtaExePath()
    {
        // Mirrors TerminalPage::_DetectWtaPath: prefer co-located wta.exe
        // (MSIX-installed scenario), fall back to walking up the running
        // module path looking for a dev build, then PATH.
        const auto modulePath = std::filesystem::path{ wil::GetModuleFileNameW<std::wstring>(nullptr) };
        const auto moduleDir = modulePath.parent_path();
        std::error_code ec;
        {
            const auto sibling = moduleDir / L"wta.exe";
            if (std::filesystem::exists(sibling, ec))
            {
                return sibling.lexically_normal().wstring();
            }
        }
        auto cursor = moduleDir;
        while (!cursor.empty())
        {
            for (const auto& relative : {
                     std::filesystem::path{ L"wta\\target\\debug\\wta.exe" },
                     std::filesystem::path{ L"wta\\target\\release\\wta.exe" },
                 })
            {
                const auto candidate = cursor / relative;
                if (std::filesystem::exists(candidate, ec))
                {
                    return candidate.lexically_normal().wstring();
                }
            }
            const auto parent = cursor.parent_path();
            if (parent == cursor) break;
            cursor = parent;
        }
        wchar_t buffer[MAX_PATH];
        if (SearchPathW(nullptr, L"wta", L".exe", MAX_PATH, buffer, nullptr) > 0)
        {
            return std::wstring{ buffer };
        }
        return {};
    }

    // Spawn `wta.exe <args>` and return its stdout on exit-0; empty
    // string otherwise. Synchronous; intended to be called from a
    // resume_background coroutine. Captures via an anonymous pipe with
    // child's stdout/stderr both routed to it (stderr swallowed
    // intentionally — we only care about the JSON payload, and any
    // human-readable error text on stderr would just confuse the
    // parser).
    std::string AIAgentsViewModel::_RunWtaCaptureStdout(const std::wstring& wtaPath,
                                                       const std::wstring& argsAfterExe,
                                                       DWORD timeoutMs)
    {
        if (wtaPath.empty())
        {
            return {};
        }

        SECURITY_ATTRIBUTES sa{};
        sa.nLength = sizeof(sa);
        sa.bInheritHandle = TRUE;

        wil::unique_handle readHandle;
        wil::unique_handle writeHandle;
        if (!CreatePipe(readHandle.addressof(), writeHandle.addressof(), &sa, 0))
        {
            return {};
        }
        // The read end must NOT be inherited by the child.
        if (!SetHandleInformation(readHandle.get(), HANDLE_FLAG_INHERIT, 0))
        {
            return {};
        }

        STARTUPINFOW si{};
        si.cb = sizeof(si);
        si.dwFlags = STARTF_USESHOWWINDOW | STARTF_USESTDHANDLES;
        si.wShowWindow = SW_HIDE;
        si.hStdOutput = writeHandle.get();
        si.hStdError = writeHandle.get();
        si.hStdInput = GetStdHandle(STD_INPUT_HANDLE);

        std::wstring cmdline = L"\"" + wtaPath + L"\" " + argsAfterExe;
        std::wstring mutableCmd = cmdline;

        PROCESS_INFORMATION pi{};
        const BOOL launched = CreateProcessW(
            wtaPath.c_str(),
            mutableCmd.data(),
            nullptr,
            nullptr,
            TRUE, // inherit handles (so the child inherits writeHandle)
            CREATE_NO_WINDOW,
            nullptr,
            nullptr,
            &si,
            &pi);
        if (!launched)
        {
            return {};
        }
        wil::unique_handle proc{ pi.hProcess };
        wil::unique_handle thread{ pi.hThread };

        // Close our copy of the write end so the read pipe sees EOF
        // when the child exits.
        writeHandle.reset();

        std::string captured;
        captured.reserve(4096);
        char buf[4096];
        for (;;)
        {
            DWORD bytesRead = 0;
            const BOOL ok = ReadFile(readHandle.get(), buf, sizeof(buf), &bytesRead, nullptr);
            if (!ok || bytesRead == 0)
            {
                break;
            }
            captured.append(buf, bytesRead);
        }

        if (WaitForSingleObject(proc.get(), timeoutMs) != WAIT_OBJECT_0)
        {
            // Best-effort terminate on timeout — we still return what
            // we captured (empty in practice).
            TerminateProcess(proc.get(), 1);
            WaitForSingleObject(proc.get(), 1000);
            return {};
        }
        DWORD exitCode = 1;
        GetExitCodeProcess(proc.get(), &exitCode);
        if (exitCode != 0)
        {
            return {};
        }
        return captured;
    }

    void AIAgentsViewModel::_ApplyStatusReport(const std::optional<::Microsoft::Terminal::AgentHooks::StatusReport>& report)
    {
        namespace AgentHooks = ::Microsoft::Terminal::AgentHooks;
        using AgentHooks::CliStatus;
        using AgentHooks::FindCli;
        using AgentHooks::FormatCliStatusLine;

        // Display strings + per-CLI detected flags. When the report is
        // missing (wta failed / not found / parse error) we surface a
        // single explanatory line per row instead of crashing or
        // silently leaving the previous text.
        if (!report.has_value())
        {
            const winrt::hstring unavailable{ L"Hook detection unavailable (wta.exe not found or status query failed)" };
            _copilotHooksStatus = unavailable;
            _claudeHooksStatus = unavailable;
            _geminiHooksStatus = unavailable;
            _copilotCliDetected = false;
            _claudeCliDetected = false;
            _geminiCliDetected = false;
        }
        else
        {
            const auto* copilot = FindCli(*report, "copilot");
            const auto* claude = FindCli(*report, "claude");
            const auto* gemini = FindCli(*report, "gemini");

            _copilotCliDetected = copilot && copilot->binaryOnPath;
            _claudeCliDetected = claude && claude->binaryOnPath;
            _geminiCliDetected = gemini && gemini->binaryOnPath;

            const auto missing = [](std::wstring_view name) {
                return winrt::hstring{ std::wstring{ name } + L" — not reported by wta" };
            };
            _copilotHooksStatus = copilot ? winrt::hstring{ FormatCliStatusLine(*copilot, L"Copilot CLI") } : missing(L"Copilot CLI");
            _claudeHooksStatus = claude ? winrt::hstring{ FormatCliStatusLine(*claude, L"Claude Code") } : missing(L"Claude Code");
            _geminiHooksStatus = gemini ? winrt::hstring{ FormatCliStatusLine(*gemini, L"Gemini CLI") } : missing(L"Gemini CLI");
        }

        _NotifyChanges(L"IsCopilotCliDetected",
                       L"IsClaudeCliDetected",
                       L"IsGeminiCliDetected",
                       L"IsAnyAgentCliDetected",
                       L"CopilotHooksStatusText",
                       L"ClaudeHooksStatusText",
                       L"GeminiHooksStatusText");
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

        const auto wtaPath = _ResolveWtaExePath();
        const auto stdoutText = _RunWtaCaptureStdout(wtaPath, L"hooks status --json", 30'000);
        auto report = ::Microsoft::Terminal::AgentHooks::ParseStatusJson(stdoutText);

        co_await wil::resume_foreground(dispatcher);

        _ApplyStatusReport(report);
        _refreshingAgentHooks = false;
    }

    void AIAgentsViewModel::InstallAgentHooks()
    {
        if (_installingAgentHooks) return;
        _installingAgentHooks = true;
        _agentHooksInstallSummary = winrt::hstring{ L"Installing hooks..." };
        _NotifyChanges(L"IsInstallingAgentHooks", L"AgentHooksInstallSummary");
        _RunHooksInstallerAsync();
    }

    winrt::fire_and_forget AIAgentsViewModel::_RunHooksInstallerAsync()
    {
        auto strongThis = get_strong();
        // Capture dispatcher synchronously while we're still on the calling
        // (UI) thread.
        auto dispatcher = winrt::Windows::UI::Xaml::Window::Current().Dispatcher();

        std::wstring summary;
        bool ok = false;

        co_await winrt::resume_background();

        const auto wtaPath = _ResolveWtaExePath();
        if (wtaPath.empty())
        {
            summary = L"Failed: could not locate wta.exe";
        }
        else
        {
            std::wstring cmdline = L"\"" + wtaPath + L"\" install-hooks";

            STARTUPINFOW si{};
            si.cb = sizeof(si);
            si.dwFlags = STARTF_USESHOWWINDOW;
            si.wShowWindow = SW_HIDE;
            PROCESS_INFORMATION pi{};
            std::wstring mutableCmd = cmdline;
            const BOOL launched = CreateProcessW(
                wtaPath.c_str(),
                mutableCmd.data(),
                nullptr,
                nullptr,
                FALSE,
                CREATE_NO_WINDOW,
                nullptr,
                nullptr,
                &si,
                &pi);
            if (!launched)
            {
                const auto err = GetLastError();
                summary = L"Failed to launch installer (error " + std::to_wstring(err) + L")";
            }
            else
            {
                WaitForSingleObject(pi.hProcess, 60'000);
                DWORD exitCode = 1;
                GetExitCodeProcess(pi.hProcess, &exitCode);
                CloseHandle(pi.hThread);
                CloseHandle(pi.hProcess);
                if (exitCode == 0)
                {
                    ok = true;
                    summary = L"Hooks installed successfully. Restart any open agent CLIs to pick up the new hooks.";
                }
                else
                {
                    summary = L"Installer exited with code " + std::to_wstring(exitCode);
                }
            }
        }

        co_await wil::resume_foreground(dispatcher);

        _installingAgentHooks = false;
        _agentHooksInstallSummary = winrt::hstring{ summary };
        _NotifyChanges(L"IsInstallingAgentHooks", L"AgentHooksInstallSummary");
        // Refresh detection / install state regardless of success so the
        // status rows reflect what's now on disk.
        RefreshAgentHooksStatus();
        (void)ok;
    }
}
