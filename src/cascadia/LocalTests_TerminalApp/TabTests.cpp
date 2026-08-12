// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include "../TerminalApp/TerminalPage.h"
#include "../TerminalApp/TerminalWindow.h"
#include "../TerminalApp/MinMaxCloseControl.h"
#include "../TerminalApp/TabRowControl.h"
#include "../TerminalApp/ShortcutActionDispatch.h"
#include "../TerminalApp/Tab.h"
#include "../TerminalApp/CommandPalette.h"
#include "../TerminalApp/ContentManager.h"
#include "../TerminalApp/KeepRunningSessionHelpers.h"
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

    struct DetachedSessionEndedRecord
    {
        winrt::guid sessionId{};
        winrt::hstring state;
    };

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
        TEST_METHOD(ShellSessionCloseActionsFollowStartupPreference);
        TEST_METHOD(ShellSessionAgentBindingQualifiesForPersistence);
        TEST_METHOD(AgentSessionRestoreRequiresDurableRestoreContext);
        TEST_METHOD(PersistedLayoutAgentSessionsReceiveRestorePaths);
        TEST_METHOD(PersistedLayoutAdoptsStillLiveKeptPanes);
        TEST_METHOD(PaneAgentSessionBindingRequiresPaneIdentity);
        TEST_METHOD(AgentPaneRestoreDoesNotRequireAgentSession);
        TEST_METHOD(PaneAgentSessionEndClearsDurableBinding);
        TEST_METHOD(ReattachKeptSessionWhenKeepRunningIsDisabled);
        TEST_METHOD(ReattachKeptSessionUsesActualIdForAgentBinding);
        TEST_METHOD(ContentIdHandoffEndClearsAgentBinding);
        TEST_METHOD(WindowCloseAcceptanceIsOneShot);
        TEST_METHOD(ContentMapOperationsUseOwnerThread);
        TEST_METHOD(RepeatedKeepRunningDetachKeepsOneGroup);
        TEST_METHOD(ContentAttachRejectsDeadConnections);
        TEST_METHOD(ReattachEndEventOwnershipHandoff);
        TEST_METHOD(DetachedReapUsesConfiguredOwnerScheduler);
        TEST_METHOD(FailedDetachedReapFallsBackToPendingDrain);
        TEST_METHOD(OwnerOperationsDoNotRepeatPendingReapDrain);
        TEST_METHOD(ClosedContentFallbackWaitsForOwnerDrain);
        TEST_METHOD(DetachedSessionMetadataAndDiscard);
        TEST_METHOD(DetachedSessionAlreadyClosedIsReapedImmediately);
        TEST_METHOD(DetachShellPanesForKeepRunningStoresDurableMetadata);
        TEST_METHOD(DetachedSessionsSkipClosedConnectionBeforeQueuedReap);
        TEST_METHOD(BeginReattachKeptGroupPreservesPaneRestoreArgs);
        TEST_METHOD(BuildKeptGroupRestoreActionsPreservesRestoreArguments);
        TEST_METHOD(DurableSessionCloseWritesSaveResultsToTabAndPersistedActions);
        TEST_METHOD(GetWindowLayoutIncludesDurableMetadataForPersistedFullLayoutsOnly);
        TEST_METHOD(PersistStateForUnnamedWindowIncludesDurableMetadata);
        TEST_METHOD(RestoreAllKeptGroupsSnapshotsOrderAndReportsFallback);
        TEST_METHOD(DetachedFailedSessionQueuedReapEmitsFailedEventOnce);
        TEST_METHOD(DiscardDeadDetachedSessionBeforeQueuedReapReturnsFalse);
        TEST_METHOD(TryReattachDeadDetachedSessionBeforeQueuedReapReturnsZero);
        TEST_METHOD(BeginReattachKeptGroupSkipsDeadMembersBeforeQueuedReap);
        TEST_METHOD(BeginReattachKeptGroupReturnsNullWhenAllMembersDeadBeforeQueuedReap);
        TEST_METHOD(DiscardKeptGroupPreservesFailedMembersAndClosesLiveMembers);
        TEST_METHOD(NaturalClosedEventThenNotifyPanesClosingEmitsOnce);
        TEST_METHOD(NaturalFailedEventThenNotifyPanesClosingEmitsOnce);
        TEST_METHOD(ReusedSessionIdAcrossControlLifetimesEmitsEndStatePerLifetime);
        TEST_METHOD(SyntheticFailedEventSuppressesDelayedNormalCallback);
        TEST_METHOD(FailedDetachFallbackEmitsFailedEventOnce);
        TEST_METHOD(SuccessfulDetachedCloseDefersEndEventToContentManager);
        TEST_METHOD(CloseProtocolLastPaneKeepsRunningWithoutEmittingEndState);
        TEST_METHOD(CloseNonLastPaneEmitsOneEndStateWithoutKeepingTheTab);
        TEST_METHOD(FocusProtocolShellSessionUsesDurableId);
        TEST_METHOD(ParseShellSessionSaveResponse);

        TEST_METHOD(TryDuplicateBadTab);
        TEST_METHOD(TryDuplicateBadPane);

        TEST_METHOD(TryZoomPane);
        TEST_METHOD(MoveFocusFromZoomedPane);
        TEST_METHOD(CloseZoomedPane);

        TEST_METHOD(SwapPanes);

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
        void _initializeTerminalPage(winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage>& page,
                                     CascadiaSettings initialSettings);
        void _createContentManager(std::function<bool(DispatcherQueueHandler)> scheduleOnOwner = {});
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

    void TabTests::_createContentManager(std::function<bool(DispatcherQueueHandler)> scheduleOnOwner)
    {
        TestOnUIThread([&]() {
            const auto ownerDispatcher = DispatcherQueue::GetForCurrentThread();
            VERIFY_IS_NOT_NULL(ownerDispatcher);
            _contentManager = scheduleOnOwner ?
                                  winrt::make_self<winrt::TerminalApp::implementation::ContentManager>(ownerDispatcher, std::move(scheduleOnOwner)) :
                                  winrt::make_self<winrt::TerminalApp::implementation::ContentManager>(ownerDispatcher);
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

    void TabTests::ShellSessionCloseActionsFollowStartupPreference()
    {
        const auto tabId = ::Microsoft::Console::Utils::GuidFromString(L"{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}");
        const auto durableId = ::Microsoft::Console::Utils::GuidFromString(L"{11111111-2222-3333-4444-555555555555}");
        const winrt::hstring tabIdString{ ::Microsoft::Console::Utils::GuidToString(tabId) };
        VERIFY_IS_TRUE(!!::IsEqualGUID(
            durableId,
            winrt::TerminalApp::implementation::GetKeepRunningGroupId(
                tabIdString,
                L"11111111-2222-3333-4444-555555555555")));
        VERIFY_IS_TRUE(!!::IsEqualGUID(
            tabId,
            winrt::TerminalApp::implementation::GetKeepRunningGroupId(
                tabIdString,
                L"not-a-durable-uuid")));

        // `wtcli kill-detached-session` resolves its SHELL_SESSION_ID argument
        // with this, so it has to read the same spellings the group id is
        // derived from — the durable id reaches us as bare text from the
        // database, and as a braced GUID everywhere else.
        using winrt::TerminalApp::implementation::TryParseShellSessionId;
        VERIFY_IS_TRUE(!!::IsEqualGUID(
            durableId,
            TryParseShellSessionId(L"11111111-2222-3333-4444-555555555555").value()));
        VERIFY_IS_TRUE(!!::IsEqualGUID(
            durableId,
            TryParseShellSessionId(L"{11111111-2222-3333-4444-555555555555}").value()));
        VERIFY_IS_FALSE(TryParseShellSessionId(L"").has_value());
        VERIFY_IS_FALSE(TryParseShellSessionId(L"not-a-durable-uuid").has_value());

        const auto disabled = winrt::TerminalApp::implementation::GetShellSessionCloseActions(FirstWindowPreference::DefaultProfile, false);
        VERIFY_IS_FALSE(disabled.save);
        VERIFY_IS_FALSE(disabled.detach);
        VERIFY_IS_FALSE(disabled.persistScrollback);

        const auto layout = winrt::TerminalApp::implementation::GetShellSessionCloseActions(FirstWindowPreference::PersistedLayout, false);
        VERIFY_IS_TRUE(layout.save);
        VERIFY_IS_FALSE(layout.detach);
        VERIFY_IS_FALSE(layout.persistScrollback);

        const auto layoutAndContent = winrt::TerminalApp::implementation::GetShellSessionCloseActions(FirstWindowPreference::PersistedLayoutAndContent, true);
        VERIFY_IS_TRUE(layoutAndContent.save);
        VERIFY_IS_TRUE(layoutAndContent.detach);
        VERIFY_IS_TRUE(layoutAndContent.persistScrollback);

        const auto keepRunning = winrt::TerminalApp::implementation::GetShellSessionCloseActions(FirstWindowPreference::DefaultProfile, true);
        VERIFY_IS_FALSE(keepRunning.save);
        VERIFY_IS_TRUE(keepRunning.detach);
        VERIFY_IS_FALSE(keepRunning.persistScrollback);
    }

    void TabTests::ShellSessionAgentBindingQualifiesForPersistence()
    {
        using winrt::TerminalApp::implementation::ShouldPersistShellSession;

        VERIFY_IS_FALSE(ShouldPersistShellSession(false, false, false, false));
        VERIFY_IS_TRUE(ShouldPersistShellSession(true, false, false, false));
        VERIFY_IS_TRUE(ShouldPersistShellSession(false, true, false, false));
        VERIFY_IS_TRUE(ShouldPersistShellSession(false, false, true, false));
        VERIFY_IS_TRUE(ShouldPersistShellSession(false, false, false, true));
    }

    void TabTests::AgentSessionRestoreRequiresDurableRestoreContext()
    {
        using winrt::TerminalApp::implementation::ShouldResumeAgentSession;

        VERIFY_IS_FALSE(ShouldResumeAgentSession(false, true, false));
        VERIFY_IS_FALSE(ShouldResumeAgentSession(true, false, false));
        VERIFY_IS_TRUE(ShouldResumeAgentSession(true, true, false));
        VERIFY_IS_TRUE(ShouldResumeAgentSession(true, false, true));
    }

    void TabTests::PersistedLayoutAdoptsStillLiveKeptPanes()
    {
        using winrt::TerminalApp::implementation::ReattachKeptPanesInPersistedLayout;

        const auto keptSessionId = ::Microsoft::Console::Utils::CreateGuid();
        const auto ordinarySessionId = ::Microsoft::Console::Utils::CreateGuid();
        const auto replacementSessionId = ::Microsoft::Console::Utils::CreateGuid();

        NewTerminalArgs keptArgs{};
        keptArgs.SessionId(keptSessionId);
        keptArgs.ShellSessionRestorePath(L"buffer_kept.txt");

        NewTerminalArgs ordinaryArgs{};
        ordinaryArgs.SessionId(ordinarySessionId);

        std::vector<ActionAndArgs> actions;
        actions.emplace_back(ShortcutAction::NewTab, NewTabArgs{ keptArgs });
        actions.emplace_back(ShortcutAction::SplitPane, SplitPaneArgs{ SplitType::Manual, SplitDirection::Right, 0.5f, ordinaryArgs });

        ReattachKeptPanesInPersistedLayout(
            actions,
            [&](const winrt::guid& sessionId) { return !!::IsEqualGUID(sessionId, keptSessionId); },
            [&]() { return replacementSessionId; });

        // The still-live pane adopts its shell and takes a fresh identity.
        VERIFY_IS_TRUE(!!::IsEqualGUID(keptSessionId, keptArgs.KeptSessionId()));
        VERIFY_IS_TRUE(!!::IsEqualGUID(replacementSessionId, keptArgs.SessionId()));
        // The snapshot stays as the fallback for a session that dies before the
        // pane is built.
        VERIFY_ARE_EQUAL(winrt::hstring{ L"buffer_kept.txt" }, keptArgs.ShellSessionRestorePath());

        // A pane with no live session is left alone and starts normally.
        VERIFY_IS_TRUE(!!::IsEqualGUID(winrt::guid{}, ordinaryArgs.KeptSessionId()));
        VERIFY_IS_TRUE(!!::IsEqualGUID(ordinarySessionId, ordinaryArgs.SessionId()));
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
            firstArgs.ShellSessionRestorePath());
        VERIFY_IS_TRUE(secondArgs.AgentSessionId().empty());
        VERIFY_IS_TRUE(secondArgs.AgentSessionAgent().empty());
        VERIFY_IS_TRUE(secondArgs.AgentResumeCommandline().empty());
        VERIFY_IS_TRUE(secondArgs.ShellSessionRestorePath().empty());
    }

    void TabTests::PaneAgentSessionBindingRequiresPaneIdentity()
    {
        using winrt::TerminalApp::implementation::ShouldBindPaneAgentSession;
        using winrt::TerminalApp::implementation::ShouldUnbindPaneAgentSession;

        // wtcli stamps `pane_bound` from whether the event carried an explicit
        // `--pane`, so an inferred (focused-pane) origin never binds.
        VERIFY_IS_FALSE(ShouldBindPaneAgentSession(true, false));
        VERIFY_IS_TRUE(ShouldBindPaneAgentSession(true, true));
        VERIFY_IS_TRUE(ShouldBindPaneAgentSession(false, false));

        // Unbinding is gated the same way: an inferred origin must not clear a
        // binding it does not own.
        VERIFY_IS_FALSE(ShouldUnbindPaneAgentSession(false));
        VERIFY_IS_TRUE(ShouldUnbindPaneAgentSession(true));
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

    void TabTests::PaneAgentSessionEndClearsDurableBinding()
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

            const auto event = [&](const std::string_view name, const bool paneBound) {
                Json::Value evt;
                evt["params"]["pane_id"] = paneId;
                evt["params"]["event"] = std::string{ name };
                evt["params"]["agent_session_id"] = "agent-session-resumed";
                evt["params"]["agent"] = "copilot";
                evt["params"]["pane_bound"] = paneBound;
                Json::StreamWriterBuilder writer;
                writer["indentation"] = "";
                page->OnPaneAgentSessionChanged(winrt::to_hstring(Json::writeString(writer, evt)));
            };

            event("agent.session.start", true);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_paneAgentSessions.count(paneSessionId)));

            // An agent-pane CLI has no WT_SESSION, so its end event names the
            // focused pane instead of its own and must leave this binding alone.
            event("agent.session.end", false);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_paneAgentSessions.count(paneSessionId)));

            // The agent that ran in this pane exited, so there is nothing left
            // to resume and the pane restores as a plain shell.
            event("agent.session.end", true);
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_paneAgentSessions.count(paneSessionId)));
        });
    }

    void TabTests::ReattachKeptSessionWhenKeepRunningIsDisabled()
    {
        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        const auto groupId = ::Microsoft::Console::Utils::GuidFromString(L"{42a75f00-1111-2222-3333-444444444444}");
        const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{42a75f00-aaaa-bbbb-cccc-dddddddddddd}");

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            auto connection = winrt::make_self<TestConnection>(
                sessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(connection);

            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
            VERIFY_IS_NOT_NULL(content);

            winrt::Microsoft::Terminal::Control::TermControl control{ content };
            VERIFY_IS_NOT_NULL(control);
            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(groupId, sessionId, L"Reattach", L"", 0, NewTerminalArgs{}, control));

            page->_settings.GlobalSettings().FirstWindowPreference(FirstWindowPreference::DefaultProfile);

            NewTerminalArgs args{};
            args.SessionId(sessionId);
            VERIFY_SUCCEEDED(page->_OpenNewTab(args));
            const auto reattachedTab = page->_GetTabImpl(page->_tabs.GetAt(page->_tabs.Size() - 1));
            VERIFY_IS_NOT_NULL(reattachedTab);
            VERIFY_IS_TRUE(reattachedTab->KeepRunning());
            VERIFY_IS_TRUE(reattachedTab->TabStatus().IsKeepRunning());
            VERIFY_ARE_EQUAL(content.Id(), reattachedTab->GetActiveTerminalControl().ContentId());
            VERIFY_ARE_EQUAL(0ull, _contentManager->TryReattachKeptSession(sessionId));

            NewTerminalArgs fallbackArgs{};
            fallbackArgs.KeptSessionId(::Microsoft::Console::Utils::GuidFromString(L"{42a75f00-eeee-ffff-1111-222222222222}"));
            VERIFY_SUCCEEDED(page->_OpenNewTab(fallbackArgs));
            const auto fallbackTab = page->_GetTabImpl(page->_tabs.GetAt(page->_tabs.Size() - 1));
            VERIFY_IS_NOT_NULL(fallbackTab);
            VERIFY_IS_FALSE(fallbackTab->KeepRunning());
            VERIFY_IS_FALSE(fallbackTab->TabStatus().IsKeepRunning());
            VERIFY_IS_TRUE(fallbackArgs.KeptSessionId() == winrt::guid{});
        });
    }

    void TabTests::ReattachKeptSessionUsesActualIdForAgentBinding()
    {
        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        const auto groupId = ::Microsoft::Console::Utils::GuidFromString(L"{52a75f00-1111-2222-3333-444444444444}");
        const auto liveSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{52a75f00-aaaa-bbbb-cccc-dddddddddddd}");
        const auto fallbackSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{52a75f00-eeee-ffff-1111-222222222222}");

        winrt::com_ptr<TestConnection> connection;
        auto sessionBornBound = false;
        const auto token = page->ProtocolVtSequenceReceived([&](auto&&, const winrt::hstring& eventJson) {
            Json::Value evt;
            Json::CharReaderBuilder builder;
            std::string errors;
            std::istringstream stream{ winrt::to_string(eventJson) };
            if (Json::parseFromStream(builder, stream, &evt, &errors) &&
                evt["method"].asString() == "session_born_bound")
            {
                sessionBornBound = true;
            }
        });
        const auto revokeToken = wil::scope_exit([&]() noexcept {
            page->ProtocolVtSequenceReceived(token);
        });

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            connection = winrt::make_self<TestConnection>(
                liveSessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
            winrt::Microsoft::Terminal::Control::TermControl control{ content };
            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(groupId, liveSessionId, L"Reattach", L"", 0, NewTerminalArgs{}, control));

            NewTerminalArgs args{};
            args.SessionId(fallbackSessionId);
            args.KeptSessionId(liveSessionId);
            args.AgentSessionId(L"agent-session-live");
            args.AgentSessionAgent(L"copilot");
            args.AgentResumeCommandline(L"copilot --resume agent-session-live");

            const auto reattachedPane = page->_MakeTerminalPane(args);
            VERIFY_IS_NOT_NULL(reattachedPane);
            VERIFY_IS_TRUE(!!::IsEqualGUID(reattachedPane->GetTerminalControl().Connection().SessionId(), liveSessionId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_paneAgentSessions.count(liveSessionId)));
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_paneAgentSessions.count(fallbackSessionId)));
            VERIFY_IS_FALSE(sessionBornBound);

            NewTerminalArgs persistedArgs{};
            persistedArgs.SessionId(liveSessionId);
            std::vector<ActionAndArgs> actions;
            actions.emplace_back(ShortcutAction::NewTab, NewTabArgs{ persistedArgs });
            page->_AddDurableSessionMetadata(page->_GetFocusedTabImpl().get(), actions);

            const auto restoredArgs = _getTerminalArgs(actions.at(0));
            VERIFY_IS_NOT_NULL(restoredArgs);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"agent-session-live" }, restoredArgs.AgentSessionId());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"copilot" }, restoredArgs.AgentSessionAgent());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"copilot --resume agent-session-live" }, restoredArgs.AgentResumeCommandline());
        });

        TestOnUIThread([&]() {
            connection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);
        });

        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_paneAgentSessions.count(liveSessionId)));
        });
    }

    void TabTests::ContentIdHandoffEndClearsAgentBinding()
    {
        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        const auto groupId = ::Microsoft::Console::Utils::GuidFromString(L"{62a75f00-1111-2222-3333-444444444444}");
        const auto liveSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{62a75f00-aaaa-bbbb-cccc-dddddddddddd}");
        const auto fallbackSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{62a75f00-eeee-ffff-1111-222222222222}");

        std::vector<ConnectionStateEventRecord> connectionStates;
        const auto protocolToken = page->ProtocolVtSequenceReceived([&](auto&&, const winrt::hstring& eventJson) {
            _recordConnectionStateEvent(eventJson, connectionStates);
        });
        const auto protocolTokenRevoker = wil::scope_exit([&]() noexcept {
            page->ProtocolVtSequenceReceived(protocolToken);
        });

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            auto connection = winrt::make_self<TestConnection>(
                liveSessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
            winrt::Microsoft::Terminal::Control::TermControl detachedControl{ content };
            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(groupId, liveSessionId, L"Reattach", L"", 0, NewTerminalArgs{}, detachedControl));
            VERIFY_ARE_EQUAL(content.Id(), _contentManager->TryReattachKeptSession(liveSessionId));

            NewTerminalArgs args{};
            args.ContentId(content.Id());
            args.SessionId(fallbackSessionId);
            args.AgentSessionId(L"agent-session-ended");
            args.AgentSessionAgent(L"copilot");
            args.AgentResumeCommandline(L"copilot --resume agent-session-ended");

            auto closeDuringConfirm = true;
            const auto changedToken = _contentManager->KeptSessionsChanged([&](auto&&, auto&&) {
                if (closeDuringConfirm)
                {
                    closeDuringConfirm = false;
                    connection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);
                }
            });
            const auto reattachedPane = page->_MakeTerminalPane(args);
            _contentManager->KeptSessionsChanged(changedToken);

            VERIFY_IS_NOT_NULL(reattachedPane);
            VERIFY_IS_FALSE(closeDuringConfirm);
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_paneAgentSessions.count(liveSessionId)));
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_paneAgentSessions.count(fallbackSessionId)));

            const auto closedStates = _statesForPane(connectionStates, _formatPaneId(liveSessionId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(closedStates.size()));
            VERIFY_ARE_EQUAL(std::string{ "closed" }, closedStates.at(0));
        });
    }

    void TabTests::WindowCloseAcceptanceIsOneShot()
    {
        auto closeAccepted = false;
        auto persistCount = 0u;
        auto eventCount = 0u;

        for (auto request = 0; request < 2; ++request)
        {
            if (winrt::TerminalApp::implementation::TryAcceptWindowClose(closeAccepted))
            {
                ++persistCount;
                ++eventCount;
            }
        }

        VERIFY_ARE_EQUAL(1u, persistCount);
        VERIFY_ARE_EQUAL(1u, eventCount);
        VERIFY_IS_TRUE(closeAccepted);
    }

    void TabTests::ContentMapOperationsUseOwnerThread()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        _createContentManager();

        const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{61616161-6262-6363-6464-656565656565}");
        auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
        auto connection = winrt::make_self<TestConnection>(
            sessionId,
            winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);

        const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
        const auto contentId = content.Id();

        const auto lookup = _contentManager->TryLookupCore(contentId);
        VERIFY_IS_NOT_NULL(lookup);
        VERIFY_ARE_EQUAL(contentId, lookup.Id());

        content.Close();

        VERIFY_IS_NULL(_contentManager->TryLookupCore(contentId));
    }

    void TabTests::RepeatedKeepRunningDetachKeepsOneGroup()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        _createContentManager();

        const auto firstGroupId = ::Microsoft::Console::Utils::GuidFromString(L"{61616161-1111-2222-3333-444444444444}");
        const auto secondGroupId = ::Microsoft::Console::Utils::GuidFromString(L"{61616161-5555-6666-7777-888888888888}");
        const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{61616161-aaaa-bbbb-cccc-dddddddddddd}");
        const winrt::hstring shellSessionId{ L"61616161-9999-aaaa-bbbb-cccccccccccc" };

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            std::vector<winrt::Microsoft::Terminal::Control::ControlInteractivity> contents;
            const auto makeControl = [&]() {
                auto connection = winrt::make_self<TestConnection>(
                    sessionId,
                    winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
                auto content = _contentManager->CreateCore(*settings, *settings, *connection);
                contents.emplace_back(content);
                return winrt::Microsoft::Terminal::Control::TermControl{ content };
            };

            const auto firstControl = makeControl();
            const auto replacementControl = makeControl();
            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(firstGroupId, sessionId, L"Tab", shellSessionId, 1, NewTerminalArgs{}, firstControl));
            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(firstGroupId, sessionId, L"Renamed tab", shellSessionId, 2, NewTerminalArgs{}, replacementControl));
            VERIFY_ARE_EQUAL(1u, _contentManager->DetachedSessions().Size());
            VERIFY_ARE_EQUAL(1u, _contentManager->KeptGroups().Size());

            auto restored = _contentManager->BeginReattachKeptGroup(firstGroupId);
            VERIFY_IS_NOT_NULL(restored);
            VERIFY_ARE_EQUAL(1u, restored.RestoreArgs().Size());
            VERIFY_IS_TRUE(restored.Title() == L"Renamed tab");
            VERIFY_IS_TRUE(restored.ShellSessionId() == shellSessionId);
            VERIFY_ARE_EQUAL(2LL, restored.ShellSessionRevision());
            _contentManager->CancelKeptGroupReattach(firstGroupId);

            const auto movedControl = makeControl();
            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(secondGroupId, sessionId, L"Renamed tab", shellSessionId, 3, NewTerminalArgs{}, movedControl));
            const auto groups = _contentManager->KeptGroups();
            VERIFY_ARE_EQUAL(1u, groups.Size());
            VERIFY_IS_FALSE(groups.HasKey(firstGroupId));
            VERIFY_IS_TRUE(groups.HasKey(secondGroupId));

            _contentManager->DiscardKeptGroup(secondGroupId);
            contents.at(0).Close();
            contents.at(1).Close();
        });
    }

    void TabTests::ContentAttachRejectsDeadConnections()
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

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();

            const auto liveSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{60606060-6161-6262-6363-646464646464}");
            auto liveConnection = winrt::make_self<TestConnection>(
                liveSessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            const auto liveContent = _contentManager->CreateCore(*settings, *settings, *liveConnection);
            const auto liveControl = page->_AttachControlToContent(liveContent.Id());
            VERIFY_IS_NOT_NULL(liveControl);
            VERIFY_ARE_EQUAL(liveContent.Id(), liveControl.ContentId());

            const auto deadSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{70707070-7171-7272-7373-747474747474}");
            auto deadConnection = winrt::make_self<TestConnection>(
                deadSessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);
            const auto deadContent = _contentManager->CreateCore(*settings, *settings, *deadConnection);
            const auto deadContentId = deadContent.Id();
            VERIFY_IS_NULL(page->_AttachControlToContent(deadContentId));
            VERIFY_IS_NULL(_contentManager->TryLookupCore(deadContentId));
            const auto closedStates = _statesForPane(connectionStates, _formatPaneId(deadSessionId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(closedStates.size()));
            VERIFY_ARE_EQUAL(std::string{ "closed" }, closedStates.at(0));

            const auto failedSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{71717171-7272-7373-7474-757575757575}");
            auto failedConnection = winrt::make_self<TestConnection>(
                failedSessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Failed);
            const auto failedContent = _contentManager->CreateCore(*settings, *settings, *failedConnection);
            const auto failedContentId = failedContent.Id();
            VERIFY_IS_NULL(page->_AttachControlToContent(failedContentId));
            VERIFY_IS_NULL(_contentManager->TryLookupCore(failedContentId));
            const auto failedStates = _statesForPane(connectionStates, _formatPaneId(failedSessionId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(failedStates.size()));
            VERIFY_ARE_EQUAL(std::string{ "failed" }, failedStates.at(0));

            const auto fallbackSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{80808080-8181-8282-8383-848484848484}");
            auto fallbackConnection = winrt::make_self<TestConnection>(
                fallbackSessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            NewTerminalArgs args{};
            args.ContentId(deadContentId);
            const auto fallbackPane = page->_MakeTerminalPane(args, nullptr, *fallbackConnection);
            VERIFY_IS_NOT_NULL(fallbackPane);
            VERIFY_IS_TRUE(fallbackPane->GetTerminalControl().ContentId() != deadContentId);
            VERIFY_IS_TRUE(!!::IsEqualGUID(fallbackPane->GetTerminalControl().Connection().SessionId(), fallbackSessionId));
        });
    }

    void TabTests::ReattachEndEventOwnershipHandoff()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        const auto closeBeforeConfirmGroupId = ::Microsoft::Console::Utils::GuidFromString(L"{11111111-aaaa-bbbb-cccc-000000000001}");
        const auto closeBeforeConfirmSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{21111111-aaaa-bbbb-cccc-000000000001}");
        std::vector<DispatcherQueueHandler> queuedReaps;
        _createContentManager([&](DispatcherQueueHandler callback) {
            queuedReaps.emplace_back(std::move(callback));
            return true;
        });

        std::vector<DetachedSessionEndedRecord> closeBeforeConfirmEvents;
        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            auto connection = winrt::make_self<TestConnection>(
                closeBeforeConfirmSessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
            winrt::Microsoft::Terminal::Control::TermControl detachedControl{ content };

            const auto closeToken = _contentManager->DetachedSessionClosed([&](auto&&, const winrt::TerminalApp::DetachedSessionEndedArgs& endedSession) {
                closeBeforeConfirmEvents.push_back({ endedSession.SessionId(), endedSession.State() });
            });
            const auto closeTokenRevoker = wil::scope_exit([&]() noexcept {
                _contentManager->DetachedSessionClosed(closeToken);
            });

            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(
                closeBeforeConfirmGroupId,
                closeBeforeConfirmSessionId,
                L"Close before confirm",
                L"shell-session-close-before-confirm",
                1,
                NewTerminalArgs{},
                detachedControl));
            VERIFY_ARE_EQUAL(content.Id(), _contentManager->TryReattachKeptSession(closeBeforeConfirmSessionId));

            auto rawControl = winrt::Microsoft::Terminal::Control::TermControl::NewControlByAttachingContent(content);
            connection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(queuedReaps.size()));

            for (size_t i = 0; i < queuedReaps.size(); ++i)
            {
                const auto reap = queuedReaps.at(i);
                reap();
            }

            VERIFY_IS_FALSE(_contentManager->ConfirmReattachedContent(content.Id()));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(closeBeforeConfirmEvents.size()));
            VERIFY_IS_TRUE(!!::IsEqualGUID(closeBeforeConfirmEvents.at(0).sessionId, closeBeforeConfirmSessionId));
            VERIFY_IS_TRUE(closeBeforeConfirmEvents.at(0).state == L"closed");
            rawControl.Close();
        });

        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        std::vector<ConnectionStateEventRecord> connectionStates;
        const auto protocolToken = page->ProtocolVtSequenceReceived([&](auto&&, const winrt::hstring& eventJson) {
            _recordConnectionStateEvent(eventJson, connectionStates);
        });
        const auto protocolTokenRevoker = wil::scope_exit([&]() noexcept {
            page->ProtocolVtSequenceReceived(protocolToken);
        });

        std::vector<DetachedSessionEndedRecord> detachedEvents;
        const auto detachedToken = _contentManager->DetachedSessionClosed([&](auto&&, const winrt::TerminalApp::DetachedSessionEndedArgs& endedSession) {
            detachedEvents.push_back({ endedSession.SessionId(), endedSession.State() });
        });
        const auto detachedTokenRevoker = wil::scope_exit([&]() noexcept {
            _contentManager->DetachedSessionClosed(detachedToken);
        });

        const auto closeDuringConfirmGroupId = ::Microsoft::Console::Utils::GuidFromString(L"{11111111-aaaa-bbbb-cccc-000000000002}");
        const auto closeDuringConfirmSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{21111111-aaaa-bbbb-cccc-000000000002}");
        const auto closeAfterSetupGroupId = ::Microsoft::Console::Utils::GuidFromString(L"{11111111-aaaa-bbbb-cccc-000000000003}");
        const auto closeAfterSetupSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{21111111-aaaa-bbbb-cccc-000000000003}");
        const auto closeDuringConfirmPaneId = _formatPaneId(closeDuringConfirmSessionId);
        const auto closeAfterSetupPaneId = _formatPaneId(closeAfterSetupSessionId);

        winrt::com_ptr<TestConnection> closeDuringConfirmConnection;
        winrt::com_ptr<TestConnection> closeAfterSetupConnection;
        winrt::Microsoft::Terminal::Control::TermControl closeDuringConfirmControl{ nullptr };
        winrt::Microsoft::Terminal::Control::TermControl closeAfterSetupControl{ nullptr };

        TestOnUIThread([&]() {
            auto preparePendingReattach = [&](const winrt::guid& groupId,
                                              const winrt::guid& sessionId,
                                              winrt::com_ptr<TestConnection>& connection) {
                auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
                connection = winrt::make_self<TestConnection>(
                    sessionId,
                    winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
                const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
                winrt::Microsoft::Terminal::Control::TermControl detachedControl{ content };
                VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(
                    groupId,
                    sessionId,
                    L"Reattach ownership handoff",
                    L"shell-session-reattach-handoff",
                    2,
                    NewTerminalArgs{},
                    detachedControl));
                VERIFY_ARE_EQUAL(content.Id(), _contentManager->TryReattachKeptSession(sessionId));
                return content;
            };

            const auto closeDuringConfirmContent = preparePendingReattach(
                closeDuringConfirmGroupId,
                closeDuringConfirmSessionId,
                closeDuringConfirmConnection);

            auto closeDuringConfirm = true;
            const auto changedToken = _contentManager->KeptSessionsChanged([&](auto&&, auto&&) {
                if (closeDuringConfirm)
                {
                    closeDuringConfirm = false;
                    closeDuringConfirmConnection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);
                }
            });
            closeDuringConfirmControl = page->_AttachControlToContent(closeDuringConfirmContent.Id());
            _contentManager->KeptSessionsChanged(changedToken);

            VERIFY_IS_NOT_NULL(closeDuringConfirmControl);
            VERIFY_IS_FALSE(closeDuringConfirm);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(_statesForPane(connectionStates, closeDuringConfirmPaneId).size()));
            closeDuringConfirmConnection->RaiseStateChanged();

            const auto closeAfterSetupContent = preparePendingReattach(
                closeAfterSetupGroupId,
                closeAfterSetupSessionId,
                closeAfterSetupConnection);
            closeAfterSetupControl = page->_AttachControlToContent(closeAfterSetupContent.Id());
            VERIFY_IS_NOT_NULL(closeAfterSetupControl);
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(_statesForPane(connectionStates, closeAfterSetupPaneId).size()));
            closeAfterSetupConnection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);
        });

        TestOnUIThread([&]() {
            const auto closeDuringConfirmStates = _statesForPane(connectionStates, closeDuringConfirmPaneId);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(closeDuringConfirmStates.size()));
            VERIFY_ARE_EQUAL(std::string{ "closed" }, closeDuringConfirmStates.at(0));

            const auto closeAfterSetupStates = _statesForPane(connectionStates, closeAfterSetupPaneId);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(closeAfterSetupStates.size()));
            VERIFY_ARE_EQUAL(std::string{ "closed" }, closeAfterSetupStates.at(0));
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(detachedEvents.size()));

            closeDuringConfirmControl.Close();
            closeAfterSetupControl.Close();
        });
    }

    void TabTests::DetachedReapUsesConfiguredOwnerScheduler()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        std::vector<DispatcherQueueHandler> queuedReaps;
        std::atomic<uint32_t> scheduleCount{ 0 };
        _createContentManager([&](DispatcherQueueHandler callback) {
            ++scheduleCount;
            queuedReaps.emplace_back(std::move(callback));
            return true;
        });

        const auto groupId = ::Microsoft::Console::Utils::GuidFromString(L"{01010101-0202-0303-0404-050505050505}");
        const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{11111111-1212-1313-1414-151515151515}");
        std::vector<DetachedSessionEndedRecord> endedSessions;

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            auto connection = winrt::make_self<TestConnection>(
                sessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
            const auto contentId = content.Id();
            winrt::Microsoft::Terminal::Control::TermControl control{ content };

            const auto closeToken = _contentManager->DetachedSessionClosed([&](auto&&, const winrt::TerminalApp::DetachedSessionEndedArgs& endedSession) {
                endedSessions.push_back({ endedSession.SessionId(), endedSession.State() });
            });
            const auto closeTokenRevoker = wil::scope_exit([&]() noexcept {
                _contentManager->DetachedSessionClosed(closeToken);
            });

            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(groupId, sessionId, L"Configured owner", L"shell-session-owner", 43, NewTerminalArgs{}, control));
            connection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Failed);

            VERIFY_ARE_EQUAL(1u, scheduleCount.load());
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(queuedReaps.size()));
            VERIFY_IS_TRUE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(endedSessions.size()));

            const auto queuedReap = queuedReaps.front();
            queuedReap();

            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
            VERIFY_IS_NULL(_contentManager->TryLookupCore(contentId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(endedSessions.size()));
            VERIFY_IS_TRUE(endedSessions.at(0).state == L"failed");
        });
    }

    void TabTests::FailedDetachedReapFallsBackToPendingDrain()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        std::atomic<uint32_t> scheduleCount{ 0 };
        _createContentManager([&](DispatcherQueueHandler) {
            ++scheduleCount;
            return false;
        });

        const auto groupId = ::Microsoft::Console::Utils::GuidFromString(L"{21212121-2222-2323-2424-252525252525}");
        const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{31313131-3232-3333-3434-353535353535}");
        std::vector<DetachedSessionEndedRecord> endedSessions;

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            auto connection = winrt::make_self<TestConnection>(
                sessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
            const auto contentId = content.Id();
            winrt::Microsoft::Terminal::Control::TermControl control{ content };

            const auto closeToken = _contentManager->DetachedSessionClosed([&](auto&&, const winrt::TerminalApp::DetachedSessionEndedArgs& endedSession) {
                endedSessions.push_back({ endedSession.SessionId(), endedSession.State() });
            });
            const auto closeTokenRevoker = wil::scope_exit([&]() noexcept {
                _contentManager->DetachedSessionClosed(closeToken);
            });

            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(groupId, sessionId, L"Pending owner reap", L"shell-session-pending", 47, NewTerminalArgs{}, control));
            connection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Failed);
            connection->RaiseStateChanged();

            VERIFY_ARE_EQUAL(2u, scheduleCount.load());
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(endedSessions.size()));

            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
            VERIFY_ARE_EQUAL(0u, _contentManager->KeptGroups().Size());
            VERIFY_IS_FALSE(_contentManager->DiscardKeptSession(sessionId));
            VERIFY_IS_NULL(_contentManager->TryLookupCore(contentId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(endedSessions.size()));
            VERIFY_IS_TRUE(endedSessions.at(0).state == L"failed");
        });
    }

    void TabTests::OwnerOperationsDoNotRepeatPendingReapDrain()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        enum class Operation
        {
            Reattach,
            Discard,
            List,
        };

        for (const auto operation : { Operation::Reattach, Operation::Discard, Operation::List })
        {
            _createContentManager([](DispatcherQueueHandler) {
                return false;
            });

            const auto targetGroupId = ::Microsoft::Console::Utils::GuidFromString(L"{91919191-9292-9393-9494-959595959595}");
            const auto triggerGroupId = ::Microsoft::Console::Utils::GuidFromString(L"{a1a1a1a1-a2a2-a3a3-a4a4-a5a5a5a5a5a5}");
            const auto targetSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{b1b1b1b1-b2b2-b3b3-b4b4-b5b5b5b5b5b5}");
            const auto triggerSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{c1c1c1c1-c2c2-c3c3-c4c4-c5c5c5c5c5c5}");

            TestOnUIThread([&]() {
                auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
                auto targetConnection = winrt::make_self<TestConnection>(
                    targetSessionId,
                    winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
                auto triggerConnection = winrt::make_self<TestConnection>(
                    triggerSessionId,
                    winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);

                const auto targetContent = _contentManager->CreateCore(*settings, *settings, *targetConnection);
                const auto triggerContent = _contentManager->CreateCore(*settings, *settings, *triggerConnection);
                winrt::Microsoft::Terminal::Control::TermControl targetControl{ targetContent };
                winrt::Microsoft::Terminal::Control::TermControl triggerControl{ triggerContent };

                VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(targetGroupId, targetSessionId, L"Target", L"target-shell", 1, NewTerminalArgs{}, targetControl));
                VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(triggerGroupId, triggerSessionId, L"Trigger", L"trigger-shell", 1, NewTerminalArgs{}, triggerControl));

                std::vector<DetachedSessionEndedRecord> endedSessions;
                const auto closeToken = _contentManager->DetachedSessionClosed([&](auto&&, const winrt::TerminalApp::DetachedSessionEndedArgs& endedSession) {
                    endedSessions.push_back({ endedSession.SessionId(), endedSession.State() });
                    if (::IsEqualGUID(endedSession.SessionId(), triggerSessionId))
                    {
                        // This pending reap is queued after the public method's
                        // initial drain has swapped its work list.
                        targetConnection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Failed);
                    }
                });

                triggerConnection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Failed);

                switch (operation)
                {
                case Operation::Reattach:
                    VERIFY_ARE_EQUAL(0ull, _contentManager->TryReattachKeptSession(targetSessionId));
                    break;
                case Operation::Discard:
                    VERIFY_IS_FALSE(_contentManager->DiscardKeptSession(targetSessionId));
                    break;
                case Operation::List:
                    VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
                    break;
                }

                VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
                VERIFY_ARE_EQUAL(0u, _contentManager->KeptGroups().Size());
                VERIFY_IS_NULL(_contentManager->TryLookupCore(targetContent.Id()));
                VERIFY_IS_NULL(_contentManager->TryLookupCore(triggerContent.Id()));
                VERIFY_ARE_EQUAL(2u, static_cast<unsigned int>(endedSessions.size()));

                _contentManager->DetachedSessionClosed(closeToken);
            });
        }
    }

    void TabTests::ClosedContentFallbackWaitsForOwnerDrain()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        _createContentManager([](DispatcherQueueHandler) {
            return false;
        });

        const auto groupId = ::Microsoft::Console::Utils::GuidFromString(L"{41414141-4242-4343-4444-454545454545}");
        const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{51515151-5252-5353-5454-555555555555}");
        winrt::Microsoft::Terminal::Control::ControlInteractivity content{ nullptr };
        auto contentId = 0ull;
        std::vector<DetachedSessionEndedRecord> endedSessions;
        winrt::event_token closeToken{};

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            auto connection = winrt::make_self<TestConnection>(
                sessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            content = _contentManager->CreateCore(*settings, *settings, *connection);
            contentId = content.Id();
            winrt::Microsoft::Terminal::Control::TermControl control{ content };

            closeToken = _contentManager->DetachedSessionClosed([&](auto&&, const winrt::TerminalApp::DetachedSessionEndedArgs& endedSession) {
                endedSessions.push_back({ endedSession.SessionId(), endedSession.State() });
            });

            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(groupId, sessionId, L"Pending close", L"shell-session-close", 53, NewTerminalArgs{}, control));
        });

        std::thread closeThread([content]() {
            content.Close();
        });
        closeThread.join();

        TestOnUIThread([&]() {
            VERIFY_IS_NULL(_contentManager->TryLookupCore(contentId));
            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(endedSessions.size()));
            VERIFY_IS_TRUE(!!::IsEqualGUID(endedSessions.at(0).sessionId, sessionId));
            VERIFY_IS_TRUE(endedSessions.at(0).state == L"closed");

            _contentManager->DetachedSessionClosed(closeToken);
        });
    }

    void TabTests::DetachedSessionMetadataAndDiscard()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        const auto groupId = ::Microsoft::Console::Utils::GuidFromString(L"{11111111-2222-3333-4444-555555555555}");
        const winrt::hstring tabTitle{ L"Detached tab title" };
        const winrt::hstring shellSessionId{ L"shell-session-42" };

        TestOnUIThread([&]() {
            const auto activeControl{ page->_GetActiveControl() };
            VERIFY_IS_NOT_NULL(activeControl);

            const auto connection{ activeControl.Connection() };
            VERIFY_IS_NOT_NULL(connection);

            const auto sessionId{ connection.SessionId() };
            VERIFY_IS_TRUE(sessionId != winrt::guid{});

            std::vector<DetachedSessionEndedRecord> endedSessions;
            const auto closeToken = _contentManager->DetachedSessionClosed([&](auto&&, const winrt::TerminalApp::DetachedSessionEndedArgs& endedSession) {
                endedSessions.push_back({ endedSession.SessionId(), endedSession.State() });
            });
            const auto closeTokenRevoker = wil::scope_exit([&]() noexcept {
                _contentManager->DetachedSessionClosed(closeToken);
            });

            const auto retained = _contentManager->DetachForKeepRunning(groupId, sessionId, tabTitle, shellSessionId, 42, NewTerminalArgs{}, activeControl);
            VERIFY_IS_TRUE(retained);

            const auto detachedSessions{ _contentManager->DetachedSessions() };
            VERIFY_ARE_EQUAL(1u, detachedSessions.Size());

            const auto detached = detachedSessions.GetAt(0);
            VERIFY_IS_TRUE(!!::IsEqualGUID(detached.SessionId(), sessionId));
            VERIFY_IS_TRUE(!!::IsEqualGUID(detached.GroupId(), groupId));
            VERIFY_IS_TRUE(detached.TabTitle() == tabTitle);
            VERIFY_IS_TRUE(detached.ShellSessionId() == shellSessionId);

            VERIFY_IS_TRUE(_contentManager->DiscardKeptSession(sessionId));
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(endedSessions.size()));
            VERIFY_IS_TRUE(!!::IsEqualGUID(endedSessions.at(0).sessionId, sessionId));
            VERIFY_IS_TRUE(endedSessions.at(0).state == L"closed");
            VERIFY_IS_FALSE(_contentManager->DiscardKeptSession(sessionId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(endedSessions.size()));
        });
    }

    void TabTests::DetachedSessionAlreadyClosedIsReapedImmediately()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        const auto groupId = ::Microsoft::Console::Utils::GuidFromString(L"{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}");
        const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{99999999-8888-7777-6666-555555555555}");

        _createContentManager();
        VERIFY_IS_NOT_NULL(_contentManager);

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            auto connection = winrt::make_self<TestConnection>(
                sessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(connection);

            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
            VERIFY_IS_NOT_NULL(content);
            const auto contentId = content.Id();

            winrt::Microsoft::Terminal::Control::TermControl control{ content };
            VERIFY_IS_NOT_NULL(control);

            connection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);

            std::vector<DetachedSessionEndedRecord> endedSessions;
            const auto closeToken = _contentManager->DetachedSessionClosed([&](auto&&, const winrt::TerminalApp::DetachedSessionEndedArgs& endedSession) {
                endedSessions.push_back({ endedSession.SessionId(), endedSession.State() });
            });
            const auto closeTokenRevoker = wil::scope_exit([&]() noexcept {
                _contentManager->DetachedSessionClosed(closeToken);
            });

            const auto retained = _contentManager->DetachForKeepRunning(groupId, sessionId, L"Already closed", L"shell-session-closed", 7, NewTerminalArgs{}, control);

            VERIFY_IS_FALSE(retained);
            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(endedSessions.size()));
            VERIFY_IS_FALSE(_contentManager->DiscardKeptSession(sessionId));
            VERIFY_IS_NULL(_contentManager->TryLookupCore(contentId));
        });
    }

    void TabTests::DetachShellPanesForKeepRunningStoresDurableMetadata()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        const winrt::hstring shellSessionId{ L"12345678-1234-5678-9abc-def012345678" };
        constexpr int64_t shellSessionRevision{ 19 };

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);

            page->_DetachShellPanesForKeepRunning(tab.get(), shellSessionId, shellSessionRevision);
            VERIFY_ARE_EQUAL(0u, _contentManager->KeptGroups().Size());

            page->_SplitPane(nullptr, SplitDirection::Right, 0.5f, page->_MakePane(nullptr, page->_GetFocusedTab(), nullptr));
            VERIFY_ARE_EQUAL(2, tab->GetLeafPaneCount());

            tab->KeepRunning(true);
            VERIFY_IS_TRUE(tab->KeepRunning());
            VERIFY_IS_TRUE(tab->TabStatus().IsKeepRunning());
            page->_DetachShellPanesForKeepRunning(tab.get(), shellSessionId, shellSessionRevision);

            const auto keptGroups = _contentManager->KeptGroups();
            VERIFY_ARE_EQUAL(1u, keptGroups.Size());
            VERIFY_ARE_EQUAL(2u, _contentManager->DetachedSessions().Size());

            winrt::guid groupId{};
            for (const auto& group : keptGroups)
            {
                groupId = group.Key();
                break;
            }

            VERIFY_IS_TRUE(groupId != winrt::guid{});
            VERIFY_IS_TRUE(!!::IsEqualGUID(groupId, ::Microsoft::Console::Utils::GuidFromPlainString(shellSessionId.c_str())));

            auto restored = _contentManager->BeginReattachKeptGroup(groupId);
            VERIFY_IS_TRUE(!!restored);
            VERIFY_ARE_EQUAL(2u, restored.ContentIds().Size());
            VERIFY_ARE_EQUAL(2u, restored.RestoreArgs().Size());
            VERIFY_IS_TRUE(restored.ShellSessionId() == shellSessionId);
            VERIFY_ARE_EQUAL(shellSessionRevision, restored.ShellSessionRevision());
            VERIFY_IS_TRUE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
            VERIFY_ARE_EQUAL(0u, _contentManager->KeptGroups().Size());
            _contentManager->CancelKeptGroupReattach(groupId);
            VERIFY_ARE_EQUAL(2u, _contentManager->DetachedSessions().Size());
            VERIFY_ARE_EQUAL(1u, _contentManager->KeptGroups().Size());

            restored = _contentManager->BeginReattachKeptGroup(groupId);
            VERIFY_IS_TRUE(!!restored);
            VERIFY_IS_TRUE(_contentManager->ConfirmReattachedContent(restored.ContentIds().GetAt(0)));
            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
        });
    }

    void TabTests::DetachedSessionsSkipClosedConnectionBeforeQueuedReap()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        const auto groupId = ::Microsoft::Console::Utils::GuidFromString(L"{abababab-bcbc-cdcd-dede-efefefefefef}");
        const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{01020304-1111-2222-3333-444455556666}");

        _createContentManager();
        VERIFY_IS_NOT_NULL(_contentManager);

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            auto connection = winrt::make_self<TestConnection>(
                sessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(connection);

            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
            VERIFY_IS_NOT_NULL(content);

            winrt::Microsoft::Terminal::Control::TermControl control{ content };
            VERIFY_IS_NOT_NULL(control);

            const auto retained = _contentManager->DetachForKeepRunning(groupId, sessionId, L"Queued close", L"shell-session-live", 11, NewTerminalArgs{}, control);
            VERIFY_IS_TRUE(retained);

            const auto liveDetachedSessions{ _contentManager->DetachedSessions() };
            VERIFY_ARE_EQUAL(1u, liveDetachedSessions.Size());
            VERIFY_ARE_EQUAL(0u, liveDetachedSessions.GetAt(0).Pid());

            connection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);

            VERIFY_IS_TRUE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
        });

        TestOnUIThread([&]() {
            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
        });
    }

    void TabTests::BuildKeptGroupRestoreActionsPreservesRestoreArguments()
    {
        const auto firstSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{11111111-aaaa-bbbb-cccc-111111111111}");
        const auto secondSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{22222222-aaaa-bbbb-cccc-222222222222}");

        NewTerminalArgs firstRestoreArgs{};
        firstRestoreArgs.ContentId(101);
        firstRestoreArgs.SessionId(firstSessionId);
        firstRestoreArgs.Profile(L"{11111111-2222-3333-4444-555555555555}");
        firstRestoreArgs.StartingDirectory(L"C:\\first");
        firstRestoreArgs.Commandline(L"first-command");
        firstRestoreArgs.TabTitle(L"first title");
        firstRestoreArgs.AgentSessionId(L"agent-first");
        firstRestoreArgs.AgentSessionAgent(L"copilot");
        firstRestoreArgs.AgentResumeCommandline(L"copilot --resume agent-first");

        NewTerminalArgs secondRestoreArgs{};
        secondRestoreArgs.ContentId(202);
        secondRestoreArgs.SessionId(secondSessionId);
        secondRestoreArgs.Profile(L"{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}");
        secondRestoreArgs.StartingDirectory(L"C:\\second");
        secondRestoreArgs.Commandline(L"second-command");
        secondRestoreArgs.TabTitle(L"second title");
        secondRestoreArgs.AgentSessionId(L"agent-second");
        secondRestoreArgs.AgentSessionAgent(L"claude");
        secondRestoreArgs.AgentResumeCommandline(L"claude --resume agent-second");

        const auto restored = winrt::make<winrt::TerminalApp::implementation::KeptGroupRestoreResult>(
            std::vector<NewTerminalArgs>{ firstRestoreArgs, secondRestoreArgs },
            winrt::hstring{ L"Detached tab name" },
            winrt::hstring{ L"shell-session-restored" },
            55);

        const auto actions = winrt::TerminalApp::implementation::BuildKeptGroupRestoreActions(restored);
        VERIFY_ARE_EQUAL(3u, static_cast<unsigned int>(actions.size()));

        VERIFY_ARE_EQUAL(ShortcutAction::RenameTab, actions.at(2).Action());
        const auto renameAction = actions.at(2).Args().try_as<RenameTabArgs>();
        VERIFY_IS_NOT_NULL(renameAction);
        VERIFY_ARE_EQUAL(winrt::hstring{ L"Detached tab name" }, renameAction.Title());

        // Restoring a detached tab hands these actions to the target window as
        // JSON, so the keep-running opt-in has to survive a round trip.
        const auto serialized = ActionAndArgs::Serialize(winrt::single_threaded_vector<ActionAndArgs>(std::vector<ActionAndArgs>{ actions }));
        const auto deserialized = ActionAndArgs::Deserialize(serialized);
        VERIFY_ARE_EQUAL(3u, deserialized.Size());
        const auto roundTrippedTab = deserialized.GetAt(0).Args().try_as<NewTabArgs>();
        VERIFY_IS_NOT_NULL(roundTrippedTab);
        const auto roundTrippedArgs = roundTrippedTab.ContentArgs().try_as<NewTerminalArgs>();
        VERIFY_IS_NOT_NULL(roundTrippedArgs);
        VERIFY_IS_TRUE(roundTrippedArgs.KeepRunning());
        const auto roundTrippedRename = deserialized.GetAt(2).Args().try_as<RenameTabArgs>();
        VERIFY_IS_NOT_NULL(roundTrippedRename);
        VERIFY_ARE_EQUAL(winrt::hstring{ L"Detached tab name" }, roundTrippedRename.Title());

        const auto untitled = winrt::make<winrt::TerminalApp::implementation::KeptGroupRestoreResult>(
            std::vector<NewTerminalArgs>{ firstRestoreArgs },
            winrt::hstring{},
            winrt::hstring{ L"shell-session-restored" },
            55);
        VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(winrt::TerminalApp::implementation::BuildKeptGroupRestoreActions(untitled).size()));

        VERIFY_ARE_EQUAL(ShortcutAction::NewTab, actions.at(0).Action());
        const auto firstAction = actions.at(0).Args().try_as<NewTabArgs>();
        VERIFY_IS_NOT_NULL(firstAction);
        const auto firstTerminalArgs = firstAction.ContentArgs().try_as<NewTerminalArgs>();
        VERIFY_IS_NOT_NULL(firstTerminalArgs);
        VERIFY_ARE_EQUAL(101ull, firstTerminalArgs.ContentId());
        VERIFY_ARE_EQUAL(firstRestoreArgs.Profile(), firstTerminalArgs.Profile());
        VERIFY_ARE_EQUAL(firstRestoreArgs.StartingDirectory(), firstTerminalArgs.StartingDirectory());
        VERIFY_ARE_EQUAL(firstRestoreArgs.Commandline(), firstTerminalArgs.Commandline());
        VERIFY_ARE_EQUAL(firstRestoreArgs.TabTitle(), firstTerminalArgs.TabTitle());
        VERIFY_ARE_EQUAL(firstRestoreArgs.AgentSessionId(), firstTerminalArgs.AgentSessionId());
        VERIFY_ARE_EQUAL(firstRestoreArgs.AgentSessionAgent(), firstTerminalArgs.AgentSessionAgent());
        VERIFY_ARE_EQUAL(firstRestoreArgs.AgentResumeCommandline(), firstTerminalArgs.AgentResumeCommandline());
        VERIFY_IS_TRUE(!!::IsEqualGUID(firstTerminalArgs.KeptSessionId(), firstSessionId));
        VERIFY_IS_TRUE(firstTerminalArgs.DurableShellSessionId() == L"shell-session-restored");
        VERIFY_ARE_EQUAL(55LL, firstTerminalArgs.DurableShellSessionRevision());

        VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actions.at(1).Action());
        const auto secondAction = actions.at(1).Args().try_as<SplitPaneArgs>();
        VERIFY_IS_NOT_NULL(secondAction);
        const auto secondTerminalArgs = secondAction.ContentArgs().try_as<NewTerminalArgs>();
        VERIFY_IS_NOT_NULL(secondTerminalArgs);
        VERIFY_ARE_EQUAL(202ull, secondTerminalArgs.ContentId());
        VERIFY_ARE_EQUAL(secondRestoreArgs.Profile(), secondTerminalArgs.Profile());
        VERIFY_ARE_EQUAL(secondRestoreArgs.StartingDirectory(), secondTerminalArgs.StartingDirectory());
        VERIFY_ARE_EQUAL(secondRestoreArgs.Commandline(), secondTerminalArgs.Commandline());
        VERIFY_ARE_EQUAL(secondRestoreArgs.TabTitle(), secondTerminalArgs.TabTitle());
        VERIFY_ARE_EQUAL(secondRestoreArgs.AgentSessionId(), secondTerminalArgs.AgentSessionId());
        VERIFY_ARE_EQUAL(secondRestoreArgs.AgentSessionAgent(), secondTerminalArgs.AgentSessionAgent());
        VERIFY_ARE_EQUAL(secondRestoreArgs.AgentResumeCommandline(), secondTerminalArgs.AgentResumeCommandline());
        VERIFY_IS_TRUE(!!::IsEqualGUID(secondTerminalArgs.KeptSessionId(), secondSessionId));
        VERIFY_IS_TRUE(secondTerminalArgs.DurableShellSessionId().empty());
        VERIFY_ARE_EQUAL(0LL, secondTerminalArgs.DurableShellSessionRevision());
    }

    void TabTests::BeginReattachKeptGroupPreservesPaneRestoreArgs()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        const auto groupId = ::Microsoft::Console::Utils::GuidFromString(L"{5a5a5a5a-1111-2222-3333-444444444444}");
        const auto firstSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{5a5a5a5a-aaaa-bbbb-cccc-dddddddddddd}");
        const auto secondSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{5a5a5a5a-eeee-ffff-1111-222222222222}");

        _createContentManager();
        VERIFY_IS_NOT_NULL(_contentManager);

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            auto firstConnection = winrt::make_self<TestConnection>(
                firstSessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            auto secondConnection = winrt::make_self<TestConnection>(
                secondSessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            const auto firstContent = _contentManager->CreateCore(*settings, *settings, *firstConnection);
            const auto secondContent = _contentManager->CreateCore(*settings, *settings, *secondConnection);
            winrt::Microsoft::Terminal::Control::TermControl firstControl{ firstContent };
            winrt::Microsoft::Terminal::Control::TermControl secondControl{ secondContent };

            NewTerminalArgs firstRestoreArgs{};
            firstRestoreArgs.Profile(L"{11111111-2222-3333-4444-555555555555}");
            firstRestoreArgs.StartingDirectory(L"C:\\first");
            firstRestoreArgs.Commandline(L"first-command");
            firstRestoreArgs.TabTitle(L"first title");
            firstRestoreArgs.AgentSessionId(L"agent-first");
            firstRestoreArgs.AgentSessionAgent(L"copilot");
            firstRestoreArgs.AgentResumeCommandline(L"copilot --resume agent-first");

            NewTerminalArgs secondRestoreArgs{};
            secondRestoreArgs.Profile(L"{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}");
            secondRestoreArgs.StartingDirectory(L"C:\\second");
            secondRestoreArgs.Commandline(L"second-command");
            secondRestoreArgs.TabTitle(L"second title");
            secondRestoreArgs.AgentSessionId(L"agent-second");
            secondRestoreArgs.AgentSessionAgent(L"claude");
            secondRestoreArgs.AgentResumeCommandline(L"claude --resume agent-second");

            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(groupId, firstSessionId, L"Restore args", L"shell-session", 9, firstRestoreArgs, firstControl));
            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(groupId, secondSessionId, L"Restore args", L"shell-session", 9, secondRestoreArgs, secondControl));
            firstRestoreArgs.Profile(L"{00000000-0000-0000-0000-000000000000}");
            secondRestoreArgs.AgentSessionId(L"mutated");

            const auto restored = _contentManager->BeginReattachKeptGroup(groupId);
            VERIFY_IS_TRUE(!!restored);
            VERIFY_ARE_EQUAL(2u, restored.RestoreArgs().Size());
            const auto restoredFirst = restored.RestoreArgs().GetAt(0);
            const auto restoredSecond = restored.RestoreArgs().GetAt(1);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"{11111111-2222-3333-4444-555555555555}" }, restoredFirst.Profile());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"C:\\first" }, restoredFirst.StartingDirectory());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"first-command" }, restoredFirst.Commandline());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"first title" }, restoredFirst.TabTitle());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"agent-first" }, restoredFirst.AgentSessionId());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"copilot" }, restoredFirst.AgentSessionAgent());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"copilot --resume agent-first" }, restoredFirst.AgentResumeCommandline());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}" }, restoredSecond.Profile());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"C:\\second" }, restoredSecond.StartingDirectory());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"second-command" }, restoredSecond.Commandline());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"second title" }, restoredSecond.TabTitle());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"agent-second" }, restoredSecond.AgentSessionId());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"claude" }, restoredSecond.AgentSessionAgent());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"claude --resume agent-second" }, restoredSecond.AgentResumeCommandline());

            VERIFY_IS_TRUE(_contentManager->ConfirmReattachedContent(firstContent.Id()));
            VERIFY_IS_TRUE(_contentManager->ConfirmReattachedContent(secondContent.Id()));
            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
        });
    }

    void TabTests::DurableSessionCloseWritesSaveResultsToTabAndPersistedActions()
    {
        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);

            const auto applyAndVerify = [&](const std::string_view response,
                                            const winrt::hstring& expectedId,
                                            const int64_t expectedRevision,
                                            const bool expectedForked) {
                const auto save = winrt::TerminalApp::implementation::TerminalPage::_ParseShellSessionSaveResponse(response);
                VERIFY_ARE_EQUAL(expectedForked, save.forked);

                winrt::TerminalApp::implementation::TerminalPage::_ApplyShellSessionSaveResult(tab.get(), save);
                VERIFY_ARE_EQUAL(expectedId, tab->DurableShellSessionId());
                VERIFY_ARE_EQUAL(expectedRevision, tab->DurableShellSessionRevision());

                const auto persistedLayout = WindowLayout::FromJson(WindowLayout::ToJson(page->_GetWindowLayout(true)));
                VERIFY_IS_NOT_NULL(persistedLayout);
                const auto persistedActions = persistedLayout.TabLayout();
                VERIFY_IS_NOT_NULL(persistedActions);
                VERIFY_ARE_EQUAL(1u, persistedActions.Size());

                const auto terminalArgs = _getTerminalArgs(persistedActions.GetAt(0));
                VERIFY_IS_NOT_NULL(terminalArgs);
                VERIFY_ARE_EQUAL(expectedId, terminalArgs.DurableShellSessionId());
                VERIFY_ARE_EQUAL(expectedRevision, terminalArgs.DurableShellSessionRevision());

                return persistedActions.GetAt(0).Args().as<NewTabArgs>();
            };

            applyAndVerify(R"({"id":"shell-session-first","revision":1,"forked":false})",
                           L"shell-session-first",
                           1,
                           false);
            const auto forkedArgs = applyAndVerify(R"({"id":"shell-session-fork","revision":2,"forked":true})",
                                                   L"shell-session-fork",
                                                   2,
                                                   true);

            page->_HandleNewTab(nullptr, ActionEventArgs{ forkedArgs });
            const auto restoredTab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(restoredTab);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"shell-session-fork" }, restoredTab->DurableShellSessionId());
            VERIFY_ARE_EQUAL(2LL, restoredTab->DurableShellSessionRevision());
        });
    }

    void TabTests::GetWindowLayoutIncludesDurableMetadataForPersistedFullLayoutsOnly()
    {
        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        TestOnUIThread([&]() {
            const auto tab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_IS_NOT_NULL(tab);

            page->_SplitPane(nullptr, SplitDirection::Right, 0.5f, page->_MakePane(nullptr, page->_GetFocusedTab(), nullptr));
            VERIFY_ARE_EQUAL(2, tab->GetLeafPaneCount());

            tab->SetDurableShellSession(L"shell-session-persisted", 91);

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

            const auto publicLayout = page->GetWindowLayout();
            VERIFY_IS_NOT_NULL(publicLayout);
            const auto publicActions = publicLayout.TabLayout();
            VERIFY_IS_NOT_NULL(publicActions);
            VERIFY_ARE_EQUAL(2u, publicActions.Size());
            const auto publicFirstTerminalArgs = _getTerminalArgs(publicActions.GetAt(0));
            VERIFY_IS_NOT_NULL(publicFirstTerminalArgs);
            VERIFY_IS_TRUE(publicFirstTerminalArgs.DurableShellSessionId().empty());
            VERIFY_ARE_EQUAL(0LL, publicFirstTerminalArgs.DurableShellSessionRevision());

            const auto persistedLayout = page->_GetWindowLayout(true);
            VERIFY_IS_NOT_NULL(persistedLayout);

            const auto roundTrippedLayout = WindowLayout::FromJson(WindowLayout::ToJson(persistedLayout));
            VERIFY_IS_NOT_NULL(roundTrippedLayout);

            const auto persistedActions = roundTrippedLayout.TabLayout();
            VERIFY_IS_NOT_NULL(persistedActions);
            VERIFY_ARE_EQUAL(2u, persistedActions.Size());

            VERIFY_ARE_EQUAL(ShortcutAction::NewTab, persistedActions.GetAt(0).Action());
            const auto firstTerminalArgs = _getTerminalArgs(persistedActions.GetAt(0));
            VERIFY_IS_NOT_NULL(firstTerminalArgs);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"shell-session-persisted" }, firstTerminalArgs.DurableShellSessionId());
            VERIFY_ARE_EQUAL(91LL, firstTerminalArgs.DurableShellSessionRevision());
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
            VERIFY_IS_TRUE(secondTerminalArgs.DurableShellSessionId().empty());
            VERIFY_ARE_EQUAL(0LL, secondTerminalArgs.DurableShellSessionRevision());
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

    void TabTests::PersistStateForUnnamedWindowIncludesDurableMetadata()
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

            tab->SetDurableShellSession(L"shell-session-unnamed", 17);
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
        VERIFY_ARE_EQUAL(winrt::hstring{ L"shell-session-unnamed" }, terminalArgs.DurableShellSessionId());
        VERIFY_ARE_EQUAL(17LL, terminalArgs.DurableShellSessionRevision());
    }

    void TabTests::RestoreAllKeptGroupsSnapshotsOrderAndReportsFallback()
    {
        const auto firstGroup = ::Microsoft::Console::Utils::GuidFromString(L"{10101010-1111-1212-1313-141414141414}");
        const auto secondGroup = ::Microsoft::Console::Utils::GuidFromString(L"{20202020-2121-2222-2323-242424242424}");
        auto groups = winrt::single_threaded_map<winrt::guid, winrt::hstring>();
        groups.Insert(firstGroup, L"first");
        groups.Insert(secondGroup, L"second");

        std::vector<winrt::guid> expectedOrder;
        for (const auto& group : groups.GetView())
        {
            expectedOrder.emplace_back(group.Key());
        }

        std::vector<winrt::guid> restoredOrder;
        const auto restored = winrt::TerminalApp::implementation::RestoreAllKeptGroups(
            groups.GetView(),
            [&](const auto& groupId) {
                restoredOrder.emplace_back(groupId);
                if (restoredOrder.size() == 1)
                {
                    groups.Clear();
                }
                return !!::IsEqualGUID(groupId, secondGroup);
            });

        VERIFY_IS_TRUE(restored);
        VERIFY_ARE_EQUAL(expectedOrder.size(), restoredOrder.size());
        for (size_t i = 0; i < expectedOrder.size(); ++i)
        {
            VERIFY_IS_TRUE(!!::IsEqualGUID(expectedOrder.at(i), restoredOrder.at(i)));
        }

        auto fallbackGroups = winrt::single_threaded_map<winrt::guid, winrt::hstring>();
        fallbackGroups.Insert(firstGroup, L"first");
        fallbackGroups.Insert(secondGroup, L"second");
        auto fallbackCalls = 0u;
        const auto fallback = winrt::TerminalApp::implementation::RestoreAllKeptGroups(
            fallbackGroups.GetView(),
            [&](const auto&) {
                ++fallbackCalls;
                return false;
            });
        VERIFY_IS_FALSE(fallback);
        VERIFY_ARE_EQUAL(2u, fallbackCalls);
    }

    void TabTests::DiscardDeadDetachedSessionBeforeQueuedReapReturnsFalse()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        const auto groupId = ::Microsoft::Console::Utils::GuidFromString(L"{fedcbafe-4444-5555-6666-777788889999}");
        const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{00112233-4455-6677-8899-aabbccddeeff}");

        _createContentManager();
        VERIFY_IS_NOT_NULL(_contentManager);

        std::vector<DetachedSessionEndedRecord> endedSessions;
        auto contentId = 0ull;

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            auto connection = winrt::make_self<TestConnection>(
                sessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(connection);

            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
            VERIFY_IS_NOT_NULL(content);
            contentId = content.Id();

            winrt::Microsoft::Terminal::Control::TermControl control{ content };
            VERIFY_IS_NOT_NULL(control);

            const auto closeToken = _contentManager->DetachedSessionClosed([&](auto&&, const winrt::TerminalApp::DetachedSessionEndedArgs& endedSession) {
                endedSessions.push_back({ endedSession.SessionId(), endedSession.State() });
            });
            const auto closeTokenRevoker = wil::scope_exit([&]() noexcept {
                _contentManager->DetachedSessionClosed(closeToken);
            });

            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(groupId, sessionId, L"Queued reap discard", L"shell-session-live", 13, NewTerminalArgs{}, control));
            VERIFY_ARE_EQUAL(1u, _contentManager->DetachedSessions().Size());

            connection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Failed);

            VERIFY_IS_TRUE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
            VERIFY_IS_FALSE(_contentManager->DiscardKeptSession(sessionId));
            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
            VERIFY_IS_NULL(_contentManager->TryLookupCore(contentId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(endedSessions.size()));
            VERIFY_IS_TRUE(!!::IsEqualGUID(endedSessions.at(0).sessionId, sessionId));
            VERIFY_IS_TRUE(endedSessions.at(0).state == L"failed");
            VERIFY_IS_FALSE(_contentManager->DiscardKeptSession(sessionId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(endedSessions.size()));
        });

        TestOnUIThread([&]() {
            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
            VERIFY_IS_NULL(_contentManager->TryLookupCore(contentId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(endedSessions.size()));
        });
    }

    void TabTests::TryReattachDeadDetachedSessionBeforeQueuedReapReturnsZero()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        const auto groupId = ::Microsoft::Console::Utils::GuidFromString(L"{12345678-1111-2222-3333-444444444444}");
        const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{87654321-aaaa-bbbb-cccc-dddddddddddd}");

        _createContentManager();
        VERIFY_IS_NOT_NULL(_contentManager);

        std::vector<DetachedSessionEndedRecord> endedSessions;
        winrt::event_token closeToken{};
        auto contentId = 0ull;

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            auto connection = winrt::make_self<TestConnection>(
                sessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(connection);

            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
            VERIFY_IS_NOT_NULL(content);
            contentId = content.Id();

            winrt::Microsoft::Terminal::Control::TermControl control{ content };
            VERIFY_IS_NOT_NULL(control);

            closeToken = _contentManager->DetachedSessionClosed([&](auto&&, const winrt::TerminalApp::DetachedSessionEndedArgs& endedSession) {
                endedSessions.push_back({ endedSession.SessionId(), endedSession.State() });
            });

            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(groupId, sessionId, L"Queued reap reattach", L"shell-session-live", 17, NewTerminalArgs{}, control));
            VERIFY_ARE_EQUAL(1u, _contentManager->DetachedSessions().Size());

            connection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Failed);

            VERIFY_IS_TRUE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
            VERIFY_ARE_EQUAL(0ull, _contentManager->TryReattachKeptSession(sessionId));
            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
            VERIFY_ARE_EQUAL(0u, _contentManager->KeptGroups().Size());
            VERIFY_IS_NULL(_contentManager->TryLookupCore(contentId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(endedSessions.size()));
            VERIFY_IS_TRUE(!!::IsEqualGUID(endedSessions.at(0).sessionId, sessionId));
            VERIFY_IS_TRUE(endedSessions.at(0).state == L"failed");
        });

        TestOnUIThread([&]() {
            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
            VERIFY_ARE_EQUAL(0u, _contentManager->KeptGroups().Size());
            VERIFY_IS_NULL(_contentManager->TryLookupCore(contentId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(endedSessions.size()));

            _contentManager->DetachedSessionClosed(closeToken);
        });
    }

    void TabTests::BeginReattachKeptGroupSkipsDeadMembersBeforeQueuedReap()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        const auto groupId = ::Microsoft::Console::Utils::GuidFromString(L"{0f0f0f0f-1010-2020-3030-404040404040}");
        const auto deadSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{deadbeef-aaaa-bbbb-cccc-111111111111}");
        const auto liveSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{feedface-dddd-eeee-ffff-222222222222}");
        constexpr int64_t shellSessionRevision{ 23 };

        _createContentManager();
        VERIFY_IS_NOT_NULL(_contentManager);

        std::vector<DetachedSessionEndedRecord> endedSessions;
        winrt::event_token closeToken{};
        auto deadContentId = 0ull;
        auto liveContentId = 0ull;

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            auto deadConnection = winrt::make_self<TestConnection>(
                deadSessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(deadConnection);
            auto liveConnection = winrt::make_self<TestConnection>(
                liveSessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(liveConnection);

            const auto deadContent = _contentManager->CreateCore(*settings, *settings, *deadConnection);
            VERIFY_IS_NOT_NULL(deadContent);
            deadContentId = deadContent.Id();

            const auto liveContent = _contentManager->CreateCore(*settings, *settings, *liveConnection);
            VERIFY_IS_NOT_NULL(liveContent);
            liveContentId = liveContent.Id();

            winrt::Microsoft::Terminal::Control::TermControl deadControl{ deadContent };
            VERIFY_IS_NOT_NULL(deadControl);
            winrt::Microsoft::Terminal::Control::TermControl liveControl{ liveContent };
            VERIFY_IS_NOT_NULL(liveControl);

            closeToken = _contentManager->DetachedSessionClosed([&](auto&&, const winrt::TerminalApp::DetachedSessionEndedArgs& endedSession) {
                endedSessions.push_back({ endedSession.SessionId(), endedSession.State() });
            });

            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(groupId, deadSessionId, L"Mixed keep-running group", L"shell-session-group", shellSessionRevision, NewTerminalArgs{}, deadControl));
            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(groupId, liveSessionId, L"Mixed keep-running group", L"shell-session-group", shellSessionRevision, NewTerminalArgs{}, liveControl));
            VERIFY_ARE_EQUAL(2u, _contentManager->DetachedSessions().Size());

            deadConnection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Failed);

            const auto remainingDetachedSessions{ _contentManager->DetachedSessions() };
            VERIFY_ARE_EQUAL(1u, remainingDetachedSessions.Size());
            VERIFY_IS_TRUE(!!::IsEqualGUID(remainingDetachedSessions.GetAt(0).SessionId(), liveSessionId));

            const auto restored = _contentManager->BeginReattachKeptGroup(groupId);
            VERIFY_IS_TRUE(!!restored);
            VERIFY_ARE_EQUAL(1u, restored.ContentIds().Size());
            VERIFY_ARE_EQUAL(1u, restored.RestoreArgs().Size());
            VERIFY_ARE_EQUAL(liveContentId, restored.ContentIds().GetAt(0));
            VERIFY_IS_TRUE(restored.ShellSessionId() == L"shell-session-group");
            VERIFY_ARE_EQUAL(shellSessionRevision, restored.ShellSessionRevision());
            VERIFY_IS_TRUE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
            VERIFY_ARE_EQUAL(0u, _contentManager->KeptGroups().Size());
            VERIFY_ARE_EQUAL(0ull, _contentManager->TryReattachKeptSession(deadSessionId));
            VERIFY_ARE_EQUAL(0ull, _contentManager->TryReattachKeptSession(liveSessionId));
            VERIFY_IS_NULL(_contentManager->TryLookupCore(deadContentId));
            VERIFY_IS_NOT_NULL(_contentManager->TryLookupCore(liveContentId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(endedSessions.size()));
            VERIFY_IS_TRUE(!!::IsEqualGUID(endedSessions.at(0).sessionId, deadSessionId));
            VERIFY_IS_TRUE(endedSessions.at(0).state == L"failed");

            VERIFY_IS_TRUE(_contentManager->ConfirmReattachedContent(liveContentId));
            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->KeptGroups().Size());
        });

        TestOnUIThread([&]() {
            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
            VERIFY_ARE_EQUAL(0u, _contentManager->KeptGroups().Size());
            VERIFY_IS_NULL(_contentManager->TryLookupCore(deadContentId));
            VERIFY_IS_NOT_NULL(_contentManager->TryLookupCore(liveContentId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(endedSessions.size()));

            _contentManager->DetachedSessionClosed(closeToken);
        });
    }

    void TabTests::BeginReattachKeptGroupReturnsNullWhenAllMembersDeadBeforeQueuedReap()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        const auto groupId = ::Microsoft::Console::Utils::GuidFromString(L"{1f1f1f1f-2020-3030-4040-505050505050}");
        const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{abcdefab-cdef-1234-5678-90abcdef1234}");
        constexpr int64_t shellSessionRevision{ 29 };

        _createContentManager();
        VERIFY_IS_NOT_NULL(_contentManager);

        auto contentId = 0ull;

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            auto connection = winrt::make_self<TestConnection>(
                sessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(connection);

            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
            VERIFY_IS_NOT_NULL(content);
            contentId = content.Id();

            winrt::Microsoft::Terminal::Control::TermControl control{ content };
            VERIFY_IS_NOT_NULL(control);

            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(groupId, sessionId, L"All dead keep-running group", L"shell-session-dead-group", shellSessionRevision, NewTerminalArgs{}, control));
            connection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);

            VERIFY_IS_TRUE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());

            const auto restored = _contentManager->BeginReattachKeptGroup(groupId);
            VERIFY_IS_FALSE(!!restored);
            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->KeptGroups().Size());
            VERIFY_IS_NULL(_contentManager->TryLookupCore(contentId));
        });

        TestOnUIThread([&]() {
            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->KeptGroups().Size());
            VERIFY_IS_NULL(_contentManager->TryLookupCore(contentId));
        });
    }

    void TabTests::DetachedFailedSessionQueuedReapEmitsFailedEventOnce()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        const auto groupId = ::Microsoft::Console::Utils::GuidFromString(L"{0abc1234-5555-6666-7777-88889999aaaa}");
        const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{9999aaaa-bbbb-cccc-dddd-eeeeffff0000}");

        _createContentManager();
        VERIFY_IS_NOT_NULL(_contentManager);

        std::vector<DetachedSessionEndedRecord> endedSessions;
        auto contentId = 0ull;

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            auto connection = winrt::make_self<TestConnection>(
                sessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(connection);

            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
            VERIFY_IS_NOT_NULL(content);
            contentId = content.Id();

            winrt::Microsoft::Terminal::Control::TermControl control{ content };
            VERIFY_IS_NOT_NULL(control);

            const auto closeToken = _contentManager->DetachedSessionClosed([&](auto&&, const winrt::TerminalApp::DetachedSessionEndedArgs& endedSession) {
                endedSessions.push_back({ endedSession.SessionId(), endedSession.State() });
            });
            const auto closeTokenRevoker = wil::scope_exit([&]() noexcept {
                _contentManager->DetachedSessionClosed(closeToken);
            });

            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(groupId, sessionId, L"Queued failed close", L"shell-session-failed", 31, NewTerminalArgs{}, control));
            VERIFY_ARE_EQUAL(1u, _contentManager->DetachedSessions().Size());

            connection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Failed);

            VERIFY_IS_TRUE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(endedSessions.size()));
        });

        TestOnUIThread([&]() {
            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
            VERIFY_IS_NULL(_contentManager->TryLookupCore(contentId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(endedSessions.size()));
            VERIFY_IS_TRUE(!!::IsEqualGUID(endedSessions.at(0).sessionId, sessionId));
            VERIFY_IS_TRUE(endedSessions.at(0).state == L"failed");
        });
    }

    void TabTests::DiscardKeptGroupPreservesFailedMembersAndClosesLiveMembers()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        const auto groupId = ::Microsoft::Console::Utils::GuidFromString(L"{0badc0de-1111-2222-3333-444455556666}");
        const auto failedSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{13572468-aaaa-bbbb-cccc-111122223333}");
        const auto liveSessionId = ::Microsoft::Console::Utils::GuidFromString(L"{24681357-dddd-eeee-ffff-444455556666}");

        _createContentManager();
        VERIFY_IS_NOT_NULL(_contentManager);

        std::vector<DetachedSessionEndedRecord> endedSessions;

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            auto failedConnection = winrt::make_self<TestConnection>(
                failedSessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(failedConnection);
            auto liveConnection = winrt::make_self<TestConnection>(
                liveSessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(liveConnection);

            const auto failedContent = _contentManager->CreateCore(*settings, *settings, *failedConnection);
            VERIFY_IS_NOT_NULL(failedContent);
            const auto liveContent = _contentManager->CreateCore(*settings, *settings, *liveConnection);
            VERIFY_IS_NOT_NULL(liveContent);

            winrt::Microsoft::Terminal::Control::TermControl failedControl{ failedContent };
            VERIFY_IS_NOT_NULL(failedControl);
            winrt::Microsoft::Terminal::Control::TermControl liveControl{ liveContent };
            VERIFY_IS_NOT_NULL(liveControl);

            const auto closeToken = _contentManager->DetachedSessionClosed([&](auto&&, const winrt::TerminalApp::DetachedSessionEndedArgs& endedSession) {
                endedSessions.push_back({ endedSession.SessionId(), endedSession.State() });
            });
            const auto closeTokenRevoker = wil::scope_exit([&]() noexcept {
                _contentManager->DetachedSessionClosed(closeToken);
            });

            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(groupId, failedSessionId, L"Discard group", L"shell-session-group-discard", 37, NewTerminalArgs{}, failedControl));
            VERIFY_IS_TRUE(_contentManager->DetachForKeepRunning(groupId, liveSessionId, L"Discard group", L"shell-session-group-discard", 37, NewTerminalArgs{}, liveControl));
            VERIFY_ARE_EQUAL(2u, _contentManager->DetachedSessions().Size());

            failedConnection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Failed);

            VERIFY_IS_TRUE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(1u, _contentManager->DetachedSessions().Size());

            _contentManager->DiscardKeptGroup(groupId);

            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, _contentManager->DetachedSessions().Size());
            VERIFY_ARE_EQUAL(0u, _contentManager->KeptGroups().Size());
            VERIFY_ARE_EQUAL(2u, static_cast<unsigned int>(endedSessions.size()));

            const auto findState = [&](const winrt::guid& sessionId) {
                for (const auto& endedSession : endedSessions)
                {
                    if (!!::IsEqualGUID(endedSession.sessionId, sessionId))
                    {
                        return endedSession.state;
                    }
                }
                return winrt::hstring{};
            };

            VERIFY_IS_TRUE(findState(failedSessionId) == L"failed");
            VERIFY_IS_TRUE(findState(liveSessionId) == L"closed");
        });

        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(2u, static_cast<unsigned int>(endedSessions.size()));
        });
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

    void TabTests::FailedDetachFallbackEmitsFailedEventOnce()
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

        winrt::com_ptr<winrt::TerminalApp::implementation::Tab> tab;
        std::string expectedPaneId;

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{42345678-1234-5678-9abc-def012345678}");
            auto connection = winrt::make_self<TestConnection>(
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
            connection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Failed);

            tab->KeepRunning(true);
            page->_DetachShellPanesForKeepRunning(tab.get(), winrt::hstring{}, 0);

            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
            VERIFY_IS_TRUE(page->_panesKeptRunning.empty());

            page->_NotifyPanesClosing(tab->GetRootPane());
        });

        TestOnUIThread([&]() {
            const auto paneStates = _statesForPane(connectionStates, expectedPaneId);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(paneStates.size()));
            VERIFY_ARE_EQUAL(std::string{ "failed" }, paneStates.at(0));
        });
    }

    void TabTests::SuccessfulDetachedCloseDefersEndEventToContentManager()
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

        const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{52345678-1234-5678-9abc-def012345678}");
        const winrt::hstring shellSessionId{ L"shell-session-52345678" };
        winrt::com_ptr<winrt::TerminalApp::implementation::Tab> tab;
        std::string expectedPaneId;
        std::vector<DetachedSessionEndedRecord> endedSessions;
        winrt::event_token closeToken{};

        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            auto connection = winrt::make_self<TestConnection>(
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

            closeToken = _contentManager->DetachedSessionClosed([&](auto&&, const winrt::TerminalApp::DetachedSessionEndedArgs& endedSession) {
                endedSessions.push_back({ endedSession.SessionId(), endedSession.State() });
            });

            tab->KeepRunning(true);
            page->_DetachShellPanesForKeepRunning(tab.get(), shellSessionId, 41);

            VERIFY_IS_TRUE(_contentManager->HasKeptSessions());
            const auto detachedSessions{ _contentManager->DetachedSessions() };
            VERIFY_ARE_EQUAL(1u, detachedSessions.Size());
            VERIFY_IS_TRUE(detachedSessions.GetAt(0).ShellSessionId() == shellSessionId);

            page->_NotifyPanesClosing(tab->GetRootPane());

            VERIFY_IS_TRUE(page->_panesKeptRunning.empty());
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_panesWithEmittedTerminalEndState.count(expectedPaneId)));
        });

        TestOnUIThread([&]() {
            const auto paneStates = _statesForPane(connectionStates, expectedPaneId);
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(paneStates.size()));

            VERIFY_IS_TRUE(_contentManager->DiscardKeptSession(sessionId));
        });

        TestOnUIThread([&]() {
            const auto paneStates = _statesForPane(connectionStates, expectedPaneId);
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(paneStates.size()));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(endedSessions.size()));
            VERIFY_IS_TRUE(!!::IsEqualGUID(endedSessions.at(0).sessionId, sessionId));
            VERIFY_IS_TRUE(endedSessions.at(0).state == L"closed");

            _contentManager->DetachedSessionClosed(closeToken);
        });
    }

    void TabTests::CloseProtocolLastPaneKeepsRunningWithoutEmittingEndState()
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

        const auto sessionId = ::Microsoft::Console::Utils::GuidFromString(L"{52a75f00-aaaa-bbbb-cccc-dddddddddddd}");
        TestOnUIThread([&]() {
            auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            auto connection = winrt::make_self<TestConnection>(
                sessionId,
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            VERIFY_IS_NOT_NULL(connection);

            const auto content = _contentManager->CreateCore(*settings, *settings, *connection);
            VERIFY_IS_NOT_NULL(content);

            NewTerminalArgs args{};
            args.ContentId(content.Id());
            VERIFY_SUCCEEDED(page->_OpenNewTab(args));

            const auto tab = page->_GetTabImpl(page->_tabs.GetAt(page->_tabs.Size() - 1));
            VERIFY_IS_NOT_NULL(tab);
            tab->SetDurableShellSession(L"existing-session", 8);
            tab->KeepRunning(true);
            page->_settings.GlobalSettings().FirstWindowPreference(FirstWindowPreference::PersistedLayout);
        });

        VERIFY_IS_TRUE(page->CloseProtocolPane(sessionId).get());

        TestOnUIThread([&]() {
            const auto detachedSessions = _contentManager->DetachedSessions();
            VERIFY_ARE_EQUAL(1u, detachedSessions.Size());
            VERIFY_IS_TRUE(!!::IsEqualGUID(sessionId, detachedSessions.GetAt(0).SessionId()));
            VERIFY_IS_TRUE(detachedSessions.GetAt(0).ShellSessionId() == L"existing-session");
        });
        VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(_statesForPane(connectionStates, _formatPaneId(sessionId)).size()));
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
            tab->KeepRunning(true);

            page->_SplitPane(nullptr, SplitDirection::Right, 0.5f, page->_MakePane(nullptr, page->_GetFocusedTab(), nullptr));
            VERIFY_ARE_EQUAL(2, tab->GetLeafPaneCount());

            const auto pane = tab->GetActivePane();
            VERIFY_IS_NOT_NULL(pane);
            const auto control = pane->GetTerminalControl();
            VERIFY_IS_NOT_NULL(control);
            closedPaneId = _formatPaneId(control.Connection().SessionId());

            page->_HandleClosePaneRequested(pane);

            VERIFY_ARE_EQUAL(1, tab->GetLeafPaneCount());
            VERIFY_IS_FALSE(_contentManager->HasKeptSessions());
        });

        const auto states = _statesForPane(connectionStates, closedPaneId);
        VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(states.size()));
        VERIFY_ARE_EQUAL(std::string{ "closed" }, states.at(0));
    }

    void TabTests::FocusProtocolShellSessionUsesDurableId()
    {
        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        const winrt::hstring durableId{ L"11111111-2222-3333-4444-555555555555" };
        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);
            tab->SetDurableShellSession(durableId, 1);
        });

        VERIFY_IS_TRUE(page->FocusProtocolShellSession(durableId).get());
        VERIFY_IS_FALSE(page->FocusProtocolShellSession(L"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").get());
    }

    void TabTests::ParseShellSessionSaveResponse()
    {
        const auto verifyInvalidJson = [](const std::string_view json) {
            VERIFY_THROWS_SPECIFIC(
                winrt::TerminalApp::implementation::TerminalPage::_ParseShellSessionSaveResponse(json),
                wil::ResultException,
                [](wil::ResultException& e) { return e.GetErrorCode() == WEB_E_INVALID_JSON_STRING; });
        };

        const auto parsed = winrt::TerminalApp::implementation::TerminalPage::_ParseShellSessionSaveResponse(R"({"id":"shell-session-7","revision":7,"forked":true})");
        VERIFY_IS_TRUE(parsed.id == L"shell-session-7");
        VERIFY_ARE_EQUAL(7LL, parsed.revision);
        VERIFY_IS_TRUE(parsed.forked);

        verifyInvalidJson(R"({"revision":7,"forked":true})");
        verifyInvalidJson(R"({"id":"shell-session-7","revision":7,"forked":true,})");
        verifyInvalidJson(R"({"id":"shell-session-7","revision":7,"forked":true}garbage)");
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
