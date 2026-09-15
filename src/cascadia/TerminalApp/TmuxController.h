// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "Pane.h"
#include "Tab.h"
#include "TmuxPaneConnection.h"
#include "TmuxProtocol.h"
#include "TmuxProcess.h"

#include <atomic>
#include <deque>
#include <functional>
#include <map>
#include <mutex>
#include <optional>
#include <unordered_map>
#include <unordered_set>

namespace winrt::TerminalApp::implementation
{
    struct TerminalPage;

    struct TmuxController : std::enable_shared_from_this<TmuxController>
    {
        explicit TmuxController(TerminalPage& page);
        ~TmuxController();

        void Start(const winrt::hstring& commandline, const winrt::hstring& workingDirectory);
        void Stop() noexcept;
        bool NewWindow();
        bool Split(const std::shared_ptr<Pane>& pane, Microsoft::Terminal::Settings::Model::SplitDirection direction, float size);
        bool ClosePane(const std::shared_ptr<Pane>& pane);
        bool CloseTab(const winrt::com_ptr<Tab>& tab);
        bool ResizePane(const std::shared_ptr<Pane>& pane, Microsoft::Terminal::Settings::Model::ResizeDirection direction);
        bool ZoomPane(const std::shared_ptr<Pane>& pane);
        void ResizeWindow();
        void SelectTab(const winrt::com_ptr<Tab>& tab);
        void Refresh();
        void RejectUnsupportedOperation();
        bool ApplyingLayout() const noexcept { return _projecting; }

    private:
        using Id = ::Microsoft::Terminal::Tmux::Id;
        using Event = ::Microsoft::Terminal::Tmux::Event;
        using Layout = ::Microsoft::Terminal::Tmux::LayoutNode;
        using ResponseHandler = std::function<void(const Event&)>;

        struct Stream
        {
            winrt::com_ptr<TmuxPaneConnection> connection;
            std::mutex mutex;
            bool ready = false;
            bool captured = false;
            bool hydrating = false;
            uint32_t attempts = 0;
            uint64_t generation = 0;
            std::string current;
            std::string saved;
            std::string state;
            std::string backlog;
        };

        struct PaneView
        {
            std::shared_ptr<Pane> pane;
            winrt::Microsoft::Terminal::Control::TermControl control{ nullptr };
            std::shared_ptr<Stream> stream;
            uint32_t columns = 0;
            uint32_t rows = 0;
        };

        struct Window
        {
            Layout layout;
            Layout visibleLayout;
            std::string layoutText;
            std::string visibleLayoutText;
            bool active = false;
        };

        static winrt::fire_and_forget _startProcess(std::shared_ptr<TmuxController> self, std::wstring commandline, std::wstring directory);
        static winrt::fire_and_forget _closeProcess(std::shared_ptr<::Microsoft::Terminal::Tmux::TmuxProcess> process);
        static winrt::fire_and_forget _startupTimeout(std::weak_ptr<TmuxController> weak);
        void _post(std::function<void(TmuxController&)> work);
        void _fail(std::string message);
        void _showFailure(const std::string& message);
        void _output(std::string_view bytes);
        void _stderr(std::string_view bytes);
        void _exited(uint32_t code);
        void _handleEvent(const Event& event);
        void _send(std::string command, ResponseHandler response = {});
        void _sendBatch(std::vector<std::pair<std::string, ResponseHandler>> commands, bool compound = false);
        void _requestRefresh();
        void _applyWindows(std::map<Id, Window> windows);
        std::map<Id, Window> _parseWindows(std::string_view text) const;
        PaneView _createPane(Id id, uint32_t columns, uint32_t rows);
        std::shared_ptr<Pane> _buildLayout(const Layout& layout);
        void _collectLeaves(const Layout& layout, std::unordered_map<Id, std::pair<uint32_t, uint32_t>>& leaves) const;
        void _hydrate(Id id, const std::shared_ptr<Stream>& stream);
        void _finishHydration(const std::shared_ptr<Stream>& stream, std::string_view pending);
        void _input(Id id, std::string_view bytes);
        void _focus(Id id);
        std::optional<Id> _paneId(const std::shared_ptr<Pane>& pane) const;
        void _updateDimensions(PaneView& view, uint32_t columns, uint32_t rows);

        winrt::weak_ref<TerminalPage> _page;
        winrt::Windows::System::DispatcherQueue _dispatcher{ nullptr };
        std::atomic<bool> _stopped = false;
        std::atomic<bool> _failed = false;
        std::atomic<bool> _exiting = false;
        std::atomic<size_t> _postedWork = 0;
        std::shared_ptr<::Microsoft::Terminal::Tmux::TmuxProcess> _process;
        std::function<void(std::string)> _writeCommand;
        ::Microsoft::Terminal::Tmux::Parser _parser;
        std::mutex _protocolMutex;
        std::deque<ResponseHandler> _responses;
        std::atomic<bool> _initialResponse = false;
        std::atomic<bool> _inventoryReceived = false;
        std::mutex _streamsMutex;
        std::unordered_map<Id, std::shared_ptr<Stream>> _streams;
        std::mutex _stderrMutex;
        std::string _stderrTail;
        std::map<Id, Window> _windows;
        std::unordered_map<Id, PaneView> _panes;
        std::map<Id, winrt::com_ptr<Tab>> _tabs;
        winrt::com_ptr<Tab> _diagnosticTab;
        winrt::com_ptr<TmuxPaneConnection> _diagnosticConnection;
        winrt::Microsoft::Terminal::Control::TermControl _diagnosticControl{ nullptr };
        bool _refreshing = false;
        bool _refreshAgain = false;
        bool _projecting = false;
        bool _diagnosticIsError = false;
        uint32_t _clientColumns = 0;
        uint32_t _clientRows = 0;
        std::optional<Id> _activePane;
        std::optional<Id> _activeWindow;
        winrt::Windows::UI::Xaml::FrameworkElement::SizeChanged_revoker _sizeChanged;
        friend class ::TerminalAppLocalTests::TabTests;
    };
}
