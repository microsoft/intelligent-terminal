// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "TmuxController.h"
#include "TerminalPage.h"
#include "TerminalPaneContent.h"
#include "../TerminalSettingsAppAdapterLib/TerminalSettings.h"

#include <charconv>
#include <cmath>
#include <sstream>

using namespace winrt::Microsoft::Terminal;
using namespace winrt::Microsoft::Terminal::Control;
using namespace winrt::Microsoft::Terminal::Settings::Model;
using namespace winrt::Microsoft::Terminal::TerminalConnection;
using namespace winrt::Windows::UI::Xaml;
namespace
{
    namespace Protocol = ::Microsoft::Terminal::Tmux;
    std::string exceptionMessage()
    {
        try
        {
            throw;
        }
        catch (const winrt::hresult_error& error)
        {
            return winrt::to_string(error.message());
        }
        catch (const std::exception& error)
        {
            return error.what();
        }
        catch (...)
        {
            return "Unexpected tmux frontend failure";
        }
    }

    uint64_t number(const std::string_view text)
    {
        uint64_t value{};
        const auto [end, error] = std::from_chars(text.data(), text.data() + text.size(), value);
        if (text.empty() || error != std::errc{} || end != text.data() + text.size())
        {
            throw Protocol::ProtocolError{ "Invalid numeric field in tmux response" };
        }
        return value;
    }

    Protocol::Id identifier(const std::string_view text, const char prefix)
    {
        if (text.size() < 2 || text.front() != prefix)
        {
            throw Protocol::ProtocolError{ "Invalid identifier in tmux response" };
        }
        return number(text.substr(1));
    }

    std::vector<std::string_view> words(std::string_view text)
    {
        std::vector<std::string_view> result;
        while (!text.empty())
        {
            const auto first = text.find_first_not_of(" \t\r");
            if (first == std::string_view::npos)
            {
                break;
            }
            text.remove_prefix(first);
            const auto end = text.find_first_of(" \t\r");
            result.emplace_back(text.substr(0, end));
            if (end == std::string_view::npos)
            {
                break;
            }
            text.remove_prefix(end);
        }
        return result;
    }

    std::string screenText(const std::string_view captured)
    {
        const auto decoded = Protocol::DecodeOctal(captured);
        std::string result;
        result.reserve(decoded.size());
        for (const auto ch : decoded)
        {
            if (ch == '\n')
            {
                result.push_back('\r');
            }
            result.push_back(ch);
        }
        return result;
    }

    std::string diagnosticText(std::string text)
    {
        for (auto& ch : text)
        {
            if (static_cast<unsigned char>(ch) < 32 && ch != '\r' && ch != '\n' && ch != '\t')
            {
                ch = '?';
            }
        }
        return text;
    }
}

namespace winrt::TerminalApp::implementation
{
    namespace Protocol = ::Microsoft::Terminal::Tmux;
    TmuxController::TmuxController(TerminalPage& page) :
        _page{ page.get_weak() },
        _dispatcher{ Windows::System::DispatcherQueue::GetForCurrentThread() }
    {
    }

    TmuxController::~TmuxController()
    {
        Stop();
    }

    void TmuxController::Start(const hstring& commandline, const hstring& workingDirectory)
    {
        const auto page = _page.get();
        THROW_HR_IF(E_ABORT, !page);
        page->_tabView.CanDragTabs(false);
        page->_tabView.CanReorderTabs(false);
        const auto profile = page->_settings.GetProfileForArgs(NewTerminalArgs{});
        const auto settings = Settings::TerminalSettings::CreateWithProfile(page->_settings, profile);
        _diagnosticConnection = winrt::make_self<TmuxPaneConnection>(nullptr, nullptr);
        _diagnosticControl = page->_CreateNewControlAndContent(settings, *_diagnosticConnection);
        const auto content = winrt::make<TerminalPaneContent>(profile, page->_terminalSettingsCache, _diagnosticControl, true);
        const auto pane = std::make_shared<Pane>(content);
        _diagnosticTab = page->_GetTabImpl(page->_CreateNewTabFromPane(pane));
        _diagnosticTab->SuppressAgentPrewarm();
        _diagnosticTab->SetTabText(L"tmux");
        _sizeChanged = page->_tabContent.SizeChanged(winrt::auto_revoke, [weak = weak_from_this()](auto&&, auto&&) {
            if (const auto self = weak.lock())
            {
                self->ResizeWindow();
            }
        });

        Protocol::TmuxProcess::Callbacks callbacks;
        callbacks.output = [weak = weak_from_this()](const std::string_view bytes) {
            if (const auto self = weak.lock(); self && !self->_stopped)
            {
                self->_output(bytes);
            }
        };
        callbacks.error = [weak = weak_from_this()](const std::string_view bytes) {
            if (const auto self = weak.lock(); self && !self->_stopped)
            {
                self->_stderr(bytes);
            }
        };
        callbacks.exited = [weak = weak_from_this()](const uint32_t code) {
            if (const auto self = weak.lock(); self && !self->_stopped)
            {
                self->_exited(code);
            }
        };
        callbacks.failed = [weak = weak_from_this()](const std::exception_ptr error) {
            if (const auto self = weak.lock(); self && !self->_stopped)
            {
                try
                {
                    std::rethrow_exception(error);
                }
                catch (...)
                {
                    self->_fail(exceptionMessage());
                }
            }
        };
        _process = std::make_shared<Protocol::TmuxProcess>(std::move(callbacks));
        _writeCommand = [process = _process](std::string command) { process->Write(std::move(command)); };
        _startProcess(shared_from_this(), std::wstring{ commandline }, std::wstring{ workingDirectory });
        _startupTimeout(weak_from_this());
    }

    winrt::fire_and_forget TmuxController::_startupTimeout(std::weak_ptr<TmuxController> weak)
    {
        co_await winrt::resume_after(std::chrono::seconds{ 30 });
        if (const auto self = weak.lock(); self && !self->_stopped && !self->_inventoryReceived)
        {
            self->_fail("Timed out waiting for the tmux control protocol and initial window inventory");
        }
    }

    winrt::fire_and_forget TmuxController::_startProcess(std::shared_ptr<TmuxController> self, std::wstring commandline, std::wstring directory)
    {
        co_await winrt::resume_background();
        try
        {
            std::shared_ptr<Protocol::TmuxProcess> process;
            {
                std::lock_guard lock{ self->_protocolMutex };
                process = self->_process;
            }
            if (process && !self->_stopped)
            {
                process->Start(std::move(commandline), std::move(directory));
            }
        }
        catch (...)
        {
            self->_fail(exceptionMessage());
        }
    }

    winrt::fire_and_forget TmuxController::_closeProcess(std::shared_ptr<Protocol::TmuxProcess> process)
    {
        co_await winrt::resume_background();
        process->Close();
    }

    void TmuxController::Stop() noexcept
    {
        if (_stopped.exchange(true))
        {
            return;
        }
        std::shared_ptr<Protocol::TmuxProcess> process;
        {
            std::lock_guard lock{ _protocolMutex };
            process = std::exchange(_process, {});
            _writeCommand = {};
            _responses.clear();
        }
        if (process)
        {
            _closeProcess(std::move(process));
        }
        _sizeChanged.revoke();
        _tabs.clear();
        _panes.clear();
        _diagnosticTab = nullptr;
        _diagnosticControl = nullptr;
        _diagnosticConnection = nullptr;
    }

    void TmuxController::_post(std::function<void(TmuxController&)> work)
    {
        if (_stopped)
        {
            return;
        }
        if (_postedWork.fetch_add(1) >= 4096 && !_failed)
        {
            --_postedWork;
            throw Protocol::ProtocolError{ "Too many pending tmux window updates" };
        }
        if (!_dispatcher.TryEnqueue([weak = weak_from_this(), work = std::move(work)]() {
                if (const auto self = weak.lock(); self && !self->_stopped)
                {
                    auto completed = wil::scope_exit([&]() noexcept { --self->_postedWork; });
                    try
                    {
                        work(*self);
                    }
                    catch (...)
                    {
                        self->_fail(exceptionMessage());
                    }
                }
            }))
        {
            --_postedWork;
            LOG_HR_MSG(E_ABORT, "The tmux window dispatcher stopped accepting work");
        }
    }

    void TmuxController::_fail(std::string message)
    {
        if (_stopped || _failed.exchange(true))
        {
            return;
        }
        LOG_HR_MSG(E_FAIL, "Tmux frontend: %hs", message.c_str());
        {
            std::lock_guard lock{ _stderrMutex };
            if (!_stderrTail.empty())
            {
                message.append("\r\n").append(_stderrTail);
            }
        }
        _post([message = std::move(message)](auto& self) {
            self._showFailure(message);
            for (const auto& [id, view] : self._panes)
            {
                view.stream->connection->SetState(ConnectionState::Failed);
            }
        });
        std::shared_ptr<Protocol::TmuxProcess> process;
        {
            std::lock_guard lock{ _protocolMutex };
            process = std::exchange(_process, {});
            _writeCommand = {};
            _responses.clear();
        }
        if (process)
        {
            _closeProcess(std::move(process));
        }
    }

    void TmuxController::_showFailure(const std::string& message)
    {
        const auto page = _page.get();
        if (!page)
        {
            return;
        }
        _diagnosticIsError = true;
        if (!_diagnosticTab || !page->_GetTabIndex(*_diagnosticTab))
        {
            const auto profile = page->_settings.GetProfileForArgs(NewTerminalArgs{});
            const auto settings = Settings::TerminalSettings::CreateWithProfile(page->_settings, profile);
            _diagnosticConnection = winrt::make_self<TmuxPaneConnection>(nullptr, nullptr);
            _diagnosticControl = page->_CreateNewControlAndContent(settings, *_diagnosticConnection);
            const auto content = winrt::make<TerminalPaneContent>(profile, page->_terminalSettingsCache, _diagnosticControl, true);
            _diagnosticTab = page->_GetTabImpl(page->_CreateNewTabFromPane(std::make_shared<Pane>(content)));
            _diagnosticTab->SuppressAgentPrewarm();
            _diagnosticTab->SetTabText(L"tmux");
        }
        _diagnosticConnection->WriteOutput(diagnosticText(message) + "\r\n");
        _diagnosticConnection->SetState(ConnectionState::Failed);
        page->_selectedTabItem(_diagnosticTab->TabViewItem());
    }

    void TmuxController::_stderr(const std::string_view bytes)
    {
        std::lock_guard lock{ _stderrMutex };
        constexpr size_t maximum = 16 * 1024;
        if (bytes.size() >= maximum)
        {
            _stderrTail.assign(bytes.substr(bytes.size() - maximum));
        }
        else
        {
            const auto excess = _stderrTail.size() + bytes.size();
            if (excess > maximum)
            {
                _stderrTail.erase(0, excess - maximum);
            }
            _stderrTail.append(bytes);
        }
    }

    void TmuxController::_output(const std::string_view bytes)
    {
        if (_failed)
        {
            return;
        }
        try
        {
            for (const auto& event : _parser.Feed(bytes))
            {
                if (_stopped || _failed)
                {
                    break;
                }
                _handleEvent(event);
            }
        }
        catch (...)
        {
            _fail(exceptionMessage());
        }
    }

    void TmuxController::_exited(const uint32_t code)
    {
        if (_failed || _stopped)
        {
            return;
        }
        try
        {
            _parser.Finish();
        }
        catch (...)
        {
            _fail(exceptionMessage());
            return;
        }
        if (_exiting && code == 0)
        {
            _post([](auto& self) {
                if (const auto page = self._page.get())
                {
                    page->CloseWindow();
                }
            });
        }
        else
        {
            _fail(fmt::format("tmux control process exited (code {})", code));
        }
    }

    void TmuxController::_handleEvent(const Event& event)
    {
        switch (event.kind)
        {
        case Event::Kind::Response:
        {
            if (!_initialResponse.exchange(true))
            {
                if ((event.flags & 1) != 0)
                {
                    throw Protocol::ProtocolError{ "Expected an unsolicited tmux attach response" };
                }
                if (!event.success)
                {
                    _fail(event.text);
                    return;
                }
                if ((event.flags & 1) == 0)
                {
                    if (!event.success)
                    {
                        _fail(event.text);
                    }
                    return;
                }
                _post([](auto& self) {
                    self.ResizeWindow();
                    self.Refresh();
                });
                return;
            }
            ResponseHandler handler;
            {
                std::lock_guard lock{ _protocolMutex };
                if (_responses.empty())
                {
                    throw Protocol::ProtocolError{ "Unexpected tmux command response" };
                }
                handler = std::move(_responses.front());
                _responses.pop_front();
            }
            if (handler)
            {
                handler(event);
            }
            else if (!event.success)
            {
                LOG_HR_MSG(E_FAIL, "Tmux command failed: %hs", event.text.c_str());
                _post([message = event.text](auto& self) { self._showFailure(message); });
            }
            break;
        }
        case Event::Kind::Output:
        {
            std::shared_ptr<Stream> stream;
            {
                std::lock_guard lock{ _streamsMutex };
                if (const auto it = _streams.find(event.paneId); it != _streams.end())
                {
                    stream = it->second;
                }
            }
            if (stream)
            {
                std::lock_guard lock{ stream->mutex };
                if (stream->ready)
                {
                    stream->connection->WriteOutput(event.text);
                }
                else if (stream->captured)
                {
                    constexpr size_t maximum = 4 * 1024 * 1024;
                    if (event.text.size() > maximum - stream->backlog.size())
                    {
                        throw Protocol::ProtocolError{ "tmux pane snapshot backlog exceeded its limit" };
                    }
                    stream->backlog.append(event.text);
                }
            }
            break;
        }
        case Event::Kind::Notification:
            if (event.name == "layout-change" || event.name == "window-add" ||
                event.name == "window-close" || event.name == "session-changed" ||
                event.name == "session-window-changed")
            {
                _post([](auto& self) { self.Refresh(); });
            }
            else if (event.name == "window-renamed")
            {
                const auto split = event.text.find(' ');
                if (split == std::string::npos)
                {
                    throw Protocol::ProtocolError{ "Invalid tmux window-renamed notification" };
                }
                const auto id = identifier(std::string_view{ event.text }.substr(0, split), '@');
                _post([id, title = event.text.substr(split + 1)](auto& self) {
                    if (const auto it = self._tabs.find(id); it != self._tabs.end())
                    {
                        it->second->SetTabText(winrt::to_hstring(title));
                    }
                });
            }
            else if (event.name == "window-pane-changed")
            {
                const auto fields = words(event.text);
                if (fields.size() != 2)
                {
                    throw Protocol::ProtocolError{ "Invalid tmux window-pane-changed notification" };
                }
                const auto window = identifier(fields[0], '@');
                const auto pane = identifier(fields[1], '%');
                _post([window, pane](auto& self) {
                    const auto tab = self._tabs.find(window);
                    const auto view = self._panes.find(pane);
                    if (tab != self._tabs.end() && view != self._panes.end())
                    {
                        self._activePane = pane;
                        self._activeWindow = window;
                        if (const auto id = view->second.pane->Id())
                        {
                            tab->second->FocusPane(*id);
                        }
                    }
                });
            }
            else if (event.name == "pause")
            {
                const auto id = identifier(event.text, '%');
                _post([id](auto& self) {
                    if (const auto it = self._panes.find(id); it != self._panes.end())
                    {
                        self._hydrate(id, it->second.stream);
                        self._send(fmt::format("refresh-client -A '%{}:continue'", id));
                    }
                });
            }
            break;
        case Event::Kind::Exit:
            _exiting = true;
            break;
        }
    }

    void TmuxController::_send(std::string command, ResponseHandler response)
    {
        std::vector<std::pair<std::string, ResponseHandler>> commands;
        commands.emplace_back(std::move(command), std::move(response));
        _sendBatch(std::move(commands));
    }

    void TmuxController::_sendBatch(std::vector<std::pair<std::string, ResponseHandler>> commands, const bool compound)
    {
        if (_stopped || _failed || _exiting || !_initialResponse)
        {
            THROW_HR_MSG(E_ILLEGAL_METHOD_CALL, "The tmux control connection is not ready");
        }
        std::string text;
        for (const auto& [command, response] : commands)
        {
            auto part = std::string_view{ command };
            if (part.ends_with('\n'))
            {
                part.remove_suffix(1);
            }
            if (part.empty() || part.find_first_of("\r\n") != std::string_view::npos)
            {
                throw Protocol::ProtocolError{ "Invalid tmux command framing" };
            }
            if (!text.empty())
            {
                text.append(compound ? " ; " : "\n");
            }
            text.append(part);
        }
        text.push_back('\n');
        std::lock_guard lock{ _protocolMutex };
        THROW_HR_IF(E_ABORT, !_writeCommand);
        if (_responses.size() + commands.size() > 32768)
        {
            throw Protocol::ProtocolError{ "Too many outstanding tmux commands" };
        }
        for (auto& [command, response] : commands)
        {
            _responses.emplace_back(std::move(response));
        }
        try
        {
            _writeCommand(std::move(text));
        }
        catch (...)
        {
            for (size_t i = 0; i < commands.size(); ++i)
            {
                _responses.pop_back();
            }
            throw;
        }
    }

    void TmuxController::Refresh()
    {
        if (_stopped || _failed || _exiting || !_initialResponse)
        {
            return;
        }
        if (_refreshing)
        {
            _refreshAgain = true;
            return;
        }
        _requestRefresh();
    }

    void TmuxController::_requestRefresh()
    {
        _refreshing = true;
        _send("list-windows -F '#{window_id} #{window_active} #{window_layout} #{window_visible_layout}'", [weak = weak_from_this()](const Event& response) {
            if (const auto self = weak.lock())
            {
                if (!response.success)
                {
                    self->_fail(response.text);
                    return;
                }
                auto windows = self->_parseWindows(response.text);
                self->_inventoryReceived = true;
                self->_post([windows = std::move(windows)](auto& owner) mutable {
                    owner._applyWindows(std::move(windows));
                    owner._refreshing = false;
                    if (std::exchange(owner._refreshAgain, false))
                    {
                        owner.Refresh();
                    }
                });
            }
        });
    }

    std::map<TmuxController::Id, TmuxController::Window> TmuxController::_parseWindows(std::string_view text) const
    {
        std::map<Id, Window> result;
        while (!text.empty())
        {
            const auto end = text.find('\n');
            const auto fields = words(text.substr(0, end));
            if (fields.size() != 4)
            {
                throw Protocol::ProtocolError{ "Invalid tmux window inventory" };
            }
            const auto id = identifier(fields[0], '@');
            Window window;
            window.active = number(fields[1]) != 0;
            window.layoutText = fields[2];
            window.visibleLayoutText = fields[3];
            window.layout = Protocol::ParseLayout(fields[2]);
            window.visibleLayout = Protocol::ParseLayout(fields[3]);
            if (!result.emplace(id, std::move(window)).second || result.size() > 256)
            {
                throw Protocol::ProtocolError{ "Duplicate window or excessive tmux window count" };
            }
            if (end == std::string_view::npos)
            {
                break;
            }
            text.remove_prefix(end + 1);
        }
        return result;
    }

    void TmuxController::_collectLeaves(const Layout& layout, std::unordered_map<Id, std::pair<uint32_t, uint32_t>>& leaves) const
    {
        if (layout.kind == Layout::Kind::Leaf)
        {
            if (!leaves.emplace(layout.paneId, std::pair{ layout.width, layout.height }).second || leaves.size() > 1024)
            {
                throw Protocol::ProtocolError{ "Duplicate pane or excessive tmux pane count" };
            }
        }
        else
        {
            for (const auto& child : layout.children)
            {
                _collectLeaves(child, leaves);
            }
        }
    }

    TmuxController::PaneView TmuxController::_createPane(const Id id, const uint32_t columns, const uint32_t rows)
    {
        const auto page = _page.get();
        THROW_HR_IF(E_ABORT, !page);
        PaneView view;
        view.stream = std::make_shared<Stream>();
        view.stream->connection = winrt::make_self<TmuxPaneConnection>(
            [weak = weak_from_this(), id](const std::string_view input) {
                if (const auto self = weak.lock())
                {
                    self->_input(id, input);
                }
            },
            nullptr);
        const auto profile = page->_settings.GetProfileForArgs(NewTerminalArgs{});
        const auto settings = Settings::TerminalSettings::CreateWithProfile(page->_settings, profile);
        settings.DefaultSettings()->InitialCols(gsl::narrow<int32_t>(columns));
        settings.DefaultSettings()->InitialRows(gsl::narrow<int32_t>(rows));
        settings.DefaultSettings()->Padding(L"0");
        settings.DefaultSettings()->ScrollState(ScrollbarState::Hidden);
        view.control = page->_CreateNewControlAndContent(settings, *view.stream->connection);
        const auto content = winrt::make<TerminalPaneContent>(profile, page->_terminalSettingsCache, view.control, true);
        view.pane = std::make_shared<Pane>(content);
        view.control.GotFocus([weak = weak_from_this(), id](auto&&, auto&&) {
            if (const auto self = weak.lock())
            {
                self->_focus(id);
            }
        });
        view.control.Initialized([weak = weak_from_this(), id](auto&&, auto&&) {
            if (const auto self = weak.lock())
            {
                if (const auto it = self->_panes.find(id); it != self->_panes.end())
                {
                    self->_updateDimensions(it->second, it->second.columns, it->second.rows);
                }
            }
        });
        _updateDimensions(view, columns, rows);
        {
            std::lock_guard lock{ _streamsMutex };
            _streams.emplace(id, view.stream);
        }
        return view;
    }

    void TmuxController::_updateDimensions(PaneView& view, const uint32_t columns, const uint32_t rows)
    {
        view.columns = columns;
        view.rows = rows;
        const auto cell = view.control.CharacterDimensions();
        if (cell.Width > 0 && cell.Height > 0)
        {
            view.control.HorizontalAlignment(HorizontalAlignment::Left);
            view.control.VerticalAlignment(VerticalAlignment::Top);
            view.control.Width(static_cast<double>(columns) * cell.Width);
            view.control.Height(static_cast<double>(rows) * cell.Height);
        }
    }

    std::shared_ptr<Pane> TmuxController::_buildLayout(const Layout& layout)
    {
        if (layout.kind == Layout::Kind::Leaf)
        {
            const auto it = _panes.find(layout.paneId);
            if (it == _panes.end())
            {
                throw Protocol::ProtocolError{ "tmux layout references an undiscovered pane" };
            }
            _updateDimensions(it->second, layout.width, layout.height);
            return it->second.pane;
        }
        const auto columns = layout.kind == Layout::Kind::Columns;
        auto root = _buildLayout(layout.children.back());
        auto extent = columns ? layout.children.back().width : layout.children.back().height;
        for (size_t i = layout.children.size() - 1; i > 0; --i)
        {
            const auto& child = layout.children[i - 1];
            const auto childExtent = columns ? child.width : child.height;
            extent += childExtent + 1;
            const auto split = (static_cast<float>(childExtent) + 0.5f) / static_cast<float>(extent);
            root = std::make_shared<Pane>(_buildLayout(child), root, columns ? SplitState::Vertical : SplitState::Horizontal, split);
        }
        root->SetLayoutReadOnly(true);
        return root;
    }

    void TmuxController::_applyWindows(std::map<Id, Window> windows)
    {
        const auto page = _page.get();
        if (!page || _stopped || _failed)
        {
            return;
        }
        if (windows.empty())
        {
            page->CloseWindow();
            return;
        }

        std::unordered_map<Id, std::pair<uint32_t, uint32_t>> leaves;
        std::unordered_map<Id, std::pair<uint32_t, uint32_t>> visibleLeaves;
        for (const auto& [id, window] : windows)
        {
            _collectLeaves(window.layout, leaves);
            _collectLeaves(window.visibleLayout, visibleLeaves);
        }
        for (const auto& [id, dimensions] : visibleLeaves)
        {
            if (!leaves.contains(id))
            {
                throw Protocol::ProtocolError{ "Visible tmux layout contains an unknown pane" };
            }
        }

        bool changed = windows.size() != _windows.size();
        for (const auto& [id, window] : windows)
        {
            const auto old = _windows.find(id);
            changed |= old == _windows.end() || old->second.layoutText != window.layoutText ||
                       old->second.visibleLayoutText != window.visibleLayoutText;
        }
        for (const auto& [id, dimensions] : leaves)
        {
            if (!_panes.contains(id))
            {
                _panes.emplace(id, _createPane(id, dimensions.first, dimensions.second));
            }
        }

        if (changed)
        {
            _projecting = true;
            const auto wasRemoving = std::exchange(page->_removing, true);
            auto finish = wil::scope_exit([&]() noexcept {
                page->_removing = wasRemoving;
                _projecting = false;
            });
            page->_tabContent.Children().Clear();
            std::map<Id, Tab::LayoutSnapshot> previous;
            std::map<Id, std::shared_ptr<Pane>> replacements;
            std::vector<Id> createdTabs;
            for (const auto& [id, tab] : _tabs)
            {
                previous.emplace(id, tab->TakeLayout());
            }
            try
            {
                for (const auto& [id, window] : windows)
                {
                    replacements.emplace(id, _buildLayout(window.visibleLayout));
                }
                for (const auto& [id, root] : replacements)
                {
                    if (const auto it = _tabs.find(id); it != _tabs.end())
                    {
                        it->second->ApplyLayout(root, previous.at(id));
                    }
                    else
                    {
                        auto tab = winrt::make_self<Tab>(root);
                        tab->SuppressAgentPrewarm();
                        tab->SetTabText(winrt::to_hstring(fmt::format("tmux @{}", id)));
                        _tabs.emplace(id, tab);
                        createdTabs.push_back(id);
                        page->_InitializeTab(tab, -1, true);
                    }
                }
            }
            catch (...)
            {
                const auto error = std::current_exception();
                for (const auto& [id, tab] : _tabs)
                {
                    if (tab->GetRootPane())
                    {
                        tab->TakeLayout();
                    }
                }
                for (const auto& [id, root] : replacements)
                {
                    root->DetachLayout();
                }
                for (const auto& [id, snapshot] : previous)
                {
                    snapshot.root->RestoreLayout();
                    _tabs.at(id)->ApplyLayout(snapshot.root, snapshot);
                }
                for (const auto id : createdTabs)
                {
                    const auto tab = _tabs.at(id);
                    page->_RemoveTab(*tab, true);
                    _tabs.erase(id);
                }
                std::rethrow_exception(error);
            }
            for (auto it = _tabs.begin(); it != _tabs.end();)
            {
                if (!windows.contains(it->first))
                {
                    page->_RemoveTab(*it->second, true);
                    it = _tabs.erase(it);
                }
                else
                {
                    ++it;
                }
            }
        }
        _windows = std::move(windows);
        for (auto it = _panes.begin(); it != _panes.end();)
        {
            if (!leaves.contains(it->first))
            {
                {
                    std::lock_guard lock{ _streamsMutex };
                    _streams.erase(it->first);
                }
                it->second.pane->Shutdown();
                it = _panes.erase(it);
            }
            else
            {
                ++it;
            }
        }

        if (!_diagnosticIsError && _diagnosticTab && page->_GetTabIndex(*_diagnosticTab))
        {
            page->_RemoveTab(*_diagnosticTab, true);
            _diagnosticTab = nullptr;
            _diagnosticControl = nullptr;
            _diagnosticConnection = nullptr;
        }
        auto active = _tabs.begin()->first;
        for (const auto& [id, window] : _windows)
        {
            if (window.active)
            {
                active = id;
                break;
            }
        }
        _activeWindow = active;
        page->_selectedTabItem(_tabs.at(active)->TabViewItem());
        page->_UpdatedSelectedTab(*_tabs.at(active));
        page->_tabContent.UpdateLayout();

        for (const auto& [id, view] : _panes)
        {
            bool hydrate;
            {
                std::lock_guard lock{ view.stream->mutex };
                hydrate = !view.stream->ready && !view.stream->hydrating;
            }
            if (hydrate)
            {
                _hydrate(id, view.stream);
            }
        }
        if (changed)
        {
            for (const auto& [id, tab] : _tabs)
            {
                _send(fmt::format("display-message -p -t @{} '#{{window_name}}'", id), [weak = weak_from_this(), id](const Event& response) {
                    if (const auto self = weak.lock(); self && response.success)
                    {
                        self->_post([id, name = response.text](auto& owner) {
                            if (const auto tab = owner._tabs.find(id); tab != owner._tabs.end())
                            {
                                tab->second->SetTabText(winrt::to_hstring(name));
                            }
                        });
                    }
                });
            }
        }
    }

    void TmuxController::_hydrate(const Id id, const std::shared_ptr<Stream>& stream)
    {
        uint64_t generation;
        {
            std::lock_guard lock{ stream->mutex };
            if (stream->hydrating)
            {
                return;
            }
            if (++stream->attempts > 3)
            {
                throw Protocol::ProtocolError{ "tmux pane state could not be captured consistently" };
            }
            generation = ++stream->generation;
            stream->hydrating = true;
            stream->ready = false;
            stream->captured = false;
            stream->backlog.clear();
            stream->state.clear();
            stream->saved.clear();
            stream->current.clear();
        }
        auto capture = [weak = weak_from_this(), stream, generation](const Event& event, const int part) {
            const auto self = weak.lock();
            if (!self || self->_stopped || self->_failed)
            {
                return;
            }
            {
                std::lock_guard lock{ stream->mutex };
                if (generation != stream->generation)
                {
                    return;
                }
                if (!event.success)
                {
                    ++stream->generation;
                    stream->hydrating = false;
                    LOG_HR_MSG(E_FAIL, "Tmux pane capture failed: %hs", event.text.c_str());
                    self->_post([](auto& owner) { owner.Refresh(); });
                    return;
                }
                if (part == 0)
                {
                    stream->state = event.text;
                }
                else if (part == 1)
                {
                    stream->saved = event.text;
                }
                else if (part == 2)
                {
                    stream->current = event.text;
                    stream->captured = true;
                }
                else
                {
                    self->_finishHydration(stream, event.text);
                }
            }
            if (part == 3)
            {
                stream->connection->SetState(ConnectionState::Connected);
            }
        };
        std::vector<std::pair<std::string, ResponseHandler>> commands;
        commands.emplace_back(fmt::format(
                                  "display-message -p -t %{} '#{{cursor_x}} #{{cursor_y}} #{{alternate_on}} #{{alternate_saved_x}} #{{alternate_saved_y}} #{{cursor_flag}} #{{insert_flag}} #{{keypad_cursor_flag}} #{{keypad_flag}} #{{mouse_standard_flag}} #{{mouse_button_flag}} #{{mouse_any_flag}} #{{mouse_utf8_flag}} #{{mouse_sgr_flag}} #{{scroll_region_upper}} #{{scroll_region_lower}} #{{wrap_flag}}'",
                                  id),
                              [capture](const Event& event) { capture(event, 0); });
        commands.emplace_back(fmt::format("capture-pane -aepqCJN -S -2000 -t %{}", id), [capture](const Event& event) { capture(event, 1); });
        commands.emplace_back(fmt::format("capture-pane -epCJN -S -2000 -t %{}", id), [capture](const Event& event) { capture(event, 2); });
        commands.emplace_back(fmt::format("capture-pane -pPC -t %{}", id), [capture](const Event& event) { capture(event, 3); });
        _sendBatch(std::move(commands), true);
    }

    void TmuxController::_finishHydration(const std::shared_ptr<Stream>& stream, const std::string_view pending)
    {
        const auto fields = words(stream->state);
        if (fields.size() != 17)
        {
            throw Protocol::ProtocolError{ "Backend does not expose the required tmux pane state fields" };
        }
        std::array<uint32_t, 17> state{};
        for (size_t i = 0; i < state.size(); ++i)
        {
            const auto value = number(fields[i]);
            if (value > 32767)
            {
                throw Protocol::ProtocolError{ "tmux pane state exceeds terminal limits" };
            }
            state[i] = static_cast<uint32_t>(value);
        }
        std::string output = "\x1b"
                             "c\x1b[3J";
        if (state[2])
        {
            output.append(screenText(stream->saved));
            output.append(fmt::format("\x1b[{};{}H\x1b[?1049h", state[4] + 1, state[3] + 1));
        }
        output.append(screenText(stream->current));
        output.append(fmt::format("\x1b[{};{}r", state[14] + 1, state[15] + 1));
        const auto mode = [&](const uint32_t value, const uint32_t code) {
            output.append(fmt::format("\x1b[?{}{}", code, value ? 'h' : 'l'));
        };
        mode(state[5], 25);
        output.append(state[6] ? "\x1b[4h" : "\x1b[4l");
        mode(state[7], 1);
        output.append(state[8] ? "\x1b=" : "\x1b>");
        mode(state[9], 1000);
        mode(state[10], 1002);
        mode(state[11], 1003);
        mode(state[12], 1005);
        mode(state[13], 1006);
        mode(state[16], 7);
        output.append(fmt::format("\x1b[{};{}H", state[1] + 1, state[0] + 1));
        output.append(Protocol::DecodeOctal(pending));
        output.append(stream->backlog);
        stream->connection->WriteOutput(output);
        stream->backlog.clear();
        stream->saved.clear();
        stream->current.clear();
        stream->state.clear();
        stream->captured = false;
        stream->hydrating = false;
        stream->ready = true;
        stream->attempts = 0;
    }

    std::optional<TmuxController::Id> TmuxController::_paneId(const std::shared_ptr<Pane>& pane) const
    {
        if (pane)
        {
            for (const auto& [id, view] : _panes)
            {
                if (pane->ContentId() && pane->ContentId() == view.pane->ContentId())
                {
                    return id;
                }
            }
        }
        return std::nullopt;
    }

    void TmuxController::_input(const Id id, const std::string_view bytes)
    {
        try
        {
            std::vector<std::pair<std::string, ResponseHandler>> commands;
            for (auto& command : Protocol::EncodeSendKeys(id, bytes))
            {
                commands.emplace_back(std::move(command), ResponseHandler{});
            }
            if (!commands.empty())
            {
                _sendBatch(std::move(commands));
            }
        }
        catch (...)
        {
            _fail(exceptionMessage());
        }
    }

    void TmuxController::_focus(const Id id)
    {
        if (_projecting || _stopped || _failed || _exiting || !_initialResponse || _activePane == id)
        {
            return;
        }
        try
        {
            const auto pane = _panes.find(id);
            if (pane == _panes.end())
            {
                return;
            }
            for (const auto& [window, tab] : _tabs)
            {
                const auto root = tab->GetRootPane();
                if (root && root->FindPaneByContentId(*pane->second.pane->ContentId()))
                {
                    SelectTab(tab);
                    break;
                }
            }
            _activePane = id;
            _send(fmt::format("select-pane -t %{}", id));
        }
        catch (...)
        {
            _fail(exceptionMessage());
        }
    }

    void TmuxController::SelectTab(const winrt::com_ptr<Tab>& selected)
    {
        if (_projecting || _stopped || _failed || _exiting || !_initialResponse)
        {
            return;
        }
        try
        {
            for (const auto& [id, tab] : _tabs)
            {
                if (tab == selected && _activeWindow != id)
                {
                    _activeWindow = id;
                    _send(fmt::format("select-window -t @{}", id));
                    break;
                }
            }
        }
        catch (...)
        {
            _fail(exceptionMessage());
        }
    }

    bool TmuxController::NewWindow()
    {
        try
        {
            _send("new-window");
            return true;
        }
        catch (...)
        {
            _showFailure(exceptionMessage());
            return false;
        }
    }

    bool TmuxController::Split(const std::shared_ptr<Pane>& pane, SplitDirection direction, const float size)
    {
        const auto id = _paneId(pane);
        if (!id)
        {
            RejectUnsupportedOperation();
            return false;
        }
        if (direction == SplitDirection::Automatic)
        {
            const auto& view = _panes.at(*id);
            direction = view.control.ActualWidth() >= view.control.ActualHeight() ? SplitDirection::Right : SplitDirection::Down;
        }
        if (!std::isfinite(size) || size <= 0 || size >= 1)
        {
            _showFailure("Invalid tmux split size");
            return false;
        }
        const auto horizontal = direction == SplitDirection::Left || direction == SplitDirection::Right;
        const auto before = direction == SplitDirection::Left || direction == SplitDirection::Up;
        try
        {
            _send(fmt::format("split-window -t %{} -{} {} -l {}%", *id, horizontal ? 'h' : 'v', before ? "-b" : "", std::clamp(static_cast<int>(std::lround(size * 100)), 1, 99)));
            return true;
        }
        catch (...)
        {
            _showFailure(exceptionMessage());
            return false;
        }
    }

    bool TmuxController::ClosePane(const std::shared_ptr<Pane>& pane)
    {
        if (!pane)
        {
            return false;
        }
        std::vector<std::pair<std::string, ResponseHandler>> commands;
        pane->WalkTree([&](const auto& leaf) {
            if (const auto id = _paneId(leaf))
            {
                commands.emplace_back(fmt::format("kill-pane -t %{}", *id), ResponseHandler{});
            }
        });
        if (commands.empty())
        {
            if (_diagnosticTab && _diagnosticTab->GetRootPane() == pane)
            {
                _diagnosticTab->Close();
                return true;
            }
            return false;
        }
        if (_failed || _exiting)
        {
            pane->Close();
            return true;
        }
        try
        {
            _sendBatch(std::move(commands));
            return true;
        }
        catch (...)
        {
            _showFailure(exceptionMessage());
            return false;
        }
    }

    bool TmuxController::CloseTab(const winrt::com_ptr<Tab>& tab)
    {
        if (tab == _diagnosticTab)
        {
            tab->Close();
            return true;
        }
        for (const auto& [id, candidate] : _tabs)
        {
            if (tab == candidate)
            {
                if (_failed || _exiting)
                {
                    tab->Close();
                    _tabs.erase(id);
                    return true;
                }
                try
                {
                    _send(fmt::format("kill-window -t @{}", id));
                    return true;
                }
                catch (...)
                {
                    _showFailure(exceptionMessage());
                    return false;
                }
            }
        }
        return false;
    }

    bool TmuxController::ResizePane(const std::shared_ptr<Pane>& pane, const ResizeDirection direction)
    {
        const auto id = _paneId(pane);
        if (!id)
        {
            return false;
        }
        char flag;
        switch (direction)
        {
        case ResizeDirection::Left:
            flag = 'L';
            break;
        case ResizeDirection::Right:
            flag = 'R';
            break;
        case ResizeDirection::Up:
            flag = 'U';
            break;
        case ResizeDirection::Down:
            flag = 'D';
            break;
        default:
            return false;
        }
        try
        {
            _send(fmt::format("resize-pane -t %{} -{} 1", *id, flag));
            return true;
        }
        catch (...)
        {
            _showFailure(exceptionMessage());
            return false;
        }
    }

    bool TmuxController::ZoomPane(const std::shared_ptr<Pane>& pane)
    {
        const auto id = _paneId(pane);
        if (!id)
        {
            return false;
        }
        try
        {
            _send(fmt::format("resize-pane -Z -t %{}", *id));
            return true;
        }
        catch (...)
        {
            _showFailure(exceptionMessage());
            return false;
        }
    }

    void TmuxController::ResizeWindow()
    {
        if (_projecting || _stopped || _failed || _exiting || !_initialResponse)
        {
            return;
        }
        const auto page = _page.get();
        if (!page)
        {
            return;
        }
        const auto control = _panes.empty() ? _diagnosticControl : _panes.begin()->second.control;
        if (!control)
        {
            return;
        }
        const auto cell = control.CharacterDimensions();
        if (cell.Width <= 0 || cell.Height <= 0 || page->_tabContent.ActualWidth() <= 4 || page->_tabContent.ActualHeight() <= 4)
        {
            return;
        }
        for (auto& [id, view] : _panes)
        {
            _updateDimensions(view, view.columns, view.rows);
        }
        const auto columns = static_cast<uint32_t>(std::clamp(std::floor((page->_tabContent.ActualWidth() - 4) / cell.Width), 1.0, 32767.0));
        const auto rows = static_cast<uint32_t>(std::clamp(std::floor((page->_tabContent.ActualHeight() - 4) / cell.Height), 1.0, 32767.0));
        if (columns == _clientColumns && rows == _clientRows)
        {
            return;
        }
        try
        {
            _clientColumns = columns;
            _clientRows = rows;
            _send(fmt::format("refresh-client -C {}x{}", columns, rows));
        }
        catch (...)
        {
            _fail(exceptionMessage());
        }
    }

    void TmuxController::RejectUnsupportedOperation()
    {
        _showFailure("This operation is not supported in a tmux-managed window.");
    }
}
