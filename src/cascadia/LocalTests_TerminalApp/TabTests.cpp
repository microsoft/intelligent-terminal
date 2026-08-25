// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include "../TerminalApp/TerminalPage.h"
#include "../TerminalApp/TerminalWindow.h"
#include "../TerminalApp/MinMaxCloseControl.h"
#include "../TerminalApp/TabRowControl.h"
#include "../TerminalApp/ShortcutActionDispatch.h"
#include "../TerminalApp/AgentPaneContent.h"
#include "../TerminalApp/AgentPaneDragStash.h"
#include "../TerminalApp/Tab.h"
#include "../TerminalApp/CommandPalette.h"
#include "../TerminalApp/ContentManager.h"
#include "../TerminalApp/AgentRestoreHelpers.h"
#include "../UnitTests_Control/MockControlSettings.h"
#include "CppWinrtTailored.h"

using namespace Microsoft::Console;
using namespace TerminalApp;
using namespace winrt::TerminalApp;
using namespace winrt::Microsoft::Terminal::Settings::Model;

using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;

using namespace winrt::Windows::ApplicationModel::DataTransfer;
using namespace winrt::Windows::Foundation::Collections;
using namespace winrt::Windows::System;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;
using namespace winrt::Windows::UI::Core;
using namespace winrt::Windows::UI::Text;

namespace winrt
{
    namespace MUX = Microsoft::UI::Xaml;
    namespace WUX = Windows::UI::Xaml;
    using IInspectable = Windows::Foundation::IInspectable;
}

namespace TerminalAppLocalTests
{
    class TestConnection : public winrt::implements<TestConnection, winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection>
    {
    public:
        TestConnection(const winrt::guid& sessionId,
                       const winrt::Microsoft::Terminal::TerminalConnection::ConnectionState initialState) noexcept :
            _sessionId{ sessionId },
            _state{ initialState }
        {
        }

        void Initialize(const winrt::Windows::Foundation::Collections::ValueSet& /*settings*/) {}
        void Start() noexcept {}
        void WriteInput(const winrt::array_view<const char16_t> data)
        {
            TerminalOutput.raise(data);
        }
        void Resize(uint32_t /*rows*/, uint32_t /*columns*/) noexcept {}
        void Close() noexcept
        {
            TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);
        }

        void SetState(const winrt::Microsoft::Terminal::TerminalConnection::ConnectionState state) noexcept
        {
            _state = state;
        }

        void RaiseStateChanged() noexcept
        {
            StateChanged.raise(*this, nullptr);
        }

        void TransitionTo(const winrt::Microsoft::Terminal::TerminalConnection::ConnectionState state) noexcept
        {
            SetState(state);
            RaiseStateChanged();
        }

        winrt::guid SessionId() const noexcept { return _sessionId; }
        winrt::Microsoft::Terminal::TerminalConnection::ConnectionState State() const noexcept { return _state; }

        til::event<winrt::Microsoft::Terminal::TerminalConnection::TerminalOutputHandler> TerminalOutput;
        til::typed_event<winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection, IInspectable> StateChanged;

    private:
        winrt::guid _sessionId{};
        std::atomic<winrt::Microsoft::Terminal::TerminalConnection::ConnectionState> _state{
            winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::NotConnected
        };
    };

    static std::string _formatPaneId(const winrt::guid& sessionId)
    {
        wchar_t buf[40]{};
        ::StringFromGUID2(sessionId, buf, ARRAYSIZE(buf));
        std::wstring ws{ buf };
        if (ws.size() > 2 && ws.front() == L'{' && ws.back() == L'}')
        {
            ws = ws.substr(1, ws.size() - 2);
        }
        return winrt::to_string(winrt::hstring{ ws });
    }

    struct ConnectionStateEventRecord
    {
        std::string paneId;
        std::string state;
    };

    static void _recordConnectionStateEvent(const winrt::hstring& eventJson,
                                            std::vector<ConnectionStateEventRecord>& connectionStates)
    {
        Json::Value evt;
        Json::CharReaderBuilder builder;
        std::string errors;
        std::istringstream stream{ winrt::to_string(eventJson) };
        if (Json::parseFromStream(builder, stream, &evt, &errors) &&
            evt["method"].asString() == "connection_state")
        {
            connectionStates.push_back({
                evt["params"]["pane_id"].asString(),
                evt["params"]["state"].asString() });
        }
    }

    static std::vector<std::string> _statesForPane(const std::vector<ConnectionStateEventRecord>& connectionStates,
                                                   const std::string& paneId)
    {
        std::vector<std::string> states;
        for (const auto& connectionState : connectionStates)
        {
            if (connectionState.paneId == paneId)
            {
                states.push_back(connectionState.state);
            }
        }
        return states;
    }

    static NewTerminalArgs _getTerminalArgs(const ActionAndArgs& action)
    {
        if (const auto newTabArgs = action.Args().try_as<NewTabArgs>())
        {
            return newTabArgs.ContentArgs().try_as<NewTerminalArgs>();
        }

        if (const auto splitPaneArgs = action.Args().try_as<SplitPaneArgs>())
        {
            return splitPaneArgs.ContentArgs().try_as<NewTerminalArgs>();
        }

        return nullptr;
    }

    // TODO:microsoft/terminal#3838:
    // Unfortunately, these tests _WILL NOT_ work in our CI. We're waiting for
    // an updated TAEF that will let us install framework packages when the test
    // package is deployed. Until then, these tests won't deploy in CI.

    class TabTests
    {
        // For this set of tests, we need to activate some XAML content. For
        // release builds, the application runs as a centennial application,
        // which lets us run full trust, and means that we need to use XAML
        // Islands to host our UI. However, in these tests, we don't really need
        // to run full trust - we just need to get some UI elements created. So
        // we can just rely on the normal UWP activation to create us.
        //
        // IMPORTANTLY! When tests need to make XAML objects, or do XAML things,
        // make sure to use RunOnUIThread. This helper will dispatch a lambda to
        // be run on the UI thread.

        BEGIN_TEST_CLASS(TabTests)
            TEST_CLASS_PROPERTY(L"RunAs", L"UAP")
            TEST_CLASS_PROPERTY(L"UAP:AppXManifest", L"TestHostAppXManifest.xml")
        END_TEST_CLASS()

        // These four tests act as canary tests. If one of them fails, then they
        // can help you identify if something much lower in the stack has
        // failed.
        TEST_METHOD(EnsureTestsActivate);
        TEST_METHOD(TryCreateConnectionType);
        TEST_METHOD(TryCreateXamlObjects);

        TEST_METHOD(TryInitializePage);

        TEST_METHOD(CreateSimpleTerminalXamlType);
        TEST_METHOD(CreateTerminalMuxXamlType);

        TEST_METHOD(CreateTerminalPage);
        TEST_METHOD(AgentSessionRestoreRequiresPersistedBufferPath);
        TEST_METHOD(PersistedLayoutAgentSessionsReceiveRestorePaths);
        TEST_METHOD(PaneAgentSessionBindingRequiresPaneIdentity);
        TEST_METHOD(AgentPaneRestoreDoesNotRequireAgentSession);
        TEST_METHOD(PaneAgentSessionEndClearsAgentBinding);
        TEST_METHOD(ContentIdHandoffEndClearsAgentBinding);
        TEST_METHOD(GetWindowLayoutIncludesAgentRestoreMetadata);
        TEST_METHOD(PersistStateIncludesAgentRestoreMetadata);
        TEST_METHOD(NaturalClosedEventThenNotifyPanesClosingEmitsOnce);
        TEST_METHOD(NaturalFailedEventThenNotifyPanesClosingEmitsOnce);
        TEST_METHOD(ReusedSessionIdAcrossControlLifetimesEmitsEndStatePerLifetime);
        TEST_METHOD(SyntheticFailedEventSuppressesDelayedNormalCallback);
        TEST_METHOD(CloseNonLastPaneEmitsOneEndStateWithoutKeepingTheTab);

        TEST_METHOD(TryDuplicateBadTab);
        TEST_METHOD(TryDuplicateBadPane);

        TEST_METHOD(TryZoomPane);
        TEST_METHOD(MoveFocusFromZoomedPane);
        TEST_METHOD(CloseZoomedPane);

        TEST_METHOD(SwapPanes);
        TEST_METHOD(BuildStartupActionsContentStashesAgentFirstPaneAsNewTab);
        TEST_METHOD(BuildStartupActionsContentStashesAgentLaterSplitAsExistingTab);
        TEST_METHOD(TransferredAgentContentFirstPaneDefersTabRekey);
        TEST_METHOD(TransferredAgentContentSplitPaneRetiresDestinationAgentPane);
        TEST_METHOD(TransferredAgentStatusReplaysMissedTabRekey);

        TEST_METHOD(NextMRUTab);
        TEST_METHOD(VerifyCommandPaletteTabSwitcherOrder);

        TEST_METHOD(TestWindowRenameSuccessful);
        TEST_METHOD(TestWindowRenameFailure);

        TEST_METHOD(TestPreviewCommitScheme);
        TEST_METHOD(TestPreviewDismissScheme);
        TEST_METHOD(TestPreviewSchemeWhilePreviewing);

        TEST_METHOD(TestClampSwitchToTab);

        TEST_CLASS_SETUP(ClassSetup)
        {
            return true;
        }

        TEST_METHOD_CLEANUP(MethodCleanup)
        {
            return true;
        }

    private:
        void _verifyBuildStartupActionsContentStashesAgentPane(SplitDirection splitDirection,
                                                               winrt::TerminalApp::implementation::AgentPaneDragStash::AttachDisposition expectedDisposition,
                                                               const winrt::guid& sourceProfileGuid);
        void _initializeTerminalPage(winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage>& page,
                                     CascadiaSettings initialSettings);
        void _createContentManager();
        winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage> _commonSetup();
        winrt::com_ptr<winrt::TerminalApp::implementation::WindowProperties> _windowProperties;
        winrt::com_ptr<winrt::TerminalApp::implementation::ContentManager> _contentManager;
    };

    template<typename TFunction>
    void TestOnUIThread(const TFunction& function)
    {
        const auto result = RunOnUIThread(function);
        VERIFY_SUCCEEDED(result);
    }

    void TabTests::_createContentManager()
    {
        TestOnUIThread([&]() {
            _contentManager = winrt::make_self<winrt::TerminalApp::implementation::ContentManager>();
        });
        VERIFY_IS_NOT_NULL(_contentManager);
    }

    void TabTests::EnsureTestsActivate()
    {
        // This test was originally used to ensure that XAML Islands was
        // initialized correctly. Now, it's used to ensure that the tests
        // actually deployed and activated. This test _should_ always pass.
        VERIFY_IS_TRUE(true);
    }

    void TabTests::TryCreateConnectionType()
    {
        // Verify we can create a WinRT type we authored
        // Just creating it is enough to know that everything is working.
        winrt::Microsoft::Terminal::TerminalConnection::EchoConnection conn{};
        VERIFY_IS_NOT_NULL(conn);
    }

    void TabTests::TryCreateXamlObjects()
    {
        auto result = RunOnUIThread([]() {
            VERIFY_IS_TRUE(true, L"Congrats! We're running on the UI thread!");

            auto v = winrt::Windows::ApplicationModel::Core::CoreApplication::GetCurrentView();
            VERIFY_IS_NOT_NULL(v, L"Ensure we have a current view");
            // Verify we can create a some XAML objects
            // Just creating all of them is enough to know that everything is working.
            winrt::Windows::UI::Xaml::Controls::UserControl controlRoot;
            VERIFY_IS_NOT_NULL(controlRoot, L"Try making a UserControl");
            winrt::Windows::UI::Xaml::Controls::Grid root;
            VERIFY_IS_NOT_NULL(root, L"Try making a Grid");
            winrt::Windows::UI::Xaml::Controls::SwapChainPanel swapChainPanel;
            VERIFY_IS_NOT_NULL(swapChainPanel, L"Try making a SwapChainPanel");
            winrt::Windows::UI::Xaml::Controls::Primitives::ScrollBar scrollBar;
            VERIFY_IS_NOT_NULL(scrollBar, L"Try making a ScrollBar");
        });

        VERIFY_SUCCEEDED(result);
    }

    void TabTests::CreateSimpleTerminalXamlType()
    {
        winrt::com_ptr<winrt::TerminalApp::implementation::MinMaxCloseControl> mmcc{ nullptr };

        auto result = RunOnUIThread([&mmcc]() {
            mmcc = winrt::make_self<winrt::TerminalApp::implementation::MinMaxCloseControl>();
            VERIFY_IS_NOT_NULL(mmcc);
        });
        VERIFY_SUCCEEDED(result);
    }

    void TabTests::CreateTerminalMuxXamlType()
    {
        winrt::com_ptr<winrt::TerminalApp::implementation::TabRowControl> tabRowControl{ nullptr };

        auto result = RunOnUIThread([&tabRowControl]() {
            tabRowControl = winrt::make_self<winrt::TerminalApp::implementation::TabRowControl>();
            VERIFY_IS_NOT_NULL(tabRowControl);
        });
        VERIFY_SUCCEEDED(result);
    }

    void TabTests::CreateTerminalPage()
    {
        winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage> page{ nullptr };

        _windowProperties = winrt::make_self<winrt::TerminalApp::implementation::WindowProperties>();
        winrt::TerminalApp::WindowProperties props = *_windowProperties;

        _createContentManager();
        winrt::TerminalApp::ContentManager contentManager = *_contentManager;

        auto result = RunOnUIThread([&page, props, contentManager]() {
            page = winrt::make_self<winrt::TerminalApp::implementation::TerminalPage>(props, contentManager);
            VERIFY_IS_NOT_NULL(page);
        });
        VERIFY_SUCCEEDED(result);
    }

    void TabTests::AgentSessionRestoreRequiresPersistedBufferPath()
    {
        using winrt::TerminalApp::implementation::ShouldResumeAgentSession;

        VERIFY_IS_FALSE(ShouldResumeAgentSession(false, false));
        VERIFY_IS_FALSE(ShouldResumeAgentSession(false, true));
        VERIFY_IS_FALSE(ShouldResumeAgentSession(true, false));
        VERIFY_IS_TRUE(ShouldResumeAgentSession(true, true));
    }

    void TabTests::PersistedLayoutAgentSessionsReceiveRestorePaths()
    {
        using winrt::TerminalApp::implementation::RemoveAgentPaneSessionFromShellBindings;
        using winrt::TerminalApp::implementation::SetPersistedLayoutAgentRestorePaths;

        const auto firstSessionId = ::Microsoft::Console::Utils::CreateGuid();
        const auto secondSessionId = ::Microsoft::Console::Utils::CreateGuid();

        NewTerminalArgs firstArgs{};
        firstArgs.SessionId(firstSessionId);
        firstArgs.AgentSessionId(L"codex-session");
        firstArgs.AgentPaneSessionId(L"copilot-pane-session");

        NewTerminalArgs secondArgs{};
        secondArgs.SessionId(secondSessionId);
        secondArgs.AgentSessionId(L"copilot-pane-session");
        secondArgs.AgentSessionAgent(L"copilot");
        secondArgs.AgentResumeCommandline(L"copilot --resume copilot-pane-session");

        std::vector<ActionAndArgs> actions;
        actions.emplace_back(ShortcutAction::NewTab, NewTabArgs{ firstArgs });
        actions.emplace_back(ShortcutAction::SplitPane, SplitPaneArgs{ SplitType::Manual, SplitDirection::Automatic, 0.5f, secondArgs });

        RemoveAgentPaneSessionFromShellBindings(actions, firstArgs.AgentPaneSessionId());
        SetPersistedLayoutAgentRestorePaths(actions, [](const winrt::guid& sessionId) {
            return winrt::hstring{ L"buffer_" + ::Microsoft::Console::Utils::GuidToPlainString(sessionId) + L".txt" };
        });

        VERIFY_ARE_EQUAL(
            winrt::hstring{ L"buffer_" + ::Microsoft::Console::Utils::GuidToPlainString(firstSessionId) + L".txt" },
            firstArgs.PersistedBufferPath());
        VERIFY_IS_TRUE(secondArgs.AgentSessionId().empty());
        VERIFY_IS_TRUE(secondArgs.AgentSessionAgent().empty());
        VERIFY_IS_TRUE(secondArgs.AgentResumeCommandline().empty());
        VERIFY_IS_TRUE(secondArgs.PersistedBufferPath().empty());
    }

    void TabTests::PaneAgentSessionBindingRequiresPaneIdentity()
    {
        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);
            const auto control = tab->GetRootPane()->GetTerminalControl();
            VERIFY_IS_NOT_NULL(control);
            const auto paneSessionId = control.Connection().SessionId();

            const auto event = [&](const std::string_view name, const std::string& paneId) {
                Json::Value evt;
                evt["params"]["pane_id"] = paneId;
                evt["params"]["event"] = std::string{ name };
                evt["params"]["agent_session_id"] = "agent-session-resumed";
                evt["params"]["agent"] = "copilot";
                Json::StreamWriterBuilder writer;
                writer["indentation"] = "";
                page->OnPaneAgentSessionChanged(winrt::to_hstring(Json::writeString(writer, evt)));
            };

            // A hook bridge that never inherited WT_SESSION publishes an empty
            // `pane_id` rather than borrowing the focused pane, so it must not
            // bind its ACP session to any pane at all.
            event("agent.session.start", "");
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_paneAgentSessions.count(paneSessionId)));

            const auto paneId = winrt::to_string(::Microsoft::Console::Utils::GuidToString(paneSessionId));
            event("agent.session.start", paneId);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_paneAgentSessions.count(paneSessionId)));

            // The same rule protects an existing binding from being cleared by
            // an unattributed end event.
            event("agent.session.end", "");
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_paneAgentSessions.count(paneSessionId)));
        });
    }

    void TabTests::AgentPaneRestoreDoesNotRequireAgentSession()
    {
        using winrt::TerminalApp::implementation::ShouldRestoreAgentPane;

        // An agent pane the user opened but never chatted in has no ACP
        // session id, yet its open/view state must still be restored.
        VERIFY_IS_TRUE(ShouldRestoreAgentPane(false, true, false));
        VERIFY_IS_TRUE(ShouldRestoreAgentPane(false, false, true));
        VERIFY_IS_TRUE(ShouldRestoreAgentPane(true, false, false));
        VERIFY_IS_FALSE(ShouldRestoreAgentPane(false, false, false));
    }

    void TabTests::PaneAgentSessionEndClearsAgentBinding()
    {
        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);
            const auto control = tab->GetRootPane()->GetTerminalControl();
            VERIFY_IS_NOT_NULL(control);
            const auto paneSessionId = control.Connection().SessionId();
            const auto paneId = winrt::to_string(::Microsoft::Console::Utils::GuidToString(paneSessionId));

            const auto event = [&](const std::string_view name) {
                Json::Value evt;
                evt["params"]["pane_id"] = paneId;
                evt["params"]["event"] = std::string{ name };
                evt["params"]["agent_session_id"] = "agent-session-resumed";
                evt["params"]["agent"] = "copilot";
                Json::StreamWriterBuilder writer;
                writer["indentation"] = "";
                page->OnPaneAgentSessionChanged(winrt::to_hstring(Json::writeString(writer, evt)));
            };

            event("agent.session.start");
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_paneAgentSessions.count(paneSessionId)));

            // A late end naming a different agent session must not clear the
            // binding a newer session just installed.
            {
                Json::Value stale;
                stale["params"]["pane_id"] = paneId;
                stale["params"]["event"] = "agent.session.end";
                stale["params"]["agent_session_id"] = "agent-session-previous";
                Json::StreamWriterBuilder writer;
                writer["indentation"] = "";
                page->OnPaneAgentSessionChanged(winrt::to_hstring(Json::writeString(writer, stale)));
            }
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_paneAgentSessions.count(paneSessionId)));

            // The agent that ran in this pane exited, so there is nothing left
            // to resume and the pane restores as a plain shell.
            event("agent.session.end");
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_paneAgentSessions.count(paneSessionId)));
        });
    }

    void TabTests::ContentIdHandoffEndClearsAgentBinding()
    {
        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        const auto liveSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{62a75f00-aaaa-bbbb-cccc-dddddddddddd}");
        const auto fallbackSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{62a75f00-eeee-ffff-1111-222222222222}");

        std::vector<ConnectionStateEventRecord> connectionStates;
        const auto protocolToken = page->ProtocolVtSequenceReceived([&](auto&&, const winrt::hstring& eventJson) {
            _recordConnectionStateEvent(eventJson, connectionStates);
        });
        const auto protocolTokenRevoker = wil::scope_exit([&]() noexcept {
            page->ProtocolVtSequenceReceived(protocolToken);
        });

        winrt::com_ptr<TestConnection> connection;
        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            connection = winrt::make_self<TestConnection>(
                liveSessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);

            NewTerminalArgs args{};
            args.ContentId(content.Id());
            args.SessionId(fallbackSessionId);
            args.AgentSessionId(L"agent-session-ended");
            args.AgentSessionAgent(L"copilot");
            args.AgentResumeCommandline(L"copilot --resume agent-session-ended");

            const auto attachedPane = page->_MakeTerminalPane(args);
            VERIFY_IS_NOT_NULL(attachedPane);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_paneAgentSessions.count(liveSessionId)));
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_paneAgentSessions.count(fallbackSessionId)));
        });

        TestOnUIThread([&]() {
            connection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);
        });

        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_paneAgentSessions.count(liveSessionId)));
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_paneAgentSessions.count(fallbackSessionId)));

            const auto closedStates = _statesForPane(connectionStates, _formatPaneId(liveSessionId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(closedStates.size()));
            VERIFY_ARE_EQUAL(std::string{ "closed" }, closedStates.at(0));
        });
    }

    void TabTests::GetWindowLayoutIncludesAgentRestoreMetadata()
    {
        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        TestOnUIThread([&]() {
            const auto tab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_IS_NOT_NULL(tab);

            page->_SplitPane(nullptr, SplitDirection::Right, 0.5f, page->_MakePane(nullptr, page->_GetFocusedTab(), nullptr));
            VERIFY_ARE_EQUAL(2, tab->GetLeafPaneCount());

            page->_paneAgentSessions.clear();

            const std::array agentSessionIds{
                winrt::hstring{ L"agent-session-1" },
                winrt::hstring{ L"agent-session-2" }
            };
            const std::array agentIds{
                winrt::hstring{ L"copilot" },
                winrt::hstring{ L"claude" }
            };
            const std::array resumeCommandlines{
                winrt::hstring{ L"copilot --resume agent-session-1" },
                winrt::hstring{ L"claude --resume agent-session-2" }
            };

            auto paneIndex = 0u;
            tab->GetRootPane()->WalkTree([&](const auto& pane) {
                if (pane->IsAgentPane())
                {
                    return;
                }

                const auto control = pane->GetTerminalControl();
                VERIFY_IS_NOT_NULL(control);

                const auto connection = control.Connection();
                VERIFY_IS_NOT_NULL(connection);

                auto& binding = page->_paneAgentSessions[connection.SessionId()];
                binding.sessionId = agentSessionIds.at(paneIndex);
                binding.agent = agentIds.at(paneIndex);
                binding.resumeCommandline = resumeCommandlines.at(paneIndex);
                paneIndex += 1;
            });
            VERIFY_ARE_EQUAL(2u, paneIndex);

            const auto persistedLayout = page->GetWindowLayout();
            VERIFY_IS_NOT_NULL(persistedLayout);

            const auto roundTrippedLayout = WindowLayout::FromJson(WindowLayout::ToJson(persistedLayout));
            VERIFY_IS_NOT_NULL(roundTrippedLayout);

            const auto persistedActions = roundTrippedLayout.TabLayout();
            VERIFY_IS_NOT_NULL(persistedActions);
            VERIFY_ARE_EQUAL(2u, persistedActions.Size());

            VERIFY_ARE_EQUAL(ShortcutAction::NewTab, persistedActions.GetAt(0).Action());
            const auto firstTerminalArgs = _getTerminalArgs(persistedActions.GetAt(0));
            VERIFY_IS_NOT_NULL(firstTerminalArgs);
            if (const auto firstBinding = page->_paneAgentSessions.find(firstTerminalArgs.SessionId());
                firstBinding != page->_paneAgentSessions.end())
            {
                VERIFY_ARE_EQUAL(firstBinding->second.sessionId, firstTerminalArgs.AgentSessionId());
                VERIFY_ARE_EQUAL(firstBinding->second.agent, firstTerminalArgs.AgentSessionAgent());
                VERIFY_ARE_EQUAL(firstBinding->second.resumeCommandline, firstTerminalArgs.AgentResumeCommandline());
            }
            else
            {
                VERIFY_FAIL(L"Expected the first persisted pane to keep its agent session metadata.");
            }

            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, persistedActions.GetAt(1).Action());
            const auto secondTerminalArgs = _getTerminalArgs(persistedActions.GetAt(1));
            VERIFY_IS_NOT_NULL(secondTerminalArgs);
            if (const auto secondBinding = page->_paneAgentSessions.find(secondTerminalArgs.SessionId());
                secondBinding != page->_paneAgentSessions.end())
            {
                VERIFY_ARE_EQUAL(secondBinding->second.sessionId, secondTerminalArgs.AgentSessionId());
                VERIFY_ARE_EQUAL(secondBinding->second.agent, secondTerminalArgs.AgentSessionAgent());
                VERIFY_ARE_EQUAL(secondBinding->second.resumeCommandline, secondTerminalArgs.AgentResumeCommandline());
            }
            else
            {
                VERIFY_FAIL(L"Expected the split pane to keep its agent session metadata.");
            }
        });
    }

    void TabTests::PersistStateIncludesAgentRestoreMetadata()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        auto applicationState = ApplicationState::SharedInstance();
        applicationState.Reset();
        const auto resetState = wil::scope_exit([&]() {
            applicationState.Reset();
        });

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);

            // A shell pane running an agent CLI is the thing that has to come
            // back after a close or a crash, so bind one and let the ordinary
            // state.json persist path carry it.
            page->_paneAgentSessions.clear();
            const auto control = tab->GetActiveTerminalControl();
            VERIFY_IS_NOT_NULL(control);
            const auto connection = control.Connection();
            VERIFY_IS_NOT_NULL(connection);

            auto& binding = page->_paneAgentSessions[connection.SessionId()];
            binding.sessionId = L"agent-session-persisted";
            binding.agent = L"copilot";
            binding.resumeCommandline = L"copilot --resume agent-session-persisted";

            page->PersistState();
        });

        const auto persistedLayouts = applicationState.PersistedWindowLayouts();
        VERIFY_IS_NOT_NULL(persistedLayouts);
        VERIFY_ARE_EQUAL(1u, persistedLayouts.Size());

        const auto persistedActions = persistedLayouts.GetAt(0).TabLayout();
        VERIFY_IS_NOT_NULL(persistedActions);
        VERIFY_ARE_EQUAL(1u, persistedActions.Size());
        VERIFY_ARE_EQUAL(ShortcutAction::NewTab, persistedActions.GetAt(0).Action());

        const auto terminalArgs = _getTerminalArgs(persistedActions.GetAt(0));
        VERIFY_IS_NOT_NULL(terminalArgs);
        VERIFY_ARE_EQUAL(winrt::hstring{ L"agent-session-persisted" }, terminalArgs.AgentSessionId());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"copilot" }, terminalArgs.AgentSessionAgent());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"copilot --resume agent-session-persisted" },
                         terminalArgs.AgentResumeCommandline());
    }

    void TabTests::NaturalClosedEventThenNotifyPanesClosingEmitsOnce()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        std::vector<ConnectionStateEventRecord> connectionStates;
        const auto token = page->ProtocolVtSequenceReceived([&](auto&&, const winrt::hstring& eventJson) {
            _recordConnectionStateEvent(eventJson, connectionStates);
        });
        const auto revokeToken = wil::scope_exit([&]() noexcept {
            page->ProtocolVtSequenceReceived(token);
        });

        winrt::com_ptr<TestConnection> connection;
        winrt::com_ptr<winrt::TerminalApp::implementation::Tab> tab;
        std::string expectedPaneId;

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{12345678-1234-5678-9abc-def012345678}");
            connection = winrt::make_self<TestConnection>(
                sessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(connection);

            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
            VERIFY_IS_NOT_NULL(content);

            NewTerminalArgs newTerminalArgs{};
            newTerminalArgs.ContentId(content.Id());
            VERIFY_SUCCEEDED(page->_OpenNewTab(newTerminalArgs));
            tab = page->_GetTabImpl(page->_tabs.GetAt(page->_tabs.Size() - 1));
            VERIFY_IS_NOT_NULL(tab);

            expectedPaneId = _formatPaneId(sessionId);
        });

        TestOnUIThread([&]() {
            connection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);
        });

        TestOnUIThread([&]() {
            const auto paneStates = _statesForPane(connectionStates, expectedPaneId);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(paneStates.size()));
            VERIFY_ARE_EQUAL(std::string{ "closed" }, paneStates.at(0));

            page->_NotifyPanesClosing(tab->GetRootPane());
        });

        TestOnUIThread([&]() {
            const auto paneStates = _statesForPane(connectionStates, expectedPaneId);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(paneStates.size()));
            VERIFY_ARE_EQUAL(std::string{ "closed" }, paneStates.at(0));
        });
    }

    void TabTests::NaturalFailedEventThenNotifyPanesClosingEmitsOnce()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        std::vector<ConnectionStateEventRecord> connectionStates;
        const auto token = page->ProtocolVtSequenceReceived([&](auto&&, const winrt::hstring& eventJson) {
            _recordConnectionStateEvent(eventJson, connectionStates);
        });
        const auto revokeToken = wil::scope_exit([&]() noexcept {
            page->ProtocolVtSequenceReceived(token);
        });

        winrt::guid sessionId{};
        winrt::com_ptr<TestConnection> connection;
        winrt::com_ptr<winrt::TerminalApp::implementation::Tab> tab;
        std::string expectedPaneId;

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{22345678-1234-5678-9abc-def012345678}");
            connection = winrt::make_self<TestConnection>(
                sessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(connection);

            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
            VERIFY_IS_NOT_NULL(content);

            NewTerminalArgs newTerminalArgs{};
            newTerminalArgs.ContentId(content.Id());
            VERIFY_SUCCEEDED(page->_OpenNewTab(newTerminalArgs));
            tab = page->_GetTabImpl(page->_tabs.GetAt(page->_tabs.Size() - 1));
            VERIFY_IS_NOT_NULL(tab);

            expectedPaneId = _formatPaneId(sessionId);
            page->_paneAgentSessions.insert_or_assign(
                sessionId,
                winrt::TerminalApp::implementation::TerminalPage::_PaneAgentSession{
                    L"agent-session-id",
                    L"copilot",
                    L"wta resume" });
        });

        TestOnUIThread([&]() {
            connection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Failed);
        });

        TestOnUIThread([&]() {
            const auto paneStates = _statesForPane(connectionStates, expectedPaneId);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(paneStates.size()));
            VERIFY_ARE_EQUAL(std::string{ "failed" }, paneStates.at(0));
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_paneAgentSessions.count(sessionId)));

            page->_NotifyPanesClosing(tab->GetRootPane());
            connection->SetState(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);
            connection->RaiseStateChanged();
        });

        TestOnUIThread([&]() {
            const auto paneStates = _statesForPane(connectionStates, expectedPaneId);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(paneStates.size()));
            VERIFY_ARE_EQUAL(std::string{ "failed" }, paneStates.at(0));
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_paneAgentSessions.count(sessionId)));
        });
    }

    void TabTests::ReusedSessionIdAcrossControlLifetimesEmitsEndStatePerLifetime()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        std::vector<ConnectionStateEventRecord> connectionStates;
        const auto token = page->ProtocolVtSequenceReceived([&](auto&&, const winrt::hstring& eventJson) {
            _recordConnectionStateEvent(eventJson, connectionStates);
        });
        const auto revokeToken = wil::scope_exit([&]() noexcept {
            page->ProtocolVtSequenceReceived(token);
        });

        const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{2dd44247-7f42-4f3e-a10b-0123456789ab}");
        const auto expectedPaneId = _formatPaneId(sessionId);

        winrt::com_ptr<TestConnection> firstConnection;
        winrt::com_ptr<TestConnection> secondConnection;
        winrt::Microsoft::Terminal::Control::TermControl firstControl{ nullptr };
        winrt::Microsoft::Terminal::Control::TermControl secondControl{ nullptr };

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            firstConnection = winrt::make_self<TestConnection>(
                sessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(firstConnection);

            const auto content = _contentManager->CreateCore(*settings, *settings, *firstConnection);
            VERIFY_IS_NOT_NULL(content);

            firstControl = page->_AttachControlToContent(content.Id());
            VERIFY_IS_TRUE(!!firstControl);
        });

        TestOnUIThread([&]() {
            firstConnection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);
        });

        TestOnUIThread([&]() {
            const auto paneStates = _statesForPane(connectionStates, expectedPaneId);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(paneStates.size()));
            VERIFY_ARE_EQUAL(std::string{ "closed" }, paneStates.at(0));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_panesWithEmittedTerminalEndState.size()));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_panesWithEmittedTerminalEndState.count(expectedPaneId)));

            firstConnection->RaiseStateChanged();
        });

        TestOnUIThread([&]() {
            const auto paneStates = _statesForPane(connectionStates, expectedPaneId);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(paneStates.size()));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_panesWithEmittedTerminalEndState.size()));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_panesWithEmittedTerminalEndState.count(expectedPaneId)));

            firstControl = nullptr;
        });

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            secondConnection = winrt::make_self<TestConnection>(
                sessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(secondConnection);

            const auto content = _contentManager->CreateCore(*settings, *settings, *secondConnection);
            VERIFY_IS_NOT_NULL(content);

            secondControl = page->_AttachControlToContent(content.Id());
            VERIFY_IS_TRUE(!!secondControl);

            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_panesWithEmittedTerminalEndState.size()));
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_panesWithEmittedTerminalEndState.count(expectedPaneId)));
        });

        TestOnUIThread([&]() {
            secondConnection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);
        });

        TestOnUIThread([&]() {
            const auto paneStates = _statesForPane(connectionStates, expectedPaneId);
            VERIFY_ARE_EQUAL(2u, static_cast<unsigned int>(paneStates.size()));
            VERIFY_ARE_EQUAL(std::string{ "closed" }, paneStates.at(0));
            VERIFY_ARE_EQUAL(std::string{ "closed" }, paneStates.at(1));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_panesWithEmittedTerminalEndState.size()));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_panesWithEmittedTerminalEndState.count(expectedPaneId)));

            secondConnection->RaiseStateChanged();
        });

        TestOnUIThread([&]() {
            const auto paneStates = _statesForPane(connectionStates, expectedPaneId);
            VERIFY_ARE_EQUAL(2u, static_cast<unsigned int>(paneStates.size()));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_panesWithEmittedTerminalEndState.size()));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_panesWithEmittedTerminalEndState.count(expectedPaneId)));
        });
    }
    void TabTests::SyntheticFailedEventSuppressesDelayedNormalCallback()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        std::vector<ConnectionStateEventRecord> connectionStates;
        const auto token = page->ProtocolVtSequenceReceived([&](auto&&, const winrt::hstring& eventJson) {
            _recordConnectionStateEvent(eventJson, connectionStates);
        });
        const auto revokeToken = wil::scope_exit([&]() noexcept {
            page->ProtocolVtSequenceReceived(token);
        });

        winrt::com_ptr<TestConnection> connection;
        winrt::com_ptr<winrt::TerminalApp::implementation::Tab> tab;
        std::string expectedPaneId;

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{32345678-1234-5678-9abc-def012345678}");
            connection = winrt::make_self<TestConnection>(
                sessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(connection);

            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
            VERIFY_IS_NOT_NULL(content);

            NewTerminalArgs newTerminalArgs{};
            newTerminalArgs.ContentId(content.Id());
            VERIFY_SUCCEEDED(page->_OpenNewTab(newTerminalArgs));
            tab = page->_GetTabImpl(page->_tabs.GetAt(page->_tabs.Size() - 1));
            VERIFY_IS_NOT_NULL(tab);

            expectedPaneId = _formatPaneId(sessionId);

            connection->SetState(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Failed);
            page->_NotifyPanesClosing(tab->GetRootPane());
            connection->RaiseStateChanged();
        });

        TestOnUIThread([&]() {
            const auto paneStates = _statesForPane(connectionStates, expectedPaneId);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(paneStates.size()));
            VERIFY_ARE_EQUAL(std::string{ "failed" }, paneStates.at(0));
        });
    }

    void TabTests::CloseNonLastPaneEmitsOneEndStateWithoutKeepingTheTab()
    {
        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        std::vector<ConnectionStateEventRecord> connectionStates;
        const auto token = page->ProtocolVtSequenceReceived([&](auto&&, const winrt::hstring& eventJson) {
            _recordConnectionStateEvent(eventJson, connectionStates);
        });
        const auto revokeToken = wil::scope_exit([&]() noexcept {
            page->ProtocolVtSequenceReceived(token);
        });

        std::string closedPaneId;
        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);

            page->_SplitPane(nullptr, SplitDirection::Right, 0.5f, page->_MakePane(nullptr, page->_GetFocusedTab(), nullptr));
            VERIFY_ARE_EQUAL(2, tab->GetLeafPaneCount());

            const auto pane = tab->GetActivePane();
            VERIFY_IS_NOT_NULL(pane);
            const auto control = pane->GetTerminalControl();
            VERIFY_IS_NOT_NULL(control);
            closedPaneId = _formatPaneId(control.Connection().SessionId());

            page->_HandleClosePaneRequested(pane);

            VERIFY_ARE_EQUAL(1, tab->GetLeafPaneCount());
        });

        const auto states = _statesForPane(connectionStates, closedPaneId);
        VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(states.size()));
        VERIFY_ARE_EQUAL(std::string{ "closed" }, states.at(0));
    }

    // Method Description:
    // - This is a helper to set up a TerminalPage for a unittest. This method
    //   does a couple things:
    //   * Create()'s a TerminalPage with the given settings. Constructing a
    //     TerminalPage so that we can get at its implementation is wacky, so
    //     this helper will do it correctly for you, even if this doesn't make a
    //     ton of sense on the surface. This is also why you need to pass both a
    //     projection and a com_ptr to this method.
    //   * It will use the provided settings object to initialize the TerminalPage
    //   * It will add the TerminalPage to the test Application, so that we can
    //     get actual layout events. Much of the Terminal assumes there's a
    //     non-zero ActualSize to the Terminal window, and adding the Page to
    //     the Application will make it behave as expected.
    //   * It will wait for the TerminalPage to finish initialization before
    //     returning control to the caller. It does this by creating an event and
    //     only setting the event when the TerminalPage raises its Initialized
    //     event, to signal that startup is complete. At this point, there will
    //     be one tab with the default profile in the page.
    //   * It will also ensure that the first tab is focused, since that happens
    //     asynchronously in the application typically.
    // Arguments:
    // - page: a TerminalPage implementation ptr that will receive the new TerminalPage instance
    // - initialSettings: a CascadiaSettings to initialize the TerminalPage with.
    // Return Value:
    // - <none>
    void TabTests::_initializeTerminalPage(winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage>& page,
                                           CascadiaSettings initialSettings)
    {
        // This is super wacky, but we can't just initialize the
        // com_ptr<impl::TerminalPage> in the lambda and assign it back out of
        // the lambda. We'll crash trying to get a weak_ref to the TerminalPage
        // during TerminalPage::Create() below.
        //
        // Instead, create the winrt object, then get a com_ptr to the
        // implementation _from_ the winrt object. This seems to work, even if
        // it's weird.
        winrt::TerminalApp::TerminalPage projectedPage{ nullptr };

        _windowProperties = winrt::make_self<winrt::TerminalApp::implementation::WindowProperties>();
        winrt::TerminalApp::WindowProperties props = *_windowProperties;
        _createContentManager();
        winrt::TerminalApp::ContentManager contentManager = *_contentManager;
        Log::Comment(NoThrowString().Format(L"Construct the TerminalPage"));
        auto result = RunOnUIThread([&projectedPage, &page, initialSettings, props, contentManager]() {
            projectedPage = winrt::TerminalApp::TerminalPage(props, contentManager);
            page.copy_from(winrt::get_self<winrt::TerminalApp::implementation::TerminalPage>(projectedPage));
            page->_settings = initialSettings;
        });
        VERIFY_SUCCEEDED(result);

        VERIFY_IS_NOT_NULL(page);
        VERIFY_IS_NOT_NULL(page->_settings);

        ::details::Event waitForInitEvent;
        if (!waitForInitEvent.IsValid())
        {
            VERIFY_SUCCEEDED(HRESULT_FROM_WIN32(::GetLastError()));
        }
        page->Initialized([&waitForInitEvent](auto&&, auto&&) {
            waitForInitEvent.Set();
        });

        Log::Comment(L"Create() the TerminalPage");

        result = RunOnUIThread([&page]() {
            VERIFY_IS_NOT_NULL(page);
            VERIFY_IS_NOT_NULL(page->_settings);
            page->Create();
            Log::Comment(L"Create()'d the page successfully");

            // Build a NewTab action, to make sure we start with one. The real
            // Terminal will always get one from AppCommandlineArgs.
            NewTerminalArgs newTerminalArgs{};
            NewTabArgs args{ newTerminalArgs };
            ActionAndArgs newTabAction{ ShortcutAction::NewTab, args };
            // push the arg onto the front
            page->_startupActions.push_back(std::move(newTabAction));
            Log::Comment(L"Added a single newTab action");

            auto app = ::winrt::Windows::UI::Xaml::Application::Current();

            winrt::TerminalApp::TerminalPage pp = *page;
            winrt::Windows::UI::Xaml::Window::Current().Content(pp);
            winrt::Windows::UI::Xaml::Window::Current().Activate();
        });
        VERIFY_SUCCEEDED(result);

        Log::Comment(L"Wait for the page to finish initializing...");
        VERIFY_SUCCEEDED(waitForInitEvent.Wait());
        Log::Comment(L"...Done");

        result = RunOnUIThread([&page]() {
            // In the real app, this isn't a problem, but doesn't happen
            // reliably in the unit tests.
            Log::Comment(L"Ensure we set the first tab as the selected one.");
            auto tab = page->_tabs.GetAt(0);
            auto tabImpl = page->_GetTabImpl(tab);
            page->_tabView.SelectedItem(tabImpl->TabViewItem());
            page->_UpdatedSelectedTab(tab);
        });
        VERIFY_SUCCEEDED(result);
    }

    void TabTests::TryInitializePage()
    {
        // This is a very simple test to prove we can create settings and a
        // TerminalPage and not only create them successfully, but also create a
        // tab using those settings successfully.

        // - - - IMPORTANT - - -
        // GH#14623: "closeOnExit": "never" is important for all test profiles. Without
        // it, the spawned process exits immediately in the UAP test environment,
        // and the default "automatic" close-on-exit behavior removes the
        // tab/pane asynchronously, racing against test assertions.
        static constexpr std::wstring_view settingsJson0{ LR"(
        {
            "defaultProfile": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
            "profiles": [
                {
                    "name" : "profile0",
                    "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
                    "historySize": 1,
                    "closeOnExit": "never"
                },
                {
                    "name" : "profile1",
                    "guid": "{6239a42c-2222-49a3-80bd-e8fdd045185c}",
                    "historySize": 2,
                    "closeOnExit": "never"
                }
            ]
        })" };

        CascadiaSettings settings0{ settingsJson0, {} };
        VERIFY_IS_NOT_NULL(settings0);

        // This is super wacky, but we can't just initialize the
        // com_ptr<impl::TerminalPage> in the lambda and assign it back out of
        // the lambda. We'll crash trying to get a weak_ref to the TerminalPage
        // during TerminalPage::Create() below.
        //
        // Instead, create the winrt object, then get a com_ptr to the
        // implementation _from_ the winrt object. This seems to work, even if
        // it's weird.
        winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage> page{ nullptr };
        _initializeTerminalPage(page, settings0);

        auto result = RunOnUIThread([&page]() {
            VERIFY_ARE_EQUAL(1u, page->_tabs.Size());
        });
        VERIFY_SUCCEEDED(result);
    }

    void TabTests::TryDuplicateBadTab()
    {
        // * Create a tab with a profile with GUID 1
        // * Reload the settings so that GUID 1 is no longer in the list of profiles
        // * Try calling _DuplicateFocusedTab on tab 1
        // * No new tab should be created (and more importantly, the app should not crash)
        //
        // Created to test GH#2455

        static constexpr std::wstring_view settingsJson0{ LR"(
        {
            "defaultProfile": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
            "profiles": [
                {
                    "name" : "profile0",
                    "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
                    "historySize": 1,
                    "closeOnExit": "never"
                },
                {
                    "name" : "profile1",
                    "guid": "{6239a42c-2222-49a3-80bd-e8fdd045185c}",
                    "historySize": 2,
                    "closeOnExit": "never"
                }
            ]
        })" };

        static constexpr std::wstring_view settingsJson1{ LR"(
        {
            "defaultProfile": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
            "profiles": [
                {
                    "name" : "profile1",
                    "guid": "{6239a42c-2222-49a3-80bd-e8fdd045185c}",
                    "historySize": 2,
                    "closeOnExit": "never"
                }
            ]
        })" };

        CascadiaSettings settings0{ settingsJson0, {} };
        VERIFY_IS_NOT_NULL(settings0);

        CascadiaSettings settings1{ settingsJson1, {} };
        VERIFY_IS_NOT_NULL(settings1);

        const auto guid1 = Microsoft::Console::Utils::GuidFromString(L"{6239a42c-1111-49a3-80bd-e8fdd045185c}");
        const auto guid2 = Microsoft::Console::Utils::GuidFromString(L"{6239a42c-2222-49a3-80bd-e8fdd045185c}");
        const auto guid3 = Microsoft::Console::Utils::GuidFromString(L"{6239a42c-3333-49a3-80bd-e8fdd045185c}");

        // This is super wacky, but we can't just initialize the
        // com_ptr<impl::TerminalPage> in the lambda and assign it back out of
        // the lambda. We'll crash trying to get a weak_ref to the TerminalPage
        // during TerminalPage::Create() below.
        //
        // Instead, create the winrt object, then get a com_ptr to the
        // implementation _from_ the winrt object. This seems to work, even if
        // it's weird.
        winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage> page{ nullptr };
        _initializeTerminalPage(page, settings0);

        auto result = RunOnUIThread([&page]() {
            VERIFY_ARE_EQUAL(1u, page->_tabs.Size());
        });
        VERIFY_SUCCEEDED(result);

        Log::Comment(L"Duplicate the first tab");
        result = RunOnUIThread([&page]() {
            page->_DuplicateFocusedTab();
            VERIFY_ARE_EQUAL(2u, page->_tabs.Size());
        });
        VERIFY_SUCCEEDED(result);

        Log::Comment(NoThrowString().Format(
            L"Change the settings of the TerminalPage so the first profile is "
            L"no longer in the list of profiles"));
        result = RunOnUIThread([&page, settings1]() {
            page->_settings = settings1;
        });
        VERIFY_SUCCEEDED(result);

        Log::Comment(L"Duplicate the tab, and don't crash");
        result = RunOnUIThread([&page]() {
            page->_DuplicateFocusedTab();
            VERIFY_ARE_EQUAL(3u, page->_tabs.Size(), L"We should successfully duplicate a tab hosting a deleted profile.");
        });
        VERIFY_SUCCEEDED(result);
    }

    void TabTests::TryDuplicateBadPane()
    {
        // * Create a tab with a profile with GUID 1
        // * Reload the settings so that GUID 1 is no longer in the list of profiles
        // * Try calling _SplitPane(Duplicate) on tab 1
        // * No new pane should be created (and more importantly, the app should not crash)
        //
        // Created to test GH#2455

        static constexpr std::wstring_view settingsJson0{ LR"(
        {
            "defaultProfile": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
            "profiles": [
                {
                    "name" : "profile0",
                    "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
                    "historySize": 1,
                    "closeOnExit": "never"
                },
                {
                    "name" : "profile1",
                    "guid": "{6239a42c-2222-49a3-80bd-e8fdd045185c}",
                    "historySize": 2,
                    "closeOnExit": "never"
                }
            ]
        })" };

        static constexpr std::wstring_view settingsJson1{ LR"(
        {
            "defaultProfile": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
            "profiles": [
                {
                    "name" : "profile1",
                    "guid": "{6239a42c-2222-49a3-80bd-e8fdd045185c}",
                    "historySize": 2,
                    "closeOnExit": "never"
                }
            ]
        })" };

        CascadiaSettings settings0{ settingsJson0, {} };
        VERIFY_IS_NOT_NULL(settings0);

        CascadiaSettings settings1{ settingsJson1, {} };
        VERIFY_IS_NOT_NULL(settings1);

        const auto guid1 = Microsoft::Console::Utils::GuidFromString(L"{6239a42c-1111-49a3-80bd-e8fdd045185c}");
        const auto guid2 = Microsoft::Console::Utils::GuidFromString(L"{6239a42c-2222-49a3-80bd-e8fdd045185c}");
        const auto guid3 = Microsoft::Console::Utils::GuidFromString(L"{6239a42c-3333-49a3-80bd-e8fdd045185c}");

        // This is super wacky, but we can't just initialize the
        // com_ptr<impl::TerminalPage> in the lambda and assign it back out of
        // the lambda. We'll crash trying to get a weak_ref to the TerminalPage
        // during TerminalPage::Create() below.
        //
        // Instead, create the winrt object, then get a com_ptr to the
        // implementation _from_ the winrt object. This seems to work, even if
        // it's weird.
        winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage> page{ nullptr };
        _initializeTerminalPage(page, settings0);

        auto result = RunOnUIThread([&page]() {
            VERIFY_ARE_EQUAL(1u, page->_tabs.Size());
        });
        VERIFY_SUCCEEDED(result);

        result = RunOnUIThread([&page]() {
            VERIFY_ARE_EQUAL(1u, page->_tabs.Size());
            auto tab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_ARE_EQUAL(1, tab->GetLeafPaneCount());
        });
        VERIFY_SUCCEEDED(result);

        Log::Comment(NoThrowString().Format(L"Duplicate the first pane"));
        result = RunOnUIThread([&page]() {
            page->_SplitPane(nullptr, SplitDirection::Automatic, 0.5f, page->_MakePane(nullptr, page->_GetFocusedTab(), nullptr));

            VERIFY_ARE_EQUAL(1u, page->_tabs.Size());
            auto tab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_ARE_EQUAL(2, tab->GetLeafPaneCount());
        });
        VERIFY_SUCCEEDED(result);

        Log::Comment(NoThrowString().Format(
            L"Change the settings of the TerminalPage so the first profile is "
            L"no longer in the list of profiles"));
        result = RunOnUIThread([&page, settings1]() {
            page->_settings = settings1;
        });
        VERIFY_SUCCEEDED(result);

        Log::Comment(NoThrowString().Format(L"Duplicate the pane, and don't crash"));
        result = RunOnUIThread([&page]() {
            page->_SplitPane(nullptr, SplitDirection::Automatic, 0.5f, page->_MakePane(nullptr, page->_GetFocusedTab(), nullptr));

            VERIFY_ARE_EQUAL(1u, page->_tabs.Size());
            auto tab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_ARE_EQUAL(3,
                             tab->GetLeafPaneCount(),
                             L"We should successfully duplicate a pane hosting a deleted profile.");
        });
        VERIFY_SUCCEEDED(result);

        auto cleanup = wil::scope_exit([] {
            auto result = RunOnUIThread([]() {
                // There's something causing us to crash north of
                // TSFInputControl::NotifyEnter, or LayoutRequested. It's very
                // unclear what that issue is. Since these tests don't run in
                // CI, simply log a message so that the dev running these tests
                // knows it's expected.
                Log::Comment(L"This test often crashes on cleanup, even when it succeeds. If it succeeded, then crashes, that's okay.");
            });
            VERIFY_SUCCEEDED(result);
        });
    }

    // Method Description:
    // - This is a helper method for setting up a TerminalPage with some common
    //   settings, and creating the first tab.
    // Arguments:
    // - <none>
    // Return Value:
    // - The initialized TerminalPage, ready to use.
    winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage> TabTests::_commonSetup()
    {
        static constexpr std::wstring_view settingsJson0{ LR"(
        {
            "defaultProfile": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
            "showTabsInTitlebar": false,
            "profiles": [
                {
                    "name" : "profile0",
                    "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
                    "tabTitle" : "Profile 0",
                    "historySize": 1,
                    "closeOnExit": "never"
                },
                {
                    "name" : "profile1",
                    "guid": "{6239a42c-2222-49a3-80bd-e8fdd045185c}",
                    "tabTitle" : "Profile 1",
                    "historySize": 2,
                    "closeOnExit": "never"
                },
                {
                    "name" : "profile2",
                    "guid": "{6239a42c-3333-49a3-80bd-e8fdd045185c}",
                    "tabTitle" : "Profile 2",
                    "historySize": 3,
                    "closeOnExit": "never"
                },
                {
                    "name" : "profile3",
                    "guid": "{6239a42c-4444-49a3-80bd-e8fdd045185c}",
                    "tabTitle" : "Profile 3",
                    "historySize": 4,
                    "closeOnExit": "never"
                }
            ],
            "schemes":
            [
                {
                    "name": "Campbell",
                    "foreground": "#CCCCCC",
                    "background": "#0C0C0C",
                    "cursorColor": "#FFFFFF",
                    "black": "#0C0C0C",
                    "red": "#C50F1F",
                    "green": "#13A10E",
                    "yellow": "#C19C00",
                    "blue": "#0037DA",
                    "purple": "#881798",
                    "cyan": "#3A96DD",
                    "white": "#CCCCCC",
                    "brightBlack": "#767676",
                    "brightRed": "#E74856",
                    "brightGreen": "#16C60C",
                    "brightYellow": "#F9F1A5",
                    "brightBlue": "#3B78FF",
                    "brightPurple": "#B4009E",
                    "brightCyan": "#61D6D6",
                    "brightWhite": "#F2F2F2"
                },
                {
                    "name": "Vintage",
                    "foreground": "#C0C0C0",
                    "background": "#000000",
                    "cursorColor": "#FFFFFF",
                    "black": "#000000",
                    "red": "#800000",
                    "green": "#008000",
                    "yellow": "#808000",
                    "blue": "#000080",
                    "purple": "#800080",
                    "cyan": "#008080",
                    "white": "#C0C0C0",
                    "brightBlack": "#808080",
                    "brightRed": "#FF0000",
                    "brightGreen": "#00FF00",
                    "brightYellow": "#FFFF00",
                    "brightBlue": "#0000FF",
                    "brightPurple": "#FF00FF",
                    "brightCyan": "#00FFFF",
                    "brightWhite": "#FFFFFF"
                },
                {
                    "name": "One Half Light",
                    "foreground": "#383A42",
                    "background": "#FAFAFA",
                    "cursorColor": "#4F525D",
                    "black": "#383A42",
                    "red": "#E45649",
                    "green": "#50A14F",
                    "yellow": "#C18301",
                    "blue": "#0184BC",
                    "purple": "#A626A4",
                    "cyan": "#0997B3",
                    "white": "#FAFAFA",
                    "brightBlack": "#4F525D",
                    "brightRed": "#DF6C75",
                    "brightGreen": "#98C379",
                    "brightYellow": "#E4C07A",
                    "brightBlue": "#61AFEF",
                    "brightPurple": "#C577DD",
                    "brightCyan": "#56B5C1",
                    "brightWhite": "#FFFFFF"
                }
            ]
        })" };

        CascadiaSettings settings0{ settingsJson0, {} };
        VERIFY_IS_NOT_NULL(settings0);

        const auto guid1 = Microsoft::Console::Utils::GuidFromString(L"{6239a42c-1111-49a3-80bd-e8fdd045185c}");
        const auto guid2 = Microsoft::Console::Utils::GuidFromString(L"{6239a42c-2222-49a3-80bd-e8fdd045185c}");

        // This is super wacky, but we can't just initialize the
        // com_ptr<impl::TerminalPage> in the lambda and assign it back out of
        // the lambda. We'll crash trying to get a weak_ref to the TerminalPage
        // during TerminalPage::Create() below.
        //
        // Instead, create the winrt object, then get a com_ptr to the
        // implementation _from_ the winrt object. This seems to work, even if
        // it's weird.
        winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage> page{ nullptr };
        _initializeTerminalPage(page, settings0);

        auto result = RunOnUIThread([&page]() {
            VERIFY_ARE_EQUAL(1u, page->_tabs.Size());
        });
        VERIFY_SUCCEEDED(result);

        return page;
    }

    void TabTests::TryZoomPane()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        auto page = _commonSetup();

        Log::Comment(L"Create a second pane");
        auto result = RunOnUIThread([&page]() {
            SplitPaneArgs args{ SplitType::Duplicate };
            ActionEventArgs eventArgs{ args };
            page->_HandleSplitPane(nullptr, eventArgs);
            auto firstTab = page->_GetTabImpl(page->_tabs.GetAt(0));

            VERIFY_ARE_EQUAL(2, firstTab->GetLeafPaneCount());
            VERIFY_IS_FALSE(firstTab->IsZoomed());
        });
        VERIFY_SUCCEEDED(result);

        Log::Comment(L"Zoom in on the pane");
        result = RunOnUIThread([&page]() {
            ActionEventArgs eventArgs{};
            page->_HandleTogglePaneZoom(nullptr, eventArgs);
            auto firstTab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_ARE_EQUAL(2, firstTab->GetLeafPaneCount());
            VERIFY_IS_TRUE(firstTab->IsZoomed());
        });
        VERIFY_SUCCEEDED(result);

        Log::Comment(L"Zoom out of the pane");
        result = RunOnUIThread([&page]() {
            ActionEventArgs eventArgs{};
            page->_HandleTogglePaneZoom(nullptr, eventArgs);
            auto firstTab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_ARE_EQUAL(2, firstTab->GetLeafPaneCount());
            VERIFY_IS_FALSE(firstTab->IsZoomed());
        });
        VERIFY_SUCCEEDED(result);
    }

    void TabTests::MoveFocusFromZoomedPane()
    {
        auto page = _commonSetup();

        Log::Comment(L"Create a second pane");
        auto result = RunOnUIThread([&page]() {
            // Set up action
            SplitPaneArgs args{ SplitType::Duplicate };
            ActionEventArgs eventArgs{ args };
            page->_HandleSplitPane(nullptr, eventArgs);
            auto firstTab = page->_GetTabImpl(page->_tabs.GetAt(0));

            VERIFY_ARE_EQUAL(2, firstTab->GetLeafPaneCount());
            VERIFY_IS_FALSE(firstTab->IsZoomed());
        });
        VERIFY_SUCCEEDED(result);

        Log::Comment(L"Zoom in on the pane");
        result = RunOnUIThread([&page]() {
            // Set up action
            ActionEventArgs eventArgs{};

            page->_HandleTogglePaneZoom(nullptr, eventArgs);

            auto firstTab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_ARE_EQUAL(2, firstTab->GetLeafPaneCount());
            VERIFY_IS_TRUE(firstTab->IsZoomed());
        });
        VERIFY_SUCCEEDED(result);

        Log::Comment(L"Move focus. We should still be zoomed.");
        result = RunOnUIThread([&page]() {
            // Set up action
            MoveFocusArgs args{ FocusDirection::Left };
            ActionEventArgs eventArgs{ args };

            page->_HandleMoveFocus(nullptr, eventArgs);

            auto firstTab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_ARE_EQUAL(2, firstTab->GetLeafPaneCount());
            VERIFY_IS_TRUE(firstTab->IsZoomed());
        });
        VERIFY_SUCCEEDED(result);
    }

    void TabTests::CloseZoomedPane()
    {
        auto page = _commonSetup();

        Log::Comment(L"Create a second pane");
        auto result = RunOnUIThread([&page]() {
            // Set up action
            SplitPaneArgs args{ SplitType::Duplicate };
            ActionEventArgs eventArgs{ args };
            page->_HandleSplitPane(nullptr, eventArgs);
            auto firstTab = page->_GetTabImpl(page->_tabs.GetAt(0));

            VERIFY_ARE_EQUAL(2, firstTab->GetLeafPaneCount());
            VERIFY_IS_FALSE(firstTab->IsZoomed());
        });
        VERIFY_SUCCEEDED(result);

        Log::Comment(L"Zoom in on the pane");
        result = RunOnUIThread([&page]() {
            // Set up action
            ActionEventArgs eventArgs{};

            page->_HandleTogglePaneZoom(nullptr, eventArgs);

            auto firstTab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_ARE_EQUAL(2, firstTab->GetLeafPaneCount());
            VERIFY_IS_TRUE(firstTab->IsZoomed());
        });
        VERIFY_SUCCEEDED(result);

        Log::Comment(L"Close Pane. This should cause us to un-zoom, and remove the second pane from the tree");
        result = RunOnUIThread([&page]() {
            // Set up action
            ActionEventArgs eventArgs{};

            page->_HandleClosePane(nullptr, eventArgs);

            auto firstTab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_IS_FALSE(firstTab->IsZoomed());
        });
        VERIFY_SUCCEEDED(result);

        // Introduce a slight delay to let the events finish propagating
        Sleep(250);

        Log::Comment(L"Check to ensure there's only one pane left.");

        result = RunOnUIThread([&page]() {
            auto firstTab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_ARE_EQUAL(1, firstTab->GetLeafPaneCount());
            VERIFY_IS_FALSE(firstTab->IsZoomed());
        });
        VERIFY_SUCCEEDED(result);
    }

    void TabTests::SwapPanes()
    {
        auto page = _commonSetup();

        Log::Comment(L"Setup 4 panes.");
        // Create the following layout
        // -------------------
        // |   1    |   2    |
        // |        |        |
        // -------------------
        // |   3    |   4    |
        // |        |        |
        // -------------------
        uint32_t firstId = 0, secondId = 0, thirdId = 0, fourthId = 0;
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(1u, page->_tabs.Size());
            auto tab = page->_GetTabImpl(page->_tabs.GetAt(0));
            firstId = tab->_activePane->Id().value();
            // We start with 1 tab, split vertically to get
            // -------------------
            // |   1    |   2    |
            // |        |        |
            // -------------------
            page->_SplitPane(nullptr, SplitDirection::Right, 0.5f, page->_MakePane(nullptr, page->_GetFocusedTab(), nullptr));
            secondId = tab->_activePane->Id().value();
        });
        Sleep(250);
        TestOnUIThread([&]() {
            // After this the `2` pane is focused, go back to `1` being focused
            page->_MoveFocus(FocusDirection::Left);
        });
        Sleep(250);
        TestOnUIThread([&]() {
            // Split again to make the 3rd tab
            // -------------------
            // |   1    |        |
            // |        |        |
            // ---------|   2    |
            // |   3    |        |
            // |        |        |
            // -------------------
            page->_SplitPane(nullptr, SplitDirection::Down, 0.5f, page->_MakePane(nullptr, page->_GetFocusedTab(), nullptr));
            auto tab = page->_GetTabImpl(page->_tabs.GetAt(0));
            // Split again to make the 3rd tab
            thirdId = tab->_activePane->Id().value();
        });
        Sleep(250);
        TestOnUIThread([&]() {
            // After this the `3` pane is focused, go back to `2` being focused
            page->_MoveFocus(FocusDirection::Right);
        });
        Sleep(250);
        TestOnUIThread([&]() {
            // Split to create the final pane
            // -------------------
            // |   1    |   2    |
            // |        |        |
            // -------------------
            // |   3    |   4    |
            // |        |        |
            // -------------------
            page->_SplitPane(nullptr, SplitDirection::Down, 0.5f, page->_MakePane(nullptr, page->_GetFocusedTab(), nullptr));
            auto tab = page->_GetTabImpl(page->_tabs.GetAt(0));
            fourthId = tab->_activePane->Id().value();
        });

        Sleep(250);
        TestOnUIThread([&]() {
            auto tab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_ARE_EQUAL(4, tab->GetLeafPaneCount());
            // just to be complete, make sure we actually have 4 different ids
            VERIFY_ARE_NOT_EQUAL(firstId, fourthId);
            VERIFY_ARE_NOT_EQUAL(secondId, fourthId);
            VERIFY_ARE_NOT_EQUAL(thirdId, fourthId);
            VERIFY_ARE_NOT_EQUAL(firstId, thirdId);
            VERIFY_ARE_NOT_EQUAL(secondId, thirdId);
            VERIFY_ARE_NOT_EQUAL(firstId, secondId);
        });

        // Gratuitous use of sleep to make sure that the UI has updated properly
        // after each operation.
        Sleep(250);
        // Now try to move the pane through the tree
        Log::Comment(L"Move pane to the left. This should swap panes 3 and 4");
        // -------------------
        // |   1    |   2    |
        // |        |        |
        // -------------------
        // |   4    |   3    |
        // |        |        |
        // -------------------
        TestOnUIThread([&]() {
            // Set up action
            SwapPaneArgs args{ FocusDirection::Left };
            ActionEventArgs eventArgs{ args };

            page->_HandleSwapPane(nullptr, eventArgs);
        });

        Sleep(250);

        TestOnUIThread([&]() {
            auto tab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_ARE_EQUAL(4, tab->GetLeafPaneCount());
            // Our currently focused pane should be `4`
            VERIFY_ARE_EQUAL(fourthId, tab->_activePane->Id().value());

            // Inspect the tree to make sure we swapped
            VERIFY_ARE_EQUAL(fourthId, tab->_rootPane->_firstChild->_secondChild->Id().value());
            VERIFY_ARE_EQUAL(thirdId, tab->_rootPane->_secondChild->_secondChild->Id().value());
        });

        Sleep(250);

        Log::Comment(L"Move pane to up. This should swap panes 1 and 4");
        // -------------------
        // |   4    |   2    |
        // |        |        |
        // -------------------
        // |   1    |   3    |
        // |        |        |
        // -------------------
        TestOnUIThread([&]() {
            // Set up action
            SwapPaneArgs args{ FocusDirection::Up };
            ActionEventArgs eventArgs{ args };

            page->_HandleSwapPane(nullptr, eventArgs);
        });

        Sleep(250);

        TestOnUIThread([&]() {
            auto tab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_ARE_EQUAL(4, tab->GetLeafPaneCount());
            // Our currently focused pane should be `4`
            VERIFY_ARE_EQUAL(fourthId, tab->_activePane->Id().value());

            // Inspect the tree to make sure we swapped
            VERIFY_ARE_EQUAL(fourthId, tab->_rootPane->_firstChild->_firstChild->Id().value());
            VERIFY_ARE_EQUAL(firstId, tab->_rootPane->_firstChild->_secondChild->Id().value());
        });

        Sleep(250);

        Log::Comment(L"Move pane to the right. This should swap panes 2 and 4");
        // -------------------
        // |   2    |   4    |
        // |        |        |
        // -------------------
        // |   1    |   3    |
        // |        |        |
        // -------------------
        TestOnUIThread([&]() {
            // Set up action
            SwapPaneArgs args{ FocusDirection::Right };
            ActionEventArgs eventArgs{ args };

            page->_HandleSwapPane(nullptr, eventArgs);
        });

        Sleep(250);

        TestOnUIThread([&]() {
            auto tab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_ARE_EQUAL(4, tab->GetLeafPaneCount());
            // Our currently focused pane should be `4`
            VERIFY_ARE_EQUAL(fourthId, tab->_activePane->Id().value());

            // Inspect the tree to make sure we swapped
            VERIFY_ARE_EQUAL(fourthId, tab->_rootPane->_secondChild->_firstChild->Id().value());
            VERIFY_ARE_EQUAL(secondId, tab->_rootPane->_firstChild->_firstChild->Id().value());
        });

        Sleep(250);

        Log::Comment(L"Move pane down. This should swap panes 3 and 4");
        // -------------------
        // |   2    |   3    |
        // |        |        |
        // -------------------
        // |   1    |   4    |
        // |        |        |
        // -------------------
        TestOnUIThread([&]() {
            // Set up action
            SwapPaneArgs args{ FocusDirection::Down };
            ActionEventArgs eventArgs{ args };

            page->_HandleSwapPane(nullptr, eventArgs);
        });

        Sleep(250);

        TestOnUIThread([&]() {
            auto tab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_ARE_EQUAL(4, tab->GetLeafPaneCount());
            // Our currently focused pane should be `4`
            VERIFY_ARE_EQUAL(fourthId, tab->_activePane->Id().value());

            // Inspect the tree to make sure we swapped
            VERIFY_ARE_EQUAL(fourthId, tab->_rootPane->_secondChild->_secondChild->Id().value());
            VERIFY_ARE_EQUAL(thirdId, tab->_rootPane->_secondChild->_firstChild->Id().value());
        });
    }

    void TabTests::TransferredAgentContentFirstPaneDefersTabRekey()
    {
        auto page = _commonSetup();
        const auto sourceProfileGuid = winrt::guid{ L"{6239a42c-5555-49a3-80bd-e8fdd045185c}" };
        const auto oldTabId = winrt::hstring{ L"source-tab-id" };

        TestOnUIThread([&]() {
            const auto focusedTab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(focusedTab);

            auto destinationAgentPane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            VERIFY_IS_NOT_NULL(destinationAgentPane);
            destinationAgentPane->IsAgentPane(true);
            page->_SplitPane(focusedTab, SplitDirection::Left, 0.5f, destinationAgentPane);
            VERIFY_IS_TRUE(focusedTab->FindAgentPane() != nullptr);
            VERIFY_IS_FALSE(focusedTab->AgentSourceProfileGuid().has_value());

            auto transferredSourcePane = page->_MakePane(nullptr, nullptr, nullptr);
            VERIFY_IS_NOT_NULL(transferredSourcePane);
            const auto transferredControl = transferredSourcePane->GetTerminalControl();
            VERIFY_IS_NOT_NULL(transferredControl);
            const auto contentId = transferredControl.ContentId();
            page->_manager.Detach(transferredControl);

            winrt::TerminalApp::implementation::AgentPaneDragStash::Stash(
                contentId,
                oldTabId,
                sourceProfileGuid,
                winrt::TerminalApp::implementation::AgentPaneDragStash::AttachDisposition::FirstPaneOfNewTab);

            NewTerminalArgs newTerminalArgs{};
            newTerminalArgs.ContentId(contentId);
            auto transferredPane = page->_MakeTerminalPane(newTerminalArgs, nullptr, nullptr);
            VERIFY_IS_NOT_NULL(transferredPane);
            VERIFY_IS_TRUE(transferredPane->IsAgentPane());

            VERIFY_IS_TRUE(focusedTab->FindAgentPane() != nullptr);
            VERIFY_IS_FALSE(focusedTab->AgentSourceProfileGuid().has_value());

            const auto agentContent = transferredPane->GetContent().try_as<winrt::TerminalApp::AgentPaneContent>();
            VERIFY_IS_NOT_NULL(agentContent);
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(agentContent);
            VERIFY_IS_TRUE(impl->TakePendingRenameFromTabId() == oldTabId);
            VERIFY_IS_TRUE(impl->TransferSourceTabId() == oldTabId);
            const auto pendingSourceProfileGuid = impl->TakePendingAgentSourceProfileGuid();
            VERIFY_IS_TRUE(pendingSourceProfileGuid.has_value());
            VERIFY_IS_TRUE(pendingSourceProfileGuid.value() == sourceProfileGuid);
        });
    }

    void TabTests::_verifyBuildStartupActionsContentStashesAgentPane(const SplitDirection splitDirection,
                                                                     const winrt::TerminalApp::implementation::AgentPaneDragStash::AttachDisposition expectedDisposition,
                                                                     const winrt::guid& sourceProfileGuid)
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto focusedTab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(focusedTab);

            const auto oldTabId = focusedTab->StableId();
            focusedTab->AgentSourceProfileGuid(sourceProfileGuid);

            auto agentPane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            VERIFY_IS_NOT_NULL(agentPane);
            agentPane->IsAgentPane(true);
            page->_SplitPane(focusedTab, splitDirection, 0.5f, agentPane);

            const auto agentContentArgs = agentPane->GetContent().GetNewTerminalArgs(BuildStartupKind::Content).try_as<NewTerminalArgs>();
            VERIFY_IS_NOT_NULL(agentContentArgs);
            const auto agentContentId = agentContentArgs.ContentId();
            VERIFY_ARE_NOT_EQUAL(0ull, agentContentId);

            const auto actions = focusedTab->BuildStartupActions(BuildStartupKind::Content);
            VERIFY_IS_TRUE(actions.size() >= 2);
            VERIFY_ARE_EQUAL(ShortcutAction::NewTab, actions.at(0).Action());

            const auto newTabArgs = actions.at(0).Args().try_as<NewTabArgs>();
            VERIFY_IS_NOT_NULL(newTabArgs);
            const auto firstPaneArgs = newTabArgs.ContentArgs().try_as<NewTerminalArgs>();
            VERIFY_IS_NOT_NULL(firstPaneArgs);

            if (expectedDisposition == winrt::TerminalApp::implementation::AgentPaneDragStash::AttachDisposition::FirstPaneOfNewTab)
            {
                VERIFY_ARE_EQUAL(agentContentId, firstPaneArgs.ContentId());
            }
            else
            {
                VERIFY_ARE_NOT_EQUAL(agentContentId, firstPaneArgs.ContentId());
                VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actions.at(1).Action());

                const auto splitPaneArgs = actions.at(1).Args().try_as<SplitPaneArgs>();
                VERIFY_IS_NOT_NULL(splitPaneArgs);
                const auto splitContentArgs = splitPaneArgs.ContentArgs().try_as<NewTerminalArgs>();
                VERIFY_IS_NOT_NULL(splitContentArgs);
                VERIFY_ARE_EQUAL(agentContentId, splitContentArgs.ContentId());
            }

            winrt::hstring stashedTabId;
            std::optional<winrt::guid> stashedSourceProfileGuid;
            auto actualDisposition = winrt::TerminalApp::implementation::AgentPaneDragStash::AttachDisposition::ExistingTabSplit;
            VERIFY_IS_TRUE(winrt::TerminalApp::implementation::AgentPaneDragStash::Take(
                agentContentId,
                stashedTabId,
                stashedSourceProfileGuid,
                actualDisposition));
            VERIFY_IS_TRUE(stashedTabId == oldTabId);
            VERIFY_IS_TRUE(stashedSourceProfileGuid.has_value());
            VERIFY_IS_TRUE(stashedSourceProfileGuid.value() == sourceProfileGuid);
            VERIFY_IS_TRUE(actualDisposition == expectedDisposition);
        });
    }

    void TabTests::BuildStartupActionsContentStashesAgentFirstPaneAsNewTab()
    {
        _verifyBuildStartupActionsContentStashesAgentPane(
            SplitDirection::Left,
            winrt::TerminalApp::implementation::AgentPaneDragStash::AttachDisposition::FirstPaneOfNewTab,
            winrt::guid{ L"{6239a42c-7777-49a3-80bd-e8fdd045185c}" });
    }

    void TabTests::BuildStartupActionsContentStashesAgentLaterSplitAsExistingTab()
    {
        _verifyBuildStartupActionsContentStashesAgentPane(
            SplitDirection::Right,
            winrt::TerminalApp::implementation::AgentPaneDragStash::AttachDisposition::ExistingTabSplit,
            winrt::guid{ L"{6239a42c-8888-49a3-80bd-e8fdd045185c}" });
    }

    void TabTests::TransferredAgentContentSplitPaneRetiresDestinationAgentPane()
    {
        auto page = _commonSetup();
        const auto sourceProfileGuid = winrt::guid{ L"{6239a42c-6666-49a3-80bd-e8fdd045185c}" };
        const auto oldTabId = winrt::hstring{ L"source-tab-id" };

        TestOnUIThread([&]() {
            const auto focusedTab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(focusedTab);

            auto destinationAgentPane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            VERIFY_IS_NOT_NULL(destinationAgentPane);
            destinationAgentPane->IsAgentPane(true);
            page->_SplitPane(focusedTab, SplitDirection::Left, 0.5f, destinationAgentPane);
            VERIFY_IS_TRUE(focusedTab->FindAgentPane() != nullptr);
            VERIFY_IS_FALSE(focusedTab->AgentSourceProfileGuid().has_value());

            auto transferredSourcePane = page->_MakePane(nullptr, nullptr, nullptr);
            VERIFY_IS_NOT_NULL(transferredSourcePane);
            const auto transferredControl = transferredSourcePane->GetTerminalControl();
            VERIFY_IS_NOT_NULL(transferredControl);
            const auto contentId = transferredControl.ContentId();
            page->_manager.Detach(transferredControl);

            winrt::TerminalApp::implementation::AgentPaneDragStash::Stash(
                contentId,
                oldTabId,
                sourceProfileGuid,
                winrt::TerminalApp::implementation::AgentPaneDragStash::AttachDisposition::ExistingTabSplit);

            NewTerminalArgs newTerminalArgs{};
            newTerminalArgs.ContentId(contentId);
            auto transferredPane = page->_MakeTerminalPane(newTerminalArgs, nullptr, nullptr);
            VERIFY_IS_NOT_NULL(transferredPane);
            VERIFY_IS_TRUE(transferredPane->IsAgentPane());

            VERIFY_IS_TRUE(focusedTab->FindAgentPane() == nullptr);
            VERIFY_IS_TRUE(focusedTab->AgentSourceProfileGuid().has_value());
            VERIFY_IS_TRUE(focusedTab->AgentSourceProfileGuid().value() == sourceProfileGuid);

            const auto agentContent = transferredPane->GetContent().try_as<winrt::TerminalApp::AgentPaneContent>();
            VERIFY_IS_NOT_NULL(agentContent);
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(agentContent);
            VERIFY_IS_TRUE(impl->TakePendingRenameFromTabId().empty());
            VERIFY_IS_TRUE(impl->TransferSourceTabId() == oldTabId);
            VERIFY_IS_FALSE(impl->TakePendingAgentSourceProfileGuid().has_value());
        });
    }

    void TabTests::TransferredAgentStatusReplaysMissedTabRekey()
    {
        auto page = _commonSetup();
        const auto oldTabId = winrt::hstring{ L"source-tab-id" };

        TestOnUIThread([&]() {
            const auto focusedTab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(focusedTab);

            auto agentPane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            VERIFY_IS_NOT_NULL(agentPane);
            agentPane->IsAgentPane(true);
            page->_SplitPane(focusedTab, SplitDirection::Left, 0.5f, agentPane);

            const auto agentContent = focusedTab->FindAgentPaneContent();
            VERIFY_IS_NOT_NULL(agentContent);
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(agentContent);
            impl->SetTransferSourceTabId(oldTabId);

            std::vector<Json::Value> protocolEvents;
            const auto token = page->ProtocolVtSequenceReceived(
                [&](auto&&, const winrt::hstring& payload) {
                    Json::Value event;
                    Json::CharReaderBuilder readerBuilder;
                    std::istringstream stream{ winrt::to_string(payload) };
                    std::string errors;
                    if (Json::parseFromStream(readerBuilder, stream, &event, &errors))
                    {
                        protocolEvents.emplace_back(std::move(event));
                    }
                });

            const auto sendStatus = [&](const winrt::hstring& tabId, const char* model) {
                Json::Value event{ Json::objectValue };
                event["type"] = "event";
                event["method"] = "agent_status";
                event["params"]["agent_id"] = "copilot";
                event["params"]["name"] = "Copilot";
                event["params"]["version"] = "v1";
                event["params"]["model"] = model;
                event["params"]["state"] = "connected";
                event["params"]["backend"] = "Windows";
                event["params"]["host_catalog_ready"] = true;
                event["params"]["tab_id"] = winrt::to_string(tabId);

                Json::StreamWriterBuilder writerBuilder;
                writerBuilder["indentation"] = "";
                page->OnAgentStatusChanged(winrt::to_hstring(Json::writeString(writerBuilder, event)));
            };

            sendStatus(oldTabId, "model-a");

            VERIFY_IS_TRUE(impl->IsHelperEventReady());
            VERIFY_IS_TRUE(impl->IsAgentConnected());
            VERIFY_IS_TRUE(impl->GetAgentName() == L"Copilot");
            VERIFY_IS_TRUE(impl->GetAgentModel() == L"model-a");
            VERIFY_ARE_EQUAL(1u, protocolEvents.size());
            VERIFY_IS_TRUE(protocolEvents[0]["method"].asString() == "tab_renamed");
            VERIFY_IS_TRUE(protocolEvents[0]["params"]["old_tab_id"].asString() == winrt::to_string(oldTabId));
            VERIFY_IS_TRUE(protocolEvents[0]["params"]["new_tab_id"].asString() == winrt::to_string(focusedTab->StableId()));
            VERIFY_IS_TRUE(impl->TransferSourceTabId().empty());

            sendStatus(focusedTab->StableId(), "model-b");

            VERIFY_IS_TRUE(impl->GetAgentModel() == L"model-b");
            VERIFY_ARE_EQUAL(1u, protocolEvents.size());

            page->ProtocolVtSequenceReceived(token);
        });
    }

    void TabTests::NextMRUTab()
    {
        // This is a test for GH#8025 - we want to make sure that MRU tab
        // ordering works correctly and that in-order/disabled switching works.
        //
        // Note: We test MRU ordering directly rather than going through the
        // command palette tab switcher, because the palette's anchor key
        // handling auto-dismisses when no modifier keys are held (which we
        // can't simulate in the test environment).

        auto page = _commonSetup();

        Log::Comment(L"Create Tab[1]");
        TestOnUIThread([&page]() {
            NewTerminalArgs newTerminalArgs{ 1 };
            page->_OpenNewTab(newTerminalArgs);
        });
        VERIFY_ARE_EQUAL(2u, page->_tabs.Size());

        Log::Comment(L"Create Tab[2]");
        TestOnUIThread([&page]() {
            NewTerminalArgs newTerminalArgs{ 2 };
            page->_OpenNewTab(newTerminalArgs);
        });
        VERIFY_ARE_EQUAL(3u, page->_tabs.Size());

        Log::Comment(L"Create Tab[3]");
        TestOnUIThread([&page]() {
            NewTerminalArgs newTerminalArgs{ 3 };
            page->_OpenNewTab(newTerminalArgs);
        });
        VERIFY_ARE_EQUAL(4u, page->_tabs.Size());

        TestOnUIThread([&page]() {
            auto focusedIndex = page->_GetFocusedTabIndex().value_or(-1);
            VERIFY_ARE_EQUAL(3u, focusedIndex, L"Verify Tab[3] is focused");
        });

        Log::Comment(L"Select Tab[1]");
        TestOnUIThread([&page]() {
            page->_SelectTab(1);
        });

        TestOnUIThread([&page]() {
            auto focusedIndex = page->_GetFocusedTabIndex().value_or(-1);
            VERIFY_ARE_EQUAL(1u, focusedIndex, L"Verify Tab[1] is focused");
        });

        // MRU order should now be: Tab[1], Tab[3], Tab[2], Tab[0]
        // Verify the MRU list directly.
        Log::Comment(L"Verify MRU order: MRU[0]=Tab[1], MRU[1]=Tab[3]");
        TestOnUIThread([&page]() {
            VERIFY_ARE_EQUAL(4u, page->_mruTabs.Size());
            uint32_t mruIdx;
            page->_tabs.IndexOf(page->_mruTabs.GetAt(0), mruIdx);
            VERIFY_ARE_EQUAL(1u, mruIdx, L"MRU[0] should be Tab[1] (most recent)");
            page->_tabs.IndexOf(page->_mruTabs.GetAt(1), mruIdx);
            VERIFY_ARE_EQUAL(3u, mruIdx, L"MRU[1] should be Tab[3] (last tab added)");
        });

        Log::Comment(L"Select MRU[1]=Tab[3] directly");
        TestOnUIThread([&page]() {
            // The next MRU tab after Tab[1] is Tab[3]
            uint32_t nextMruIdx;
            page->_tabs.IndexOf(page->_mruTabs.GetAt(1), nextMruIdx);
            page->_SelectTab(nextMruIdx);
        });

        TestOnUIThread([&page]() {
            auto focusedIndex = page->_GetFocusedTabIndex().value_or(-1);
            VERIFY_ARE_EQUAL(3u, focusedIndex, L"Verify Tab[3] is focused");
        });

        Log::Comment(L"Select MRU[1]=Tab[1] directly");
        TestOnUIThread([&page]() {
            uint32_t nextMruIdx;
            page->_tabs.IndexOf(page->_mruTabs.GetAt(1), nextMruIdx);
            page->_SelectTab(nextMruIdx);
        });

        TestOnUIThread([&page]() {
            auto focusedIndex = page->_GetFocusedTabIndex().value_or(-1);
            VERIFY_ARE_EQUAL(1u, focusedIndex, L"Verify Tab[1] is focused");
        });

        // The Disabled tab switcher mode uses direct index-based switching
        // without the command palette, so it works in the test environment.
        Log::Comment(L"Change the tab switch order to not use the tab switcher (which is in-order always)");
        page->_settings.GlobalSettings().TabSwitcherMode(TabSwitcherMode::Disabled);

        Log::Comment(L"Switch to the next in-order tab: Tab[2]");
        TestOnUIThread([&page]() {
            page->_SelectNextTab(true, nullptr);
        });
        TestOnUIThread([&page]() {
            auto focusedIndex = page->_GetFocusedTabIndex().value_or(-1);
            VERIFY_ARE_EQUAL(2u, focusedIndex, L"Verify Tab[2] is focused");
        });

        Log::Comment(L"Switch to the next in-order tab: Tab[3]");
        TestOnUIThread([&page]() {
            page->_SelectNextTab(true, nullptr);
        });
        TestOnUIThread([&page]() {
            auto focusedIndex = page->_GetFocusedTabIndex().value_or(-1);
            VERIFY_ARE_EQUAL(3u, focusedIndex, L"Verify Tab[3] is focused");
        });
    }

    void TabTests::VerifyCommandPaletteTabSwitcherOrder()
    {
        // This is a test for GH#8188 - we want to make sure that the MRU
        // ordering is correctly maintained as tabs are selected.
        //
        // Note: We verify MRU ordering directly rather than going through
        // the command palette tab switcher, because the palette's anchor key
        // handling auto-dismisses when no modifier keys are held (which we
        // can't simulate in the test environment).

        auto page = _commonSetup();

        Log::Comment(L"Create 3 additional tabs");
        RunOnUIThread([&page]() {
            NewTerminalArgs newTerminalArgs{ 1 };
            page->_OpenNewTab(newTerminalArgs);
            page->_OpenNewTab(newTerminalArgs);
            page->_OpenNewTab(newTerminalArgs);
        });
        VERIFY_ARE_EQUAL(4u, page->_mruTabs.Size());

        Log::Comment(L"give alphabetical names to all tabs");
        TestOnUIThread([&page]() {
            page->_GetTabImpl(page->_tabs.GetAt(0))->Title(L"a");
        });
        TestOnUIThread([&page]() {
            page->_GetTabImpl(page->_tabs.GetAt(1))->Title(L"b");
        });
        TestOnUIThread([&page]() {
            page->_GetTabImpl(page->_tabs.GetAt(2))->Title(L"c");
        });
        TestOnUIThread([&page]() {
            page->_GetTabImpl(page->_tabs.GetAt(3))->Title(L"d");
        });

        TestOnUIThread([&page]() {
            Log::Comment(L"Sanity check the titles of our tabs are what we set them to.");

            VERIFY_ARE_EQUAL(L"a", page->_tabs.GetAt(0).Title());
            VERIFY_ARE_EQUAL(L"b", page->_tabs.GetAt(1).Title());
            VERIFY_ARE_EQUAL(L"c", page->_tabs.GetAt(2).Title());
            VERIFY_ARE_EQUAL(L"d", page->_tabs.GetAt(3).Title());

            // MRU order after creating Tab[0]-Tab[3]: MRU[0]=Tab[3], MRU[3]=Tab[0]
            VERIFY_ARE_EQUAL(L"d", page->_mruTabs.GetAt(0).Title());
            VERIFY_ARE_EQUAL(L"c", page->_mruTabs.GetAt(1).Title());
            VERIFY_ARE_EQUAL(L"b", page->_mruTabs.GetAt(2).Title());
            VERIFY_ARE_EQUAL(L"a", page->_mruTabs.GetAt(3).Title());
        });

        Log::Comment(L"Select Tab[0] through Tab[3] to establish MRU order");
        RunOnUIThread([&page]() {
            page->_UpdatedSelectedTab(page->_tabs.GetAt(0));
            page->_UpdatedSelectedTab(page->_tabs.GetAt(1));
            page->_UpdatedSelectedTab(page->_tabs.GetAt(2));
            page->_UpdatedSelectedTab(page->_tabs.GetAt(3));
        });

        Log::Comment(L"Verify MRU order: MRU[0]='d', MRU[1]='c', MRU[2]='b', MRU[3]='a'");
        VERIFY_ARE_EQUAL(4u, page->_mruTabs.Size());
        VERIFY_ARE_EQUAL(L"d", page->_mruTabs.GetAt(0).Title());
        VERIFY_ARE_EQUAL(L"c", page->_mruTabs.GetAt(1).Title());
        VERIFY_ARE_EQUAL(L"b", page->_mruTabs.GetAt(2).Title());
        VERIFY_ARE_EQUAL(L"a", page->_mruTabs.GetAt(3).Title());

        Log::Comment(L"Select Tab[2]='c' (MRU[1] after 'd')");
        TestOnUIThread([&page]() {
            page->_SelectTab(2);
        });

        Log::Comment(L"Verify MRU order updated: MRU[0]='c', MRU[1]='d', MRU[2]='b', MRU[3]='a'");
        TestOnUIThread([&page]() {
            VERIFY_ARE_EQUAL(L"c", page->_mruTabs.GetAt(0).Title());
            VERIFY_ARE_EQUAL(L"d", page->_mruTabs.GetAt(1).Title());
            VERIFY_ARE_EQUAL(L"b", page->_mruTabs.GetAt(2).Title());
            VERIFY_ARE_EQUAL(L"a", page->_mruTabs.GetAt(3).Title());
        });
    }

    void TabTests::TestWindowRenameSuccessful()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        auto page = _commonSetup();
        page->RenameWindowRequested([&page, this](auto&&, const winrt::TerminalApp::RenameWindowRequestedArgs args) {
            // In the real terminal, this would bounce up to the monarch and
            // come back down. Instead, immediately call back and set the name.
            //
            // This replicates how TerminalWindow works
            _windowProperties->WindowName(args.ProposedName());
        });

        auto windowNameChanged = false;
        _windowProperties->PropertyChanged([&page, &windowNameChanged](auto&&, const winrt::WUX::Data::PropertyChangedEventArgs& args) mutable {
            if (args.PropertyName() == L"WindowNameForDisplay")
            {
                windowNameChanged = true;
            }
        });

        TestOnUIThread([&page]() {
            page->_RequestWindowRename(winrt::hstring{ L"Foo" });
        });
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(L"Foo", page->WindowProperties().WindowName());
            VERIFY_IS_TRUE(windowNameChanged,
                           L"The window name should have changed, and we should have raised a notification that WindowNameForDisplay changed");
        });
    }
    void TabTests::TestWindowRenameFailure()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        auto page = _commonSetup();
        auto windowNameChanged = false;

        page->PropertyChanged([&page, &windowNameChanged](auto&&, const winrt::WUX::Data::PropertyChangedEventArgs& args) mutable {
            if (args.PropertyName() == L"WindowNameForDisplay")
            {
                windowNameChanged = true;
            }
        });

        TestOnUIThread([&page]() {
            page->_RequestWindowRename(winrt::hstring{ L"Foo" });
        });
        TestOnUIThread([&]() {
            VERIFY_IS_FALSE(windowNameChanged,
                            L"The window name should not have changed, we should have rejected the change.");
        });
    }

    static til::color _getControlBackgroundColor(winrt::TerminalApp::implementation::ContentManager* contentManager,
                                                 const winrt::Microsoft::Terminal::Control::TermControl& c)
    {
        auto interactivity{ contentManager->TryLookupCore(c.ContentId()) };
        VERIFY_IS_NOT_NULL(interactivity);
        const auto core{ interactivity.Core() };
        return til::color{ core.BackgroundColor() };
    }

    void TabTests::TestPreviewCommitScheme()
    {
        Log::Comment(L"Preview a color scheme. Make sure it's applied, then committed accordingly");

        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        TestOnUIThread([&page, this]() {
            const auto& activeControl{ page->_GetActiveControl() };
            VERIFY_IS_NOT_NULL(activeControl);

            const auto backgroundColor{ _getControlBackgroundColor(_contentManager.get(), activeControl) };
            VERIFY_ARE_EQUAL(til::color{ 0xff0c0c0c }, backgroundColor);
        });

        TestOnUIThread([&page]() {
            Log::Comment(L"Emulate previewing the SetColorScheme action");
            SetColorSchemeArgs args{ L"Vintage" };
            ActionAndArgs actionAndArgs{ ShortcutAction::SetColorScheme, args };
            page->_PreviewAction(actionAndArgs);
        });

        TestOnUIThread([&page, this]() {
            const auto& activeControl{ page->_GetActiveControl() };
            VERIFY_IS_NOT_NULL(activeControl);

            Log::Comment(L"Color should be changed to the preview");
            const auto backgroundColor{ _getControlBackgroundColor(_contentManager.get(), activeControl) };
            VERIFY_ARE_EQUAL(til::color{ 0xff000000 }, backgroundColor);

            // And we should have stored a function to revert the change.
            VERIFY_ARE_EQUAL(1u, page->_restorePreviewFuncs.size());
        });

        TestOnUIThread([&page]() {
            Log::Comment(L"Emulate committing the SetColorScheme action");

            SetColorSchemeArgs args{ L"Vintage" };
            page->_EndPreview();
            page->_HandleSetColorScheme(nullptr, ActionEventArgs{ args });
        });

        TestOnUIThread([&page, this]() {
            const auto& activeControl{ page->_GetActiveControl() };
            VERIFY_IS_NOT_NULL(activeControl);

            Log::Comment(L"Color should be changed");
            const auto backgroundColor{ _getControlBackgroundColor(_contentManager.get(), activeControl) };
            VERIFY_ARE_EQUAL(til::color{ 0xff000000 }, backgroundColor);

            // After preview there should be no more restore functions to execute.
            VERIFY_ARE_EQUAL(0u, page->_restorePreviewFuncs.size());
        });

        Log::Comment(L"Sleep to let events propagate");
        // If you don't do this, we will _sometimes_ crash as we're tearing down
        // the control from this test as we start the next one. We crash
        // somewhere in the CursorPositionChanged handler. It's annoying, but
        // this works.
        Sleep(250);
    }

    void TabTests::TestPreviewDismissScheme()
    {
        Log::Comment(L"Preview a color scheme. Make sure it's applied, then dismissed accordingly");

        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        TestOnUIThread([&page, this]() {
            const auto& activeControl{ page->_GetActiveControl() };
            VERIFY_IS_NOT_NULL(activeControl);

            const auto backgroundColor{ _getControlBackgroundColor(_contentManager.get(), activeControl) };
            VERIFY_ARE_EQUAL(til::color{ 0xff0c0c0c }, backgroundColor);
        });

        TestOnUIThread([&page]() {
            Log::Comment(L"Emulate previewing the SetColorScheme action");
            SetColorSchemeArgs args{ L"Vintage" };
            ActionAndArgs actionAndArgs{ ShortcutAction::SetColorScheme, args };
            page->_PreviewAction(actionAndArgs);
        });

        TestOnUIThread([&page, this]() {
            const auto& activeControl{ page->_GetActiveControl() };
            VERIFY_IS_NOT_NULL(activeControl);

            Log::Comment(L"Color should be changed to the preview");
            const auto backgroundColor{ _getControlBackgroundColor(_contentManager.get(), activeControl) };
            VERIFY_ARE_EQUAL(til::color{ 0xff000000 }, backgroundColor);
        });

        TestOnUIThread([&page]() {
            Log::Comment(L"Emulate dismissing the SetColorScheme action");
            page->_EndPreview();
        });

        TestOnUIThread([&page, this]() {
            const auto& activeControl{ page->_GetActiveControl() };
            VERIFY_IS_NOT_NULL(activeControl);

            Log::Comment(L"Color should be the same as it originally was");
            const auto backgroundColor{ _getControlBackgroundColor(_contentManager.get(), activeControl) };
            VERIFY_ARE_EQUAL(til::color{ 0xff0c0c0c }, backgroundColor);
        });
        Log::Comment(L"Sleep to let events propagate");
        Sleep(250);
    }

    void TabTests::TestPreviewSchemeWhilePreviewing()
    {
        Log::Comment(L"Preview a color scheme, then preview another scheme. ");

        Log::Comment(L"Preview a color scheme. Make sure it's applied, then committed accordingly");

        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        TestOnUIThread([&page, this]() {
            const auto& activeControl{ page->_GetActiveControl() };
            VERIFY_IS_NOT_NULL(activeControl);

            const auto backgroundColor{ _getControlBackgroundColor(_contentManager.get(), activeControl) };
            VERIFY_ARE_EQUAL(til::color{ 0xff0c0c0c }, backgroundColor);
        });

        TestOnUIThread([&page]() {
            Log::Comment(L"Emulate previewing the SetColorScheme action");
            SetColorSchemeArgs args{ L"Vintage" };
            page->_PreviewColorScheme(args);
        });

        TestOnUIThread([&page, this]() {
            const auto& activeControl{ page->_GetActiveControl() };
            VERIFY_IS_NOT_NULL(activeControl);

            Log::Comment(L"Color should be changed to the preview");
            const auto backgroundColor{ _getControlBackgroundColor(_contentManager.get(), activeControl) };
            VERIFY_ARE_EQUAL(til::color{ 0xff000000 }, backgroundColor);
        });

        TestOnUIThread([&page]() {
            Log::Comment(L"Now, preview another scheme");
            SetColorSchemeArgs args{ L"One Half Light" };
            page->_PreviewColorScheme(args);
        });

        TestOnUIThread([&page, this]() {
            const auto& activeControl{ page->_GetActiveControl() };
            VERIFY_IS_NOT_NULL(activeControl);

            Log::Comment(L"Color should be changed to the preview");
            const auto backgroundColor{ _getControlBackgroundColor(_contentManager.get(), activeControl) };
            VERIFY_ARE_EQUAL(til::color{ 0xffFAFAFA }, backgroundColor);
        });

        TestOnUIThread([&page]() {
            Log::Comment(L"Emulate committing the SetColorScheme action");

            SetColorSchemeArgs args{ L"One Half Light" };
            page->_EndPreview();
            page->_HandleSetColorScheme(nullptr, ActionEventArgs{ args });
        });

        TestOnUIThread([&page, this]() {
            const auto& activeControl{ page->_GetActiveControl() };
            VERIFY_IS_NOT_NULL(activeControl);

            Log::Comment(L"Color should be changed");
            const auto backgroundColor{ _getControlBackgroundColor(_contentManager.get(), activeControl) };
            VERIFY_ARE_EQUAL(til::color{ 0xffFAFAFA }, backgroundColor);
        });
        Log::Comment(L"Sleep to let events propagate");
        Sleep(250);
    }

    void TabTests::TestClampSwitchToTab()
    {
        Log::Comment(L"Test that switching to a tab index higher than the number of tabs just clamps to the last tab.");

        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        Log::Comment(L"Create a second tab");
        TestOnUIThread([&page]() {
            NewTerminalArgs newTerminalArgs{ 1 };
            page->_OpenNewTab(newTerminalArgs);
        });
        VERIFY_ARE_EQUAL(2u, page->_tabs.Size());

        Log::Comment(L"Create a third tab");
        TestOnUIThread([&page]() {
            NewTerminalArgs newTerminalArgs{ 2 };
            page->_OpenNewTab(newTerminalArgs);
        });
        VERIFY_ARE_EQUAL(3u, page->_tabs.Size());

        TestOnUIThread([&page]() {
            auto focusedTabIndexOpt{ page->_GetFocusedTabIndex() };
            VERIFY_IS_TRUE(focusedTabIndexOpt.has_value());
            VERIFY_ARE_EQUAL(2u, focusedTabIndexOpt.value());
        });

        TestOnUIThread([&page]() {
            Log::Comment(L"Switch to the first tab");
            page->_SelectTab(0);
        });

        TestOnUIThread([&page]() {
            auto focusedTabIndexOpt{ page->_GetFocusedTabIndex() };

            VERIFY_IS_TRUE(focusedTabIndexOpt.has_value());
            VERIFY_ARE_EQUAL(0u, focusedTabIndexOpt.value());
        });

        TestOnUIThread([&page]() {
            Log::Comment(L"Switch to the tab 6, which is greater than number of tabs. This should switch to the third tab");
            page->_SelectTab(6);
        });

        TestOnUIThread([&page]() {
            auto focusedTabIndexOpt{ page->_GetFocusedTabIndex() };
            VERIFY_IS_TRUE(focusedTabIndexOpt.has_value());
            VERIFY_ARE_EQUAL(2u, focusedTabIndexOpt.value());
        });
    }

}
