// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// This file contains the protocol bridge methods for TerminalPage.
// These methods are called by the TerminalProtocolComServer to query
// and mutate terminal state. They return typed WinRT structs across
// the DLL boundary.
//
// IMPORTANT: These methods are called from background threads (COM).
// All access to UI state must be marshaled to the UI thread via Dispatcher().
// Each method is a direct coroutine that uses co_await to switch threads.
// The ComServer calls .get() on the returned IAsyncOperation to block.

#include "pch.h"
#include "TerminalPage.h"
#include "../../types/inc/utils.hpp"
#include "../inc/IntelligentTerminalPaths.h"
#include "../TerminalSettingsAppAdapterLib/TerminalSettings.h"

#include <array>
#include <chrono>
#include <filesystem>
#include <fstream>
#include <json/json.h>
#include <sddl.h>
#include <wil/resource.h>
#include <wil/stl.h>
#include "../TerminalProtocol/ProtocolParsing.h"

namespace ProtocolParsing = Microsoft::Terminal::Protocol::Parsing;

using namespace winrt;
using namespace winrt::Windows::Foundation;
using namespace winrt::Windows::UI::Core;
using namespace winrt::Microsoft::Terminal;
using namespace winrt::Microsoft::Terminal::Control;
using namespace winrt::Microsoft::Terminal::TerminalConnection;
using namespace winrt::Microsoft::Terminal::Settings::Model;
namespace Protocol = winrt::Microsoft::Terminal::Protocol;
namespace Model = winrt::Microsoft::Terminal::Settings::Model;

namespace winrt::TerminalApp::implementation
{
    // Helper to get PID from a pane's terminal control connection.
    static uint32_t _getPidFromPane(const std::shared_ptr<Pane>& pane)
    {
        if (const auto termControl = pane->GetTerminalControl())
        {
            const auto conn = termControl.Connection();
            if (conn)
            {
                if (const auto conpty = conn.try_as<ConptyConnection>())
                {
                    const auto handle = conpty.RootProcessHandle();
                    if (handle)
                    {
                        return static_cast<uint32_t>(GetProcessId(reinterpret_cast<HANDLE>(handle)));
                    }
                }
            }
        }
        return 0;
    }

    // Get the connection SessionId for a terminal pane, or empty guid for non-terminal panes.
    static winrt::guid _getSessionIdFromPane(const std::shared_ptr<Pane>& pane)
    {
        if (const auto termContent = pane->GetContent().try_as<TerminalApp::TerminalPaneContent>())
        {
            if (const auto control = termContent.GetTermControl())
            {
                if (const auto conn = control.Connection())
                {
                    return conn.SessionId();
                }
            }
        }
        return {};
    }

    static bool _initializeHighIntegritySecurityAttributes(SECURITY_ATTRIBUTES& sa, wil::unique_hlocal_security_descriptor& sd) noexcept
    try
    {
        unsigned long cb;
        THROW_IF_WIN32_BOOL_FALSE(ConvertStringSecurityDescriptorToSecurityDescriptorW(L"S:(ML;;NRNW;;;HI)", SDDL_REVISION_1, wil::out_param_ptr<PSECURITY_DESCRIPTOR*>(sd), &cb));
        sa.nLength = sizeof(SECURITY_ATTRIBUTES);
        sa.lpSecurityDescriptor = sd.get();
        sa.bInheritHandle = false;
        return true;
    }
    CATCH_LOG_RETURN_FALSE()

    static wil::unique_hfile _createBufferFileForWrite(const std::filesystem::path& path, const bool elevatedOnly)
    {
        SECURITY_ATTRIBUTES sa{};
        wil::unique_hlocal_security_descriptor sd;
        SECURITY_ATTRIBUTES* securityAttributes{ nullptr };
        if (elevatedOnly)
        {
            if (!_initializeHighIntegritySecurityAttributes(sa, sd))
            {
                return {};
            }
            securityAttributes = &sa;
        }

        return wil::unique_hfile{ CreateFileW(path.c_str(), GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_DELETE, securityAttributes, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr) };
    }

    static bool _copyBufferFileForRestore(const std::filesystem::path& source, const std::filesystem::path& destination, const bool elevatedOnly)
    {
        wil::unique_hfile input{ CreateFileW(source.c_str(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_DELETE, nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL | FILE_FLAG_SEQUENTIAL_SCAN, nullptr) };
        if (!input)
        {
            return false;
        }

        auto output = _createBufferFileForWrite(destination, elevatedOnly);
        if (!output)
        {
            return false;
        }

        std::array<char, 64 * 1024> buffer{};
        for (;;)
        {
            DWORD bytesRead{};
            if (!ReadFile(input.get(), buffer.data(), static_cast<DWORD>(buffer.size()), &bytesRead, nullptr))
            {
                return false;
            }
            if (bytesRead == 0)
            {
                return true;
            }

            DWORD bytesWrittenTotal{};
            while (bytesWrittenTotal < bytesRead)
            {
                DWORD bytesWritten{};
                if (!WriteFile(output.get(), buffer.data() + bytesWrittenTotal, bytesRead - bytesWrittenTotal, &bytesWritten, nullptr) || bytesWritten == 0)
                {
                    return false;
                }
                bytesWrittenTotal += bytesWritten;
            }
        }
    }

    uint32_t TerminalPage::TabCount() const
    {
        return [this]() -> IAsyncOperation<uint32_t> {
            co_await wil::resume_foreground(Dispatcher());
            co_return NumberOfTabs();
        }().get();
    }

    Windows::Foundation::IReference<uint32_t> TerminalPage::FocusedTabIndex() const
    {
        return [this]() -> IAsyncOperation<Windows::Foundation::IReference<uint32_t>> {
            co_await wil::resume_foreground(Dispatcher());
            const auto idx = _GetFocusedTabIndex();
            if (idx.has_value())
            {
                co_return Windows::Foundation::IReference<uint32_t>(idx.value());
            }
            co_return nullptr;
        }().get();
    }

    // ============================================================================
    // Queries — return typed WinRT structs
    // ============================================================================

    IAsyncOperation<Protocol::PaneInfo> TerminalPage::GetProtocolActivePane()
    {
        auto strong = get_strong();
        co_await wil::resume_foreground(Dispatcher());

        Protocol::PaneInfo result{};

        const auto focusedTabIdx = _GetFocusedTabIndex();
        if (!focusedTabIdx.has_value())
            co_return result;

        const auto tab = _tabs.GetAt(focusedTabIdx.value());
        const auto tabImpl = _GetTabImpl(tab);
        if (!tabImpl)
            co_return result;

        const auto activePane = tabImpl->GetActivePane();
        if (!activePane)
            co_return result;

        // If the active pane is an agent pane, return the source pane instead.
        // "Active" in the protocol means "the pane the user is working in".
        auto effectivePane = activePane;
        if (activePane->IsAgentPane())
        {
            const auto rootPane = tabImpl->GetRootPane();
            if (rootPane)
            {
                rootPane->WalkTree([&](const auto& pane) {
                    if (pane->IsSourceOfAgentPane())
                        effectivePane = pane;
                });
            }
        }

        result.SessionId = _getSessionIdFromPane(effectivePane);
        result.TabId = focusedTabIdx.value();
        result.IsActive = true;
        result.IsAgentPane = effectivePane->IsAgentPane();

        TerminalApp::TerminalPaneContent termContent{ nullptr };
        if (const auto t = effectivePane->GetContent().try_as<TerminalApp::TerminalPaneContent>())
        {
            termContent = t;
        }
        else if (const auto a = effectivePane->GetContent().try_as<TerminalApp::AgentPaneContent>())
        {
            termContent = a.GetTerminalContent();
        }
        if (termContent)
        {
            result.Title = termContent.Title();
            const auto profile = termContent.GetProfile();
            result.Profile = profile ? profile.Name() : L"";
        }

        if (const auto termControl = effectivePane->GetTerminalControl())
        {
            result.Cwd = termControl.WorkingDirectory();
            result.Shell = termControl.ShellName();
            result.ShellVersion = termControl.ShellVersion();
        }

        result.Pid = _getPidFromPane(effectivePane);
        co_return result;
    }

    IAsyncOperation<Windows::Foundation::Collections::IVector<Protocol::TabInfo>> TerminalPage::GetProtocolTabs()
    {
        auto strong = get_strong();
        co_await wil::resume_foreground(Dispatcher());

        auto tabs = winrt::single_threaded_vector<Protocol::TabInfo>();
        const auto focusedIdx = _GetFocusedTabIndex();

        for (uint32_t i = 0; i < _tabs.Size(); ++i)
        {
            const auto tab = _tabs.GetAt(i);
            const auto tabImpl = _GetTabImpl(tab);
            if (!tabImpl)
                continue;

            Protocol::TabInfo info{};
            info.TabId = i;
            info.Title = tab.Title();
            info.StableId = tabImpl->StableId();
            info.IsActive = focusedIdx.has_value() && (focusedIdx.value() == i);
            // Working dir of the first non-agent shell pane (tab distinguisher).
            if (const auto rootPane = tabImpl->GetRootPane())
            {
                rootPane->WalkTree([&](const auto& pane) {
                    if (!info.Cwd.empty() || pane->IsAgentPane())
                    {
                        return;
                    }
                    if (const auto term = pane->GetContent().try_as<TerminalApp::ITerminalPaneContent>())
                    {
                        if (const auto ctrl = term.GetTermControl())
                        {
                            if (const auto cwd = ctrl.WorkingDirectory(); !cwd.empty())
                            {
                                info.Cwd = cwd;
                            }
                        }
                    }
                });
            }
            // Count terminal panes only (those with a SessionId).
            uint32_t terminalPaneCount = 0;
            if (const auto rootPane = tabImpl->GetRootPane())
            {
                rootPane->WalkTree([&](const auto& pane) {
                    if (_getSessionIdFromPane(pane) != winrt::guid{})
                        terminalPaneCount++;
                });
            }
            info.PaneCount = terminalPaneCount;
            tabs.Append(info);
        }

        co_return tabs;
    }

    IAsyncOperation<Windows::Foundation::Collections::IVector<Protocol::PaneInfo>> TerminalPage::GetProtocolPanes(uint32_t tabIdFilter)
    {
        auto strong = get_strong();

        co_await wil::resume_foreground(Dispatcher());

        auto panes = winrt::single_threaded_vector<Protocol::PaneInfo>();

        for (uint32_t tabIdx = 0; tabIdx < _tabs.Size(); ++tabIdx)
        {
            if (tabIdFilter != UINT32_MAX && tabIdx != tabIdFilter)
                continue;

            const auto tab = _tabs.GetAt(tabIdx);
            const auto tabImpl = _GetTabImpl(tab);
            if (!tabImpl)
                continue;

            const auto rootPane = tabImpl->GetRootPane();
            if (!rootPane)
                continue;

            const auto activePane = tabImpl->GetActivePane();
            const auto activeIsAgent = activePane && activePane->IsAgentPane();

            rootPane->WalkTree([&](const auto& pane) {
                if (!pane->GetContent())
                    return; // Skip branch nodes

                const auto sid = _getSessionIdFromPane(pane);
                if (sid == winrt::guid{})
                    return; // Skip non-terminal panes

                Protocol::PaneInfo info{};
                info.SessionId = sid;
                info.TabId = tabIdx;
                info.IsAgentPane = pane->IsAgentPane();
                info.IsActive = activeIsAgent
                    ? pane->IsSourceOfAgentPane()
                    : (activePane == pane);
                info.Pid = _getPidFromPane(pane);

                TerminalApp::TerminalPaneContent termContent{ nullptr };
                if (const auto t = pane->GetContent().try_as<TerminalApp::TerminalPaneContent>())
                {
                    termContent = t;
                }
                else if (const auto a = pane->GetContent().try_as<TerminalApp::AgentPaneContent>())
                {
                    termContent = a.GetTerminalContent();
                }
                if (termContent)
                {
                    info.Title = termContent.Title();
                    const auto profile = termContent.GetProfile();
                    info.Profile = profile ? profile.Name() : L"";

                    if (const auto termControl = pane->GetTerminalControl())
                    {
                        info.Rows = termControl.ViewHeight();
                        info.Columns = 0;
                        info.Cwd = termControl.WorkingDirectory();
                        info.Shell = termControl.ShellName();
                        info.ShellVersion = termControl.ShellVersion();
                    }
                }

                panes.Append(info);
            });
        }

        co_return panes;
    }

    IAsyncOperation<Protocol::PaneOutput> TerminalPage::ReadProtocolPaneOutput(winrt::guid sessionId, hstring source, int32_t maxLines)
    {
        auto strong = get_strong();
        const auto sourceStr = winrt::to_string(source);
        const auto sourceRoute = ProtocolParsing::ClassifyPaneOutputSource(sourceStr);
        const auto effectiveMaxLines = (maxLines <= 0) ? 200 : maxLines;

        co_await wil::resume_foreground(Dispatcher());

        Protocol::PaneOutput result{};

        // UI-thread work: find pane, read buffer.
        hstring fullBuffer;
        int32_t viewHeight = 0;
        for (const auto& tab : _tabs)
        {
            const auto tabImpl = _GetTabImpl(tab);
            if (!tabImpl)
                continue;

            const auto rootPane = tabImpl->GetRootPane();
            if (!rootPane)
                continue;

            const auto foundPane = rootPane->FindPaneBySessionId(sessionId);
            if (!foundPane)
                continue;

            const auto termControl = foundPane->GetTerminalControl();
            if (!termControl)
                co_return result; // empty SessionId signals not-ready

            try
            {
                if (sourceRoute == ProtocolParsing::PaneOutputSource::LastPrompt)
                {
                    // Special path: return only the most recent completed
                    // shell prompt (command + output, bracketed by FTCS
                    // marks). Avoids leaking arbitrary trailing buffer
                    // content (older commands, secrets) to external agents.
                    result.SessionId = sessionId;
                    const auto lastPrompt = termControl.ReadLastPrompt();
                    auto lastPromptStr = winrt::to_string(lastPrompt);
                    if (lastPromptStr.empty())
                    {
                        // No OSC 133 marks (or no completed prompt yet) —
                        // signal so the caller can fall back to a line-count
                        // read. has_marks=false signals the caller to fall back.
                        result.HasMarks = false;
                        result.Content = L"";
                        result.LineCount = 0;
                        result.Truncated = false;
                        co_return result;
                    }
                    int32_t lineCount = 1;
                    for (auto ch : lastPromptStr)
                    {
                        if (ch == '\n')
                            ++lineCount;
                    }
                    result.HasMarks = true;
                    result.Content = winrt::to_hstring(lastPromptStr);
                    result.LineCount = lineCount;
                    result.Truncated = false;
                    co_return result;
                }

                fullBuffer = termControl.ReadEntireBuffer();
                viewHeight = termControl.ViewHeight();
            }
            catch (...)
            {
                co_return result; // empty SessionId signals error
            }

            result.SessionId = sessionId;
            break;
        }

        if (result.SessionId == winrt::guid{})
            co_return result; // not found

        // Move off UI thread for string processing.
        co_await winrt::resume_background();

        auto fullBufferStr = winrt::to_string(fullBuffer);
        std::vector<std::string> lines;
        std::istringstream iss(fullBufferStr);
        std::string line;
        while (std::getline(iss, line))
        {
            if (!line.empty() && line.back() == '\r')
                line.pop_back();
            lines.push_back(line);
        }

        if (sourceRoute == ProtocolParsing::PaneOutputSource::Screen)
        {
            const auto startIdx = lines.size() > static_cast<size_t>(viewHeight)
                                      ? lines.size() - viewHeight
                                      : 0;

            std::string content;
            int lineCount = 0;
            for (size_t i = startIdx; i < lines.size(); ++i)
            {
                if (!content.empty())
                    content += "\n";
                content += lines[i];
                lineCount++;
            }

            result.Content = winrt::to_hstring(content);
            result.LineCount = lineCount;
            result.Truncated = false;
        }
        else
        {
            const auto truncated = (static_cast<int32_t>(lines.size()) > effectiveMaxLines);
            const auto startIdx = truncated ? lines.size() - effectiveMaxLines : 0;

            std::string content;
            int lineCount = 0;
            for (size_t i = startIdx; i < lines.size(); ++i)
            {
                if (!content.empty())
                    content += "\n";
                content += lines[i];
                lineCount++;
            }

            result.Content = winrt::to_hstring(content);
            result.LineCount = lineCount;
            result.Truncated = truncated;
        }

        co_return result;
    }

    IAsyncOperation<Protocol::ProcessStatus> TerminalPage::GetProtocolProcessStatus(winrt::guid sessionId)
    {
        auto strong = get_strong();

        co_await wil::resume_foreground(Dispatcher());

        Protocol::ProcessStatus result{};

        for (const auto& tab : _tabs)
        {
            const auto tabImpl = _GetTabImpl(tab);
            if (!tabImpl)
                continue;

            const auto rootPane = tabImpl->GetRootPane();
            if (!rootPane)
                continue;

            const auto foundPane = rootPane->FindPaneBySessionId(sessionId);
            if (!foundPane)
                continue;

            result.SessionId = sessionId;

            const auto termControl = foundPane->GetTerminalControl();
            if (!termControl)
            {
                result.State = L"unknown";
                co_return result;
            }

            const auto conn = termControl.Connection();
            if (!conn)
            {
                result.State = L"exited";
                co_return result;
            }

            const auto connState = termControl.ConnectionState();

            if (connState == ConnectionState::Connected)
            {
                result.State = L"running";
                result.Pid = _getPidFromPane(foundPane);
            }
            else
            {
                result.State = L"exited";
                if (const auto conpty = conn.try_as<ConptyConnection>())
                {
                    const auto handle = conpty.RootProcessHandle();
                    if (handle)
                    {
                        DWORD exitCode = 0;
                        if (GetExitCodeProcess(reinterpret_cast<HANDLE>(handle), &exitCode))
                        {
                            if (exitCode != STILL_ACTIVE)
                            {
                                result.ExitCode = static_cast<int32_t>(exitCode);
                                result.HasExitCode = true;
                            }
                        }
                        result.Pid = static_cast<uint32_t>(GetProcessId(reinterpret_cast<HANDLE>(handle)));
                    }
                }
            }

            co_return result;
        }

        co_return result; // empty SessionId = not found
    }

    IAsyncOperation<Protocol::SessionVariable> TerminalPage::GetProtocolSessionVariable(winrt::guid sessionId, hstring name)
    {
        auto strong = get_strong();

        co_await wil::resume_foreground(Dispatcher());

        Protocol::SessionVariable result{};

        for (const auto& tab : _tabs)
        {
            const auto tabImpl = _GetTabImpl(tab);
            if (!tabImpl)
                continue;

            const auto rootPane = tabImpl->GetRootPane();
            if (!rootPane)
                continue;

            const auto foundPane = rootPane->FindPaneBySessionId(sessionId);
            if (!foundPane)
                continue;

            result.SessionId = sessionId;
            result.Name = name;

            const auto value = foundPane->GetSessionVariable(name);
            if (value.has_value())
            {
                result.Value = value.value();
                result.Exists = true;
            }
            else
            {
                result.Value = L"";
                result.Exists = false;
            }

            co_return result;
        }

        co_return result; // empty SessionId = not found
    }

    // ============================================================================
    // Mutations — return typed structs or bool
    // ============================================================================

    IAsyncOperation<bool> TerminalPage::SetProtocolSessionVariable(winrt::guid sessionId, hstring name, hstring value)
    {
        auto strong = get_strong();

        co_await wil::resume_foreground(Dispatcher());

        for (const auto& tab : _tabs)
        {
            const auto tabImpl = _GetTabImpl(tab);
            if (!tabImpl)
                continue;

            const auto rootPane = tabImpl->GetRootPane();
            if (!rootPane)
                continue;

            const auto foundPane = rootPane->FindPaneBySessionId(sessionId);
            if (!foundPane)
                continue;

            if (value.empty())
                foundPane->RemoveSessionVariable(name);
            else
                foundPane->SetSessionVariable(name, value);
            co_return true;
        }

        co_return false;
    }

    IAsyncOperation<Protocol::TabCreationResult> TerminalPage::CreateProtocolTab(NewTerminalArgs args, bool background)
    {
        auto strong = get_strong();
        co_await wil::resume_foreground(Dispatcher());

        Protocol::TabCreationResult result{};

        auto pane = _MakePane(args, nullptr);
        if (!pane)
            co_return result;

        _CreateNewTabFromPane(pane, -1, /*openInBackground=*/background);
        _tabContent.UpdateLayout(); // Force synchronous terminal initialization

        if (_tabs.Size() == 0)
            co_return result;

        const auto newTabIdx = _tabs.Size() - 1;
        const auto newTab = _tabs.GetAt(newTabIdx);
        const auto tabImpl = _GetTabImpl(newTab);

        result.TabId = newTabIdx;

        if (tabImpl)
        {
            const auto rootPane = tabImpl->GetRootPane();
            if (rootPane)
            {
                result.SessionId = _getSessionIdFromPane(rootPane);
                result.Pid = _getPidFromPane(rootPane);
            }
        }

        co_return result;
    }

    std::filesystem::path TerminalPage::_SavedWorkspaceSessionDir(const winrt::hstring& id)
    {
        std::filesystem::path dir{ std::wstring_view{ CascadiaSettings::SettingsDirectory() } };
        dir /= L"SavedWorkspaceSessions";
        dir /= std::wstring_view{ id };
        return dir;
    }

    IAsyncOperation<winrt::hstring> TerminalPage::SaveWorkspaceSessionProtocol(winrt::hstring tabIds, winrt::hstring title, winrt::hstring mode)
    {
        auto strong = get_strong();
        co_await wil::resume_foreground(Dispatcher());

        // Parse the comma-separated StableId list. The first entry is the
        // initiating /save-ws tab (used for overwrite-conflict binding).
        std::vector<winrt::hstring> stableIds;
        {
            const auto csv = winrt::to_string(tabIds);
            size_t start = 0;
            while (start <= csv.size())
            {
                const auto comma = csv.find(',', start);
                const auto len = (comma == std::string::npos) ? std::string::npos : comma - start;
                auto tok = csv.substr(start, len);
                if (!tok.empty())
                {
                    stableIds.push_back(winrt::to_hstring(tok));
                }
                if (comma == std::string::npos)
                {
                    break;
                }
                start = comma + 1;
            }
        }
        if (stableIds.empty())
        {
            co_return winrt::hstring{};
        }

        const auto initiatingTab = _FindTabByStableId(stableIds.front());
        if (!initiatingTab)
        {
            // The initiating tab isn't in this window/page — let the COM
            // server try the next window.
            co_return winrt::hstring{};
        }

        // Overwrite-conflict is keyed on the initiating tab's workspace binding
        // (set on restore or a prior save), not on the tab StableId — so it
        // survives resume into a fresh tab and generalizes to multi-tab.
        const auto boundId = initiatingTab->BoundWorkspaceId();
        Model::SavedWorkspaceSession existing{ nullptr };
        if (!boundId.empty())
        {
            if (const auto sessions = ApplicationState::SharedInstance().SavedWorkspaceSessions())
            {
                for (const auto& s : sessions)
                {
                    if (s.Id() == boundId)
                    {
                        existing = s;
                        break;
                    }
                }
            }
        }

        auto modeStr = winrt::to_string(mode);
        if (modeStr.empty())
        {
            modeStr = "auto";
        }

        if (modeStr == "auto" && existing)
        {
            Json::Value out{ Json::objectValue };
            out["outcome"] = "conflict";
            out["existingId"] = winrt::to_string(existing.Id());
            out["existingTitle"] = winrt::to_string(existing.Title());
            Json::StreamWriterBuilder wb;
            co_return winrt::to_hstring(Json::writeString(wb, out));
        }

        winrt::hstring id;
        if (modeStr == "overwrite" && !boundId.empty())
        {
            id = boundId;
        }
        if (id.empty())
        {
            id = winrt::hstring{ ::Microsoft::Console::Utils::GuidToString(::Microsoft::Console::Utils::CreateGuid()) };
        }

        const auto dir = _SavedWorkspaceSessionDir(id);
        std::error_code ec;
        std::filesystem::remove_all(dir, ec);
        std::filesystem::create_directories(dir, ec);

        // Build one SavedWorkspaceTab per selected tab. Buffer files (keyed by
        // globally-unique pane SessionId) and per-tab chat-history files
        // (agent-chat-{i}.json) all share the workspace dir.
        auto tabRecords = winrt::single_threaded_vector<Model::SavedWorkspaceTab>();
        uint32_t tabIdx = 0;
        for (const auto& stableId : stableIds)
        {
            const auto tabImpl = _FindTabByStableId(stableId);
            if (!tabImpl)
            {
                continue;
            }

            auto actions = tabImpl->BuildStartupActions(BuildStartupKind::Persist);

            std::vector<winrt::hstring> bufferIds;
            if (const auto rootPane = tabImpl->GetRootPane())
            {
                rootPane->WalkTree([&](const auto& pane) {
                    try
                    {
                        const auto term = pane->GetContent().try_as<TerminalApp::ITerminalPaneContent>();
                        if (!term)
                        {
                            return;
                        }
                        const auto control = term.GetTermControl();
                        if (!control)
                        {
                            return;
                        }
                        const auto connection = control.Connection();
                        if (!connection)
                        {
                            return;
                        }
                        const auto sessionId = connection.SessionId();
                        if (sessionId == winrt::guid{})
                        {
                            return;
                        }

                        const auto sidStr = winrt::hstring{ fmt::format(FMT_COMPILE(L"{}"), sessionId) };
                        const auto path = dir / (std::wstring{ L"buffer_" } + std::wstring{ sidStr } + L".txt");
                        if (auto file = _createBufferFileForWrite(path, IsRunningElevated()))
                        {
                            control.PersistTo(reinterpret_cast<int64_t>(file.get()));
                            bufferIds.push_back(sidStr);
                        }
                    }
                    CATCH_LOG();
                });
            }

            Model::SavedWorkspaceTab tabRecord;
            tabRecord.SourceStableId(stableId);
            tabRecord.TabActions(winrt::single_threaded_vector<Model::ActionAndArgs>(std::move(actions)));
            tabRecord.BufferSessionIds(winrt::single_threaded_vector<winrt::hstring>(std::move(bufferIds)));

            // Locate the helper-written chat-history file for this tab (braces
            // stripped, matching the Rust writer).
            auto histStem = std::wstring{ std::wstring_view{ stableId } };
            std::erase(histStem, L'{');
            std::erase(histStem, L'}');
            const auto histSrc = ::IntelligentTerminal::AgentPaneHistoryDir() / (histStem + L".json");
            std::error_code hec;
            const bool hasHistory = !histSrc.empty() && std::filesystem::exists(histSrc, hec);
            const bool hasVisibleAgentPane = tabImpl->FindAgentPane() && !tabImpl->HasStashedAgentPane();

            if (hasHistory || hasVisibleAgentPane)
            {
                const auto& globals = _settings.GlobalSettings();
                Model::SavedWorkspaceAgentPane agentPane;
                agentPane.Cli(globals.EffectiveAcpAgent());
                agentPane.Model(globals.AcpModel());
                agentPane.Position(globals.AgentPanePosition());

                if (hasHistory)
                {
                    const auto histName = std::wstring{ L"agent-chat-" } + std::to_wstring(tabIdx) + L".json";
                    std::error_code cec;
                    std::filesystem::copy_file(histSrc, dir / histName, std::filesystem::copy_options::overwrite_existing, cec);
                    if (!cec)
                    {
                        agentPane.ChatHistoryFile(winrt::hstring{ histName });
                    }

                    // Read the tab's ACP session id from its history JSON so
                    // restore can session/load it (agent memory).
                    try
                    {
                        std::ifstream f{ histSrc, std::ios::binary };
                        Json::Value root;
                        Json::CharReaderBuilder rb;
                        std::string errs;
                        if (f && Json::parseFromStream(rb, f, &root, &errs) && root.isObject() && root["session_id"].isString())
                        {
                            agentPane.AgentSessionId(winrt::to_hstring(root["session_id"].asString()));
                        }
                    }
                    CATCH_LOG();
                }

                tabRecord.AgentPane(agentPane);
            }

            tabRecords.Append(tabRecord);
            // Bind every saved tab to the workspace so a later /save-ws on any
            // of them detects the conflict and overwrites the right record.
            tabImpl->BoundWorkspaceId(id);
            tabIdx++;
        }

        if (tabRecords.Size() == 0)
        {
            co_return winrt::hstring{};
        }

        Model::SavedWorkspaceSession record;
        record.Id(id);
        record.Title(title);
        const auto nowMs = std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::system_clock::now().time_since_epoch()).count();
        record.SavedAt(winrt::to_hstring(nowMs));
        record.Tabs(tabRecords);
        ApplicationState::SharedInstance().UpsertSavedWorkspaceSession(record);

        Json::Value out{ Json::objectValue };
        out["outcome"] = "saved";
        out["id"] = winrt::to_string(id);
        out["title"] = winrt::to_string(title);
        Json::StreamWriterBuilder wb;
        co_return winrt::to_hstring(Json::writeString(wb, out));
    }

    IAsyncOperation<winrt::hstring> TerminalPage::ListSavedWorkspaceSessionsProtocol()
    {
        auto strong = get_strong();
        co_await wil::resume_foreground(Dispatcher());

        Json::Value arr{ Json::arrayValue };
        const auto sessions = ApplicationState::SharedInstance().SavedWorkspaceSessions();
        if (sessions)
        {
            for (const auto& s : sessions)
            {
                // A workspace is "open" when some live tab is bound to it.
                bool isOpen = false;
                for (const auto& t : _tabs)
                {
                    if (const auto ti = _GetTabImpl(t); ti && ti->BoundWorkspaceId() == s.Id())
                    {
                        isOpen = true;
                        break;
                    }
                }

                Json::Value o{ Json::objectValue };
                o["id"] = winrt::to_string(s.Id());
                o["title"] = winrt::to_string(s.Title());
                o["savedAt"] = winrt::to_string(s.SavedAt());
                o["isOpen"] = isOpen;
                arr.append(o);
            }
        }
        Json::StreamWriterBuilder wb;
        co_return winrt::to_hstring(Json::writeString(wb, arr));
    }

    IAsyncOperation<winrt::hstring> TerminalPage::RestoreWorkspaceSessionProtocol(winrt::hstring id)
    {
        auto strong = get_strong();
        co_await wil::resume_foreground(Dispatcher());

        Model::SavedWorkspaceSession record{ nullptr };
        const auto sessions = ApplicationState::SharedInstance().SavedWorkspaceSessions();
        if (sessions)
        {
            for (const auto& s : sessions)
            {
                if (s.Id() == id)
                {
                    record = s;
                    break;
                }
            }
        }
        if (!record)
        {
            co_return winrt::hstring{};
        }

        // Already open? Focus the live tab bound to this workspace.
        for (const auto& t : _tabs)
        {
            if (const auto ti = _GetTabImpl(t); ti && ti->BoundWorkspaceId() == id)
            {
                FocusTab(*ti);

                Json::Value out{ Json::objectValue };
                out["outcome"] = "focused";
                Json::StreamWriterBuilder wb;
                co_return winrt::to_hstring(Json::writeString(wb, out));
            }
        }

        const auto dir = _SavedWorkspaceSessionDir(id);
        const std::filesystem::path settingsRoot{ std::wstring_view{ CascadiaSettings::SettingsDirectory() } };

        // Stage every tab's buffers into the settings root and gather the
        // replay actions across all tabs (a workspace holds one tab today).
        std::vector<Model::ActionAndArgs> actions;
        if (const auto tabs = record.Tabs())
        {
            using namespace std::string_view_literals;
            const auto filenamePrefix = IsRunningElevated() ? L"elevated_"sv : L"buffer_"sv;

            for (const auto& tab : tabs)
            {
                if (const auto ids = tab.BufferSessionIds())
                {
                    for (const auto& sid : ids)
                    {
                        const std::wstring storedName = std::wstring{ L"buffer_" } + std::wstring{ sid } + L".txt";
                        const std::wstring stagedName = std::wstring{ filenamePrefix } + std::wstring{ sid } + L".txt";
                        _copyBufferFileForRestore(dir / storedName, settingsRoot / stagedName, IsRunningElevated());
                    }
                }
                if (const auto tabActions = tab.TabActions())
                {
                    for (const auto& a : tabActions)
                    {
                        actions.push_back(a);
                    }
                }
            }
        }

        const auto tabCountBefore = _tabs.Size();

        // Replay the workspace's startup actions inline (mirrors
        // ProcessStartupActions' GH#13136 suspend-between-panes discipline) so
        // this coroutine can await completion and then bind the created tabs.
        // ProcessStartupActions itself is fire-and-forget and, with tabs
        // already open, suspends before creating the tab — so we can't await it
        // or read back the new tab.
        auto suspend = _tabs.Size() > 0;
        for (auto&& a : actions)
        {
            if (suspend)
            {
                co_await wil::resume_foreground(Dispatcher(), winrt::Windows::UI::Core::CoreDispatcherPriority::Low);
            }
            _actionDispatch->DoAction(a);
            suspend = true;
        }

        // Bind every newly created tab to this workspace (so a later /save-ws
        // overwrites the right record) and restore its agent pane when the
        // record has one. Runs synchronously so any pending load-session is
        // registered before the deferred pre-warm tick fires.
        const auto tabs = record.Tabs();
        for (uint32_t i = tabCountBefore; i < _tabs.Size(); ++i)
        {
            const auto ti = _GetTabImpl(_tabs.GetAt(i));
            if (!ti)
            {
                continue;
            }
            ti->BoundWorkspaceId(id);

            const uint32_t recIdx = i - tabCountBefore;
            if (!tabs || recIdx >= tabs.Size())
            {
                continue;
            }
            const auto ap = tabs.GetAt(recIdx).AgentPane();
            if (!ap)
            {
                continue;
            }

            const auto stableId = ti->StableId();
            const auto sid = winrt::to_string(ap.AgentSessionId());

            // Resolve the saved chat-history file (workspace-relative) to an
            // absolute path so the helper can rehydrate the exact UI.
            std::string histPath;
            if (const auto histFile = ap.ChatHistoryFile(); !histFile.empty())
            {
                histPath = winrt::to_string((_SavedWorkspaceSessionDir(id) / std::wstring_view{ histFile }).wstring());
            }

            if ((!sid.empty() || !histPath.empty()) && !stableId.empty())
            {
                // The deferred pre-warm consumes this: --initial-load-session-id
                // rehydrates agent memory via session/load, --initial-chat-history
                // restores the exact saved chat UI (and suppresses the load's
                // plain-text replay). Either may be empty independently.
                _pendingLoadSessions[stableId] = _PendingLoadSession{ sid, std::string{}, histPath };
            }
            // /save-ws is only typed in a visible agent pane, so restore
            // reopens it visible. The echo of this request lands in
            // OnAgentStateChanged, which un-stashes / spawns the (loaded) pane.
            _RequestAgentStateForTab(ti, std::nullopt, /*paneOpen*/ true);
        }

        Json::Value out{ Json::objectValue };
        out["outcome"] = "opened";
        Json::StreamWriterBuilder wb;
        co_return winrt::to_hstring(Json::writeString(wb, out));
    }

    void TerminalPage::SetRestoreWorkspaceOnInit(winrt::hstring id)
    {
        _restoreWorkspaceIdOnInit = id;
    }

    // Self-restore an Eternal-Terminal workspace into THIS (freshly created)
    // window once its page is Initialized. Mirrors RestoreWorkspaceSessionProtocol
    // but, because the window was created empty (with a default startup tab),
    // it also closes those pre-existing default tab(s) afterwards so the window
    // shows exactly the restored workspace.
    safe_void_coroutine TerminalPage::_RestoreWorkspaceOnInit(winrt::hstring id)
    {
        auto strong = get_strong();
        co_await wil::resume_foreground(Dispatcher());

        Model::SavedWorkspaceSession record{ nullptr };
        if (const auto sessions = ApplicationState::SharedInstance().SavedWorkspaceSessions())
        {
            for (const auto& s : sessions)
            {
                if (s.Id() == id)
                {
                    record = s;
                    break;
                }
            }
        }
        if (!record)
        {
            co_return;
        }

        // Capture the fresh window's default tab(s) to close after restoring.
        std::vector<winrt::com_ptr<Tab>> preexisting;
        for (const auto& t : _tabs)
        {
            if (const auto ti = _GetTabImpl(t))
            {
                preexisting.push_back(ti);
            }
        }

        const auto dir = _SavedWorkspaceSessionDir(id);
        const std::filesystem::path settingsRoot{ std::wstring_view{ CascadiaSettings::SettingsDirectory() } };

        std::vector<Model::ActionAndArgs> actions;
        if (const auto tabs = record.Tabs())
        {
            using namespace std::string_view_literals;
            const auto filenamePrefix = IsRunningElevated() ? L"elevated_"sv : L"buffer_"sv;
            for (const auto& tab : tabs)
            {
                if (const auto ids = tab.BufferSessionIds())
                {
                    for (const auto& sid : ids)
                    {
                        const std::wstring storedName = std::wstring{ L"buffer_" } + std::wstring{ sid } + L".txt";
                        const std::wstring stagedName = std::wstring{ filenamePrefix } + std::wstring{ sid } + L".txt";
                        _copyBufferFileForRestore(dir / storedName, settingsRoot / stagedName, IsRunningElevated());
                    }
                }
                if (const auto tabActions = tab.TabActions())
                {
                    for (const auto& a : tabActions)
                    {
                        actions.push_back(a);
                    }
                }
            }
        }

        const auto tabCountBefore = _tabs.Size();
        auto suspend = _tabs.Size() > 0;
        for (auto&& a : actions)
        {
            if (suspend)
            {
                co_await wil::resume_foreground(Dispatcher(), winrt::Windows::UI::Core::CoreDispatcherPriority::Low);
            }
            _actionDispatch->DoAction(a);
            suspend = true;
        }

        const auto tabs = record.Tabs();
        for (uint32_t i = tabCountBefore; i < _tabs.Size(); ++i)
        {
            const auto ti = _GetTabImpl(_tabs.GetAt(i));
            if (!ti)
            {
                continue;
            }
            ti->BoundWorkspaceId(id);

            const uint32_t recIdx = i - tabCountBefore;
            if (!tabs || recIdx >= tabs.Size())
            {
                continue;
            }
            const auto ap = tabs.GetAt(recIdx).AgentPane();
            if (!ap)
            {
                continue;
            }

            const auto stableId = ti->StableId();
            const auto sid = winrt::to_string(ap.AgentSessionId());
            std::string histPath;
            if (const auto histFile = ap.ChatHistoryFile(); !histFile.empty())
            {
                histPath = winrt::to_string((_SavedWorkspaceSessionDir(id) / std::wstring_view{ histFile }).wstring());
            }
            if ((!sid.empty() || !histPath.empty()) && !stableId.empty())
            {
                _pendingLoadSessions[stableId] = _PendingLoadSession{ sid, std::string{}, histPath };
            }
            _RequestAgentStateForTab(ti, std::nullopt, /*paneOpen*/ true);
        }

        // Drop the fresh window's default startup tab(s).
        for (const auto& def : preexisting)
        {
            _RemoveTab(*def);
        }
    }

    IAsyncOperation<bool> TerminalPage::DeleteSavedWorkspaceSessionProtocol(winrt::hstring id)
    {
        auto strong = get_strong();
        co_await wil::resume_foreground(Dispatcher());

        const bool removed = ApplicationState::SharedInstance().RemoveSavedWorkspaceSession(id);
        std::error_code ec;
        std::filesystem::remove_all(_SavedWorkspaceSessionDir(id), ec);
        co_return removed;
    }

    IAsyncOperation<Protocol::TabCreationResult> TerminalPage::SplitProtocolPane(winrt::guid sessionId, SplitDirection direction, float size, NewTerminalArgs args, bool background)
    {
        auto strong = get_strong();

        co_await wil::resume_foreground(Dispatcher());

        Protocol::TabCreationResult result{};

        for (uint32_t tabIdx = 0; tabIdx < _tabs.Size(); ++tabIdx)
        {
            const auto tab = _tabs.GetAt(tabIdx);
            const auto tabImpl = _GetTabImpl(tab);
            if (!tabImpl)
                continue;

            const auto rootPane = tabImpl->GetRootPane();
            if (!rootPane)
                continue;

            const auto foundPane = rootPane->FindPaneBySessionId(sessionId);
            if (!foundPane)
                continue;

            if (const auto id = foundPane->Id())
            {
                tabImpl->FocusPane(id.value());
            }

            auto newPane = _MakePane(args, nullptr);
            if (!newPane)
                co_return result;

            const auto newPanePid = _getPidFromPane(newPane);
            auto newPaneRef = newPane; // copy shared_ptr before move

            _SplitPane(tabImpl, direction, size, std::move(newPane), /*focusNewPane=*/!background);
            _tabContent.UpdateLayout(); // Force synchronous terminal initialization

            result.TabId = tabIdx;
            result.SessionId = _getSessionIdFromPane(newPaneRef);
            result.Pid = newPanePid;
            co_return result;
        }

        co_return result;
    }

    IAsyncOperation<bool> TerminalPage::CloseProtocolPane(winrt::guid sessionId)
    {
        auto strong = get_strong();

        co_await wil::resume_foreground(Dispatcher());

        for (const auto& tab : _tabs)
        {
            const auto tabImpl = _GetTabImpl(tab);
            if (!tabImpl)
                continue;

            const auto rootPane = tabImpl->GetRootPane();
            if (!rootPane)
                continue;

            const auto foundPane = rootPane->FindPaneBySessionId(sessionId);
            if (!foundPane)
                continue;

            foundPane->Close();
            co_return true;
        }

        co_return false;
    }

    IAsyncOperation<bool> TerminalPage::SendProtocolInput(winrt::guid sessionId, hstring text)
    {
        auto strong = get_strong();
        // Replace \n with \r — shells expect carriage return (Enter key)
        // rather than line feed to execute commands.
        std::wstring input{ text };
        std::replace(input.begin(), input.end(), L'\n', L'\r');

        co_await wil::resume_foreground(Dispatcher());

        for (const auto& tab : _tabs)
        {
            const auto tabImpl = _GetTabImpl(tab);
            if (!tabImpl)
                continue;

            const auto rootPane = tabImpl->GetRootPane();
            if (!rootPane)
                continue;

            const auto foundPane = rootPane->FindPaneBySessionId(sessionId);
            if (!foundPane)
                continue;

            const auto termControl = foundPane->GetTerminalControl();
            if (!termControl)
                co_return false;

            termControl.SendInput(winrt::hstring{ input });
            co_return true;
        }

        co_return false;
    }

    // Switch focus to `sessionId`: if it lives in a non-active tab, switch tabs
    // first; then focus the pane within its tab and programmatically focus
    // its TermControl. Used by the recommendation executor so that hitting
    // "Run" follows focus to the destination pane.
    IAsyncOperation<bool> TerminalPage::FocusProtocolPane(winrt::guid sessionId)
    {
        auto strong = get_strong();

        co_await wil::resume_foreground(Dispatcher());

        for (const auto& tab : _tabs)
        {
            const auto tabImpl = _GetTabImpl(tab);
            if (!tabImpl)
                continue;

            const auto rootPane = tabImpl->GetRootPane();
            if (!rootPane)
                continue;

            const auto foundPane = rootPane->FindPaneBySessionId(sessionId);
            if (!foundPane)
                continue;

            const auto paneId = foundPane->Id();
            if (!paneId)
                co_return false;

            // Bring this window to the foreground. `focus_pane` can target a
            // pane that lives in a *different* window than the one driving the
            // request (e.g. Enter on a session in window B whose pane lives in
            // window A). The `_SetFocusedTab` / `FocusPane` calls below only
            // move XAML focus *within* this window — they don't activate the OS
            // window, and when the target pane is already the focused pane here
            // they no-op entirely. Without an explicit summon the window would
            // then stay in the background whenever it happened to already have
            // the target pane focused, while working only when focus actually
            // transitioned (an accidental side effect). Raising
            // `SummonWindowRequested` mirrors the desktop-notification
            // activation path (TabManagement.cpp) and makes `focus_pane`
            // reliably surface the window regardless of its prior focus state.
            SummonWindowRequested.raise(nullptr, nullptr);

            _SetFocusedTab(tab);

            // The pane may be a currently-stashed agent pane (Ctrl+Shift+. /
            // openAgentPane toggle). `FindPaneBySessionId` happily returns
            // hidden panes (HidePane only collapses the XAML layout, it
            // doesn't detach from the parent's _firstChild/_secondChild
            // tree), but `FocusPane` → `_Focus()` on a hidden TermControl
            // silently drops because the element isn't in the visual tree.
            // Detect that case and route through `RestoreStashedAgentPane`,
            // which re-adds the pane to the XAML tree and schedules a
            // low-priority Focus() so the freshly re-parented TermControl
            // actually receives focus.
            if (foundPane->IsHidden())
            {
                const auto splitDir = _AgentPanePositionToSplitDirection(_settings.GlobalSettings().AgentPanePosition());
                if (tabImpl->RestoreStashedAgentPane(splitDir))
                {
                    // Mirror the unstash to wta so wta's tab.pane_open
                    // state stays in sync. Without this, the
                    // `_SetFocusedTab(tab)` above triggers a `tab_changed`
                    // round-trip whose echo (`agent_state_changed` with
                    // the stale `pane_open=false`) lands in
                    // `OnAgentStateChanged` and immediately re-stashes
                    // the pane we just restored. Matches the unstash
                    // path in `_OpenOrReuseAgentPane`
                    // (TerminalPage.cpp:2510-2518).
                    //
                    // View is intentionally left as nullopt: focus_pane
                    // is a "go look at this session" gesture, not a
                    // chat/sessions view switch, so we let wta echo back
                    // whichever view the pane was last in.
                    _RequestAgentStateForTab(tabImpl, std::nullopt, /*pane_open*/ true);
                    co_return true;
                }
                // Restore precondition failed (e.g. agent pane is the root
                // pane, so there's no parent to fold into). Fall through to
                // the legacy focus path — it will no-op visually but won't
                // crash, and the caller will at least observe a `false`
                // return and can decide to escalate (e.g. open a new pane).
            }

            if (!tabImpl->FocusPane(paneId.value()))
                co_return false;

            if (const auto termControl = foundPane->GetTerminalControl())
            {
                termControl.Focus(winrt::Windows::UI::Xaml::FocusState::Programmatic);
            }
            co_return true;
        }

        co_return false;
    }

}
