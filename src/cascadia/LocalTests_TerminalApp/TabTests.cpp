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
#include "../TerminalApp/ContentTransfer.h"
#include "../TerminalSettingsAppAdapterLib/TerminalSettings.h"
#include "../TerminalApp/TerminalSettingsCache.h"
#include "../TerminalApp/TerminalPaneContent.h"
#include "../inc/AgentPaneRestore.h"
#include "../UnitTests_Control/MockControlSettings.h"
#include "CppWinrtTailored.h"

#include <winrt/Windows.UI.Xaml.Automation.h>

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
            _closeThreadId = GetCurrentThreadId();
            TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);
            ++_closeCount;
            _closedEvent.Set();
        }

        uint32_t CloseCount() const noexcept { return _closeCount.load(); }
        DWORD CloseThreadId() const noexcept { return _closeThreadId.load(); }
        bool WaitForClose(DWORD timeout = 10000) const noexcept
        {
            return WaitForSingleObject(_closedEvent.m_handle, timeout) == WAIT_OBJECT_0;
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
        std::atomic<uint32_t> _closeCount{ 0 };
        std::atomic<DWORD> _closeThreadId{ 0 };
        ::details::Event _closedEvent;
        winrt::guid _sessionId{};
        std::atomic<winrt::Microsoft::Terminal::TerminalConnection::ConnectionState> _state{
            winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::NotConnected
        };
    };

    struct TrackedAgentCore
    {
        explicit TrackedAgentCore(const winrt::TerminalApp::ContentManager& manager) :
            connection{ winrt::make_self<TestConnection>(winrt::guid{ L"{6239a42c-aaaa-49a3-80bd-e8fdd045185c}" },
                                                        winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected) }
        {
            const auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            core = manager.CreateCore(*settings, *settings, *connection);
            core.Closed([count = closeEvents](auto&&, auto&&) { ++*count; });
        }

        winrt::com_ptr<TestConnection> connection;
        winrt::Microsoft::Terminal::Control::ControlInteractivity core{ nullptr };
        std::shared_ptr<std::atomic<uint32_t>> closeEvents{ std::make_shared<std::atomic<uint32_t>>(0) };
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
            connectionStates.push_back({ evt["params"]["pane_id"].asString(),
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

    struct TransferLeafSnapshot
    {
        std::shared_ptr<Pane> pane;
        winrt::TerminalApp::IPaneContent content{ nullptr };
        winrt::Microsoft::Terminal::Control::TermControl control{ nullptr };
        winrt::Microsoft::Terminal::Control::ControlInteractivity core{ nullptr };
        winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection connection{ nullptr };
        uint64_t contentId{};
        std::shared_ptr<std::atomic<uint32_t>> closed{ std::make_shared<std::atomic<uint32_t>>(0) };
    };

    struct TransferTabSnapshot
    {
        winrt::com_ptr<winrt::TerminalApp::implementation::Tab> tab;
        std::shared_ptr<Pane> root;
        std::shared_ptr<Pane> activePane;
        winrt::hstring stableId;
        winrt::hstring actions;
        std::vector<TransferLeafSnapshot> leaves;
    };

    struct ContentTransferFixture
    {
        winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage> source;
        winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage> destination;
        Grid host{ nullptr };
        TransferTabSnapshot original;
        std::vector<TransferTabSnapshot> destinationTabs;
        winrt::TerminalApp::AgentPaneContent agent{ nullptr };
        uint64_t agentContentId{};
        uint64_t agentGeneration{};
        winrt::hstring agentRestoreIdentity;
        winrt::hstring agentCustomCommand{ L"\"C:\\Test Tools\\custom-agent.exe\" --acp" };
        winrt::hstring agentRestoreCommandline;
        bool hidden{};
        std::vector<winrt::com_ptr<TestConnection>> connections;
        std::shared_ptr<std::vector<Json::Value>> events{ std::make_shared<std::vector<Json::Value>>() };
    };

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
        TEST_METHOD(PaneContextPropagatesCaptureFailure);
        TEST_METHOD(AgentSessionRestoreRequiresPersistedBufferPath);
        TEST_METHOD(AgentPaneRestoreRecordRoundTrips);
        TEST_METHOD(PersistedLayoutAgentSessionsReceiveRestorePaths);
        TEST_METHOD(PaneAgentSessionBindingRequiresPaneIdentity);
        TEST_METHOD(AgentPaneRestoreDoesNotRequireAgentSession);
        TEST_METHOD(PaneAgentSessionEndClearsAgentBinding);
        TEST_METHOD(ContentIdAttachedPaneEmitsEndStateForItsConnection);
        TEST_METHOD(GetWindowLayoutIncludesAgentRestoreMetadata);
        TEST_METHOD(ResumedPaneIdentityPersistsWithoutHooksOrBannerParsing);
        TEST_METHOD(RestoredSessionBindingsWaitForTheirHelperSubscription);
        TEST_METHOD(LateRestoredSessionBindingNotifiesAfterPaneAttachment);
        TEST_METHOD(EndedRestoredSessionDoesNotReplayItsBinding);
        TEST_METHOD(AgentRestoreRecordOutlivesTheAgentPane);
        TEST_METHOD(KilledCliKeepsAgentBindingUntilPaneCloses);
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
        TEST_METHOD(BuildStartupActionsContentPreservesAgentFirstPaneOwnership);
        TEST_METHOD(AgentPaneTransferIdentityRoundTripsWithContent);
        TEST_METHOD(AgentPaneTransferIdentityIsNotPersistedByDefault);
        TEST_METHOD(BuildStartupActionsContentPreservesAgentLaterSplitOwnership);
        TEST_METHOD(BuildStartupActionsContentPreservesHiddenAgentIdentity);
        TEST_METHOD(TransferredAgentContentFirstPaneDefersTabRekey);
        TEST_METHOD(TransferredAgentFirstPaneCompletesHiddenTab);
        TEST_METHOD(TransferredAgentContentRejectsStaleAndFailedAttach);
        TEST_METHOD(AttachContentStopsAfterRejectedFirstAction);
        TEST_METHOD(ContentTransferAgentFirstExpiryRollsBack);
        TEST_METHOD(ContentTransferAgentLaterExpiryRollsBack);
        TEST_METHOD(ContentTransferAgentFirstSetupFailureRollsBack);
        TEST_METHOD(ContentTransferAgentLaterSetupFailureRollsBack);
        TEST_METHOD(ContentTransferAgentFirstInsertionFailureRollsBack);
        TEST_METHOD(ContentTransferAgentLaterInsertionFailureRollsBack);
        TEST_METHOD(ContentTransferAgentFirstLaterSplitFailureRollsBack);
        TEST_METHOD(ContentTransferAgentLaterLaterSplitFailureRollsBack);
        TEST_METHOD(ContentTransferFirstLayoutWaitsForReceiver);
        TEST_METHOD(ContentTransferReceiverWaitsForFirstLayout);
        TEST_METHOD(ContentTransferExpiredStartupClosesEmptyReceiver);
        TEST_METHOD(ContentTransferMissingMoveHandlerPreservesSource);
        TEST_METHOD(ContentTransferRejectedWindowPreservesSource);
        TEST_METHOD(ContentTransferReviewHiddenZoomedTabMovesToExistingPage);
        TEST_METHOD(ContentTransferReviewHiddenZoomedTabMovesToFreshReceiver);
        TEST_METHOD(ContentTransferReviewHiddenTabMovesWithoutZoom);
        TEST_METHOD(ContentTransferReviewVisibleZoomedTabMoves);
        TEST_METHOD(ContentTransferReviewHiddenZoomedNestedTabKeepsFinalFocus);
        TEST_METHOD(ContentTransferReviewRollbackPreservesScrollOffset);
        TEST_METHOD(ContentTransferReviewScrollOffsetWithoutTransferIsStable);
        TEST_METHOD(ContentTransferReviewSuccessfulMovePreservesScrollOffset);
        TEST_METHOD(ContentTransferReviewClosedAgentSuppressionMovesWithTab);
        TEST_METHOD(ContentTransferReviewSinglePaneKeepsDestinationSuppression);
        TEST_METHOD(ContentTransferReviewSinglePaneKeepsSuppressedDestination);
        TEST_METHOD(ClosingAgentPaneSuppressesPrewarm);
        TEST_METHOD(AgentPaneTeardownAllowsSynchronousRecreation);
        TEST_METHOD(AgentPaneLifetimeMovesAndDestroysRealCoresOnce);
        TEST_METHOD(AgentPaneLifetimeReceiveClosesRealCoreOnce);
        TEST_METHOD(AgentPaneLifetimeFailedReceiveRetiresRealCore);
        TEST_METHOD(AgentPaneLifetimeTimeoutRetiresOnlyAbandonedRealCore);
        TEST_METHOD(SourceTerminalPaneSkipsAgentPane);
        TEST_METHOD(TransferredAgentContentSplitPaneRetiresDestinationAgentPane);
        TEST_METHOD(TransferredAgentStatusReplaysMissedTabRekey);
        TEST_METHOD(AgentPaneIndicatorsIgnoreAgentPaneInPaneCount);
        TEST_METHOD(AgentPaneIndicatorsIgnoreNonTerminalPanes);
        TEST_METHOD(AgentPaneIndicatorsRefreshAfterNonActivePaneClose);
        TEST_METHOD(PendingAgentOpenSurvivesStartupProjection);
        TEST_METHOD(InitialSessionsViewSurvivesStartupProjection);
        TEST_METHOD(AgentReadyRuntimeConfigIncludesCurrentYoloState);

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
        using TransferStage = winrt::TerminalApp::implementation::TerminalPage::ContentTransferStage;
        static winrt::TerminalApp::implementation::SharedWtaLease _acquireIsolatedAgentLease();
        std::unique_ptr<ContentTransferFixture> _createContentTransferFixture(bool agentFirst, bool hidden, bool freshReceiver = false, bool twoLeaves = false, std::optional<int32_t> historySize = std::nullopt);
        void _verifyContentTransferReviewZoom(bool hidden, bool zoomed, bool freshReceiver, bool twoLeaves = true);
        void _verifyContentTransferReviewScroll(bool transfer, bool reject = true);
        void _verifyContentTransferReviewSuppression(bool singlePane, bool destinationSuppressed = false);
        void _waitForContentTransferReviewUI(const std::function<bool()>& predicate);
        TransferTabSnapshot _snapshotTransferTab(const winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage>& page,
                                               const winrt::com_ptr<winrt::TerminalApp::implementation::Tab>& tab);
        void _verifyTransferTabUnchanged(const winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage>& page,
                                        const TransferTabSnapshot& snapshot);
        void _verifyTransferRollback(const ContentTransferFixture& fixture);
        void _verifyTransferCommitted(const ContentTransferFixture& fixture);
        void _verifyTransferredAgentRestoreState(const ContentTransferFixture& fixture,
                                                const winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage>& page,
                                                const winrt::com_ptr<winrt::TerminalApp::implementation::Tab>& tab);
        void _closeContentTransferFixture(ContentTransferFixture& fixture, bool verify);
        winrt::TerminalApp::RequestMoveContentArgs _requestContentTransfer(const ContentTransferFixture& fixture);
        void _verifyContentTransferFailure(bool agentFirst, bool hidden, TransferStage stage);
        void _verifyContentTransferStartupGate(bool layoutFirst);
        void _verifyContentTransferSourceRejection(bool rejectWindow);
        NewTerminalArgs _storeOwnedAgentTransfer(
            const winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage>& page,
            const TrackedAgentCore& tracked);
        void _verifyBuildStartupActionsContentPreservesAgentOwnership(SplitDirection splitDirection,
                                                                     bool hidden = false);
        void _initializeTerminalPage(winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage>& page,
                                     CascadiaSettings initialSettings,
                                     winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection connection = nullptr,
                                     Grid layoutHost = nullptr);
        void _createContentManager();
        winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage> _commonSetup(
            winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection connection = nullptr,
            Grid layoutHost = nullptr,
            std::optional<int32_t> historySize = std::nullopt);
        winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage> _restoreBindingsSetup();
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
        namespace Restore = ::Microsoft::Terminal::AgentPaneRestore;

        // A shell pane running an agent CLI persists as that agent's resume
        // invocation in the ordinary `commandline`, so recognising one is what
        // tells the restore to skip seeding the saved scrollback.
        const auto resume = Restore::BuildResumeCommandline(L"claude", L"agent-session-1");
        VERIFY_ARE_EQUAL(std::wstring{ LR"(cmd.exe /d /s /c "claude --resume agent-session-1")" }, resume);
        VERIFY_IS_TRUE(Restore::IsResumeCommandline(resume));

        const auto target = Restore::ParseResumeCommandline(resume);
        VERIFY_ARE_EQUAL(std::wstring{ L"claude" }, target.agent);
        VERIFY_ARE_EQUAL(std::wstring{ L"agent-session-1" }, target.sessionId);

        // An ordinary shell must not be mistaken for one.
        VERIFY_IS_FALSE(Restore::IsResumeCommandline(L"pwsh.exe"));
        VERIFY_IS_FALSE(Restore::IsResumeCommandline(L""));

        // A session id is validated before it lands inside a command line that
        // gets executed, and an unknown agent has no resume spelling at all.
        VERIFY_IS_TRUE(Restore::BuildResumeCommandline(L"claude", L"bad id & calc.exe").empty());
        VERIFY_IS_TRUE(Restore::BuildResumeCommandline(L"claude", L"sidekick-1").empty());
        VERIFY_IS_TRUE(Restore::BuildResumeCommandline(L"nosuchagent", L"agent-session-1").empty());

        // Every built-in agent whose `resume_flag` is non-empty in
        // `tools/wta/src/agent_registry.rs` has to be spellable here, or a
        // shell pane running it comes back as a bare prompt. OpenCode is the
        // one that does not use `--resume`.
        VERIFY_ARE_EQUAL(
            std::wstring{ LR"(cmd.exe /d /s /c "opencode --session agent-session-1")" },
            Restore::BuildResumeCommandline(L"opencode", L"agent-session-1"));
        for (const auto agent : { L"copilot", L"claude", L"codex", L"gemini", L"opencode" })
        {
            const auto built = Restore::BuildResumeCommandline(agent, L"agent-session-1");
            VERIFY_IS_FALSE(built.empty());
            VERIFY_ARE_EQUAL(std::wstring{ agent }, Restore::ParseResumeCommandline(built).agent);
        }
    }

    void TabTests::AgentPaneRestoreRecordRoundTrips()
    {
        namespace Restore = ::Microsoft::Terminal::AgentPaneRestore;

        // The record is written as a command line and read back with
        // `CommandLineToArgvW`, so every value has to survive that parser.
        // Backslashes are the trap: it only treats them specially when they run
        // into a quote, so doubling all of them turns an ordinary Windows path
        // into one with doubled separators.
        Restore::Fields written;
        written.sessionId = L"agent-session-1";
        written.view = Restore::ChatView;
        written.agentIdentity = L"wsl:Ubuntu-22.04:claude";
        written.customCommand = LR"(C:\tools\my agent\agent.exe --acp --note "hi" --trailing C:\dir\)";
        written.yoloControlOwner = L"manual";

        const auto commandline = Restore::BuildPaneCommandline(LR"(C:\Program Files\wta.exe)", written);

        auto argc = 0;
        const wil::unique_hlocal_ptr<PWSTR[]> argv{ ::CommandLineToArgvW(commandline.c_str(), &argc) };
        VERIFY_IS_NOT_NULL(argv.get());
        std::vector<std::wstring> tokens;
        for (auto i = 0; i < argc; ++i)
        {
            tokens.emplace_back(argv[i]);
        }

        VERIFY_ARE_EQUAL(std::wstring{ LR"(C:\Program Files\wta.exe)" }, tokens.at(0));

        const auto read = Restore::ParsePaneCommandline(tokens);
        VERIFY_ARE_EQUAL(written.sessionId, read.sessionId);
        VERIFY_ARE_EQUAL(written.view, read.view);
        VERIFY_ARE_EQUAL(written.agentIdentity, read.agentIdentity);
        VERIFY_ARE_EQUAL(written.customCommand, read.customCommand);
        VERIFY_ARE_EQUAL(written.yoloControlOwner, read.yoloControlOwner);

        // Empty fields are simply absent rather than round-tripping as `""`.
        Restore::Fields sparse;
        sparse.sessionId = L"only-a-session";
        const auto sparseCmd = Restore::BuildPaneCommandline(L"wta.exe", sparse);
        VERIFY_IS_TRUE(sparseCmd.find(Restore::ViewFlag) == std::wstring::npos);
        VERIFY_IS_TRUE(sparseCmd.find(Restore::CustomCommandFlag) == std::wstring::npos);
        VERIFY_IS_TRUE(sparseCmd.find(Restore::YoloControlOwnerFlag) == std::wstring::npos);
    }

    void TabTests::PersistedLayoutAgentSessionsReceiveRestorePaths()
    {
        namespace Restore = ::Microsoft::Terminal::AgentPaneRestore;

        // The agent pane's saved command line carries only what a restart
        // cannot re-derive. Everything else — the master pipe, the owner ids,
        // the resolved CLI path — is rebuilt by the spawn path, so none of it
        // may appear here.
        Restore::Fields fields;
        fields.sessionId = L"acp-1";
        fields.view = Restore::SessionsView;
        fields.agentIdentity = L"wsl:Ubuntu:claude";
        fields.customCommand = L"custom-acp --stdio";

        const auto commandline = Restore::BuildPaneCommandline(L"C:\\wta.exe", fields);
        VERIFY_IS_TRUE(commandline.find(L"--connect-master") == std::wstring::npos);
        VERIFY_IS_TRUE(commandline.find(L"--owner-tab-id") == std::wstring::npos);

        auto argc = 0;
        const wil::unique_hlocal_ptr<PWSTR[]> argv{ ::CommandLineToArgvW(commandline.c_str(), &argc) };
        VERIFY_IS_NOT_NULL(argv.get());
        std::vector<std::wstring> tokens;
        for (auto i = 0; i < argc; ++i)
        {
            tokens.emplace_back(argv[i]);
        }

        const auto parsed = Restore::ParsePaneCommandline(tokens);
        VERIFY_ARE_EQUAL(fields.sessionId, parsed.sessionId);
        VERIFY_ARE_EQUAL(fields.view, parsed.view);
        VERIFY_ARE_EQUAL(fields.agentIdentity, parsed.agentIdentity);
        VERIFY_ARE_EQUAL(fields.customCommand, parsed.customCommand);
    }

    void TabTests::PaneContextPropagatesCaptureFailure()
    {
        const auto connection = winrt::make_self<TestConnection>(
            winrt::guid{ L"{62a75f00-aaaa-bbbb-cccc-dddddddddddd}" },
            winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
        auto page = _commonSetup(*connection);
        VERIFY_IS_NOT_NULL(page);

        winrt::guid sessionId{};
        winrt::TerminalApp::TerminalPage projectedPage{ nullptr };
        TestOnUIThread([&]() {
            projectedPage = *page;
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);
            const auto control = tab->GetActivePane()->GetTerminalControl();
            VERIFY_IS_NOT_NULL(control);
            sessionId = control.Connection().SessionId();
            VERIFY_ARE_NOT_EQUAL(winrt::guid{}, sessionId);
        });

        for (const auto explicitSource : { true, false })
        {
            winrt::Windows::Foundation::IAsyncOperation<winrt::Microsoft::Terminal::Protocol::PaneContext> operation{ nullptr };
            TestOnUIThread([&]() {
                operation = projectedPage.GetProtocolPaneContext(explicitSource ? sessionId : winrt::guid{}, explicitSource, 0, 100);
            });
            VERIFY_ARE_EQUAL(sessionId, operation.get().Pane.SessionId);

            // Bypass COM's argument validation to make both bounded readers reject
            // their zero-line budget. A read failure must not become "pane not found".
            TestOnUIThread([&]() {
                operation = projectedPage.GetProtocolPaneContext(explicitSource ? sessionId : winrt::guid{}, explicitSource, -1, 100);
            });
            VERIFY_THROWS_SPECIFIC(
                operation.get(),
                winrt::hresult_error,
                [](const winrt::hresult_error& error) { return error.code() == E_INVALIDARG; });
        }
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
        namespace Restore = ::Microsoft::Terminal::AgentPaneRestore;

        // An agent pane the user opened but never chatted in has no ACP
        // session id, yet it must still come back — its content type says it is
        // an agent pane regardless, and the stashed variant carries the one bit
        // saying the user had toggled it away.
        VERIFY_IS_TRUE(Restore::IsPaneType(Restore::PaneType));
        VERIFY_IS_TRUE(Restore::IsPaneType(Restore::StashedPaneType));
        VERIFY_IS_FALSE(Restore::IsPaneType(L""));
        VERIFY_IS_FALSE(Restore::IsPaneType(L"settings"));

        Restore::Fields empty;
        empty.view = Restore::ChatView;
        const auto commandline = Restore::BuildPaneCommandline(L"wta.exe", empty);
        VERIFY_IS_TRUE(commandline.find(Restore::SessionIdFlag) == std::wstring::npos);
        VERIFY_IS_TRUE(commandline.find(Restore::ViewFlag) != std::wstring::npos);
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

    // A pane built by attaching an existing ContentId reports its end state
    // against the SessionId of the connection it attached to, not the one the
    // action happened to carry. The two differ on a cross-window move, where
    // the args name a fresh pane while the content keeps the original
    // connection — attributing the event to the wrong id would route it to a
    // pane that no longer exists.
    void TabTests::ContentIdAttachedPaneEmitsEndStateForItsConnection()
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

            const auto attachedPane = page->_MakeTerminalPane(args);
            VERIFY_IS_NOT_NULL(attachedPane);
        });

        TestOnUIThread([&]() {
            connection->TransitionTo(winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed);
        });

        TestOnUIThread([&]() {
            const auto closedStates = _statesForPane(connectionStates, _formatPaneId(liveSessionId));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(closedStates.size()));
            VERIFY_ARE_EQUAL(std::string{ "closed" }, closedStates.at(0));

            // Nothing is attributed to the id the action carried.
            VERIFY_ARE_EQUAL(
                0u,
                static_cast<unsigned int>(_statesForPane(connectionStates, _formatPaneId(fallbackSessionId)).size()));
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
                VERIFY_ARE_EQUAL(
                    winrt::hstring{ ::Microsoft::Terminal::AgentPaneRestore::BuildResumeCommandline(
                        firstBinding->second.agent,
                        firstBinding->second.sessionId) },
                    firstTerminalArgs.Commandline());
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
                VERIFY_ARE_EQUAL(
                    winrt::hstring{ ::Microsoft::Terminal::AgentPaneRestore::BuildResumeCommandline(
                        secondBinding->second.agent,
                        secondBinding->second.sessionId) },
                    secondTerminalArgs.Commandline());
            }
            else
            {
                VERIFY_FAIL(L"Expected the split pane to keep its agent session metadata.");
            }
        });
    }

    void TabTests::ResumedPaneIdentityPersistsWithoutHooksOrBannerParsing()
    {
        auto page = _restoreBindingsSetup();
        TestOnUIThread([&]() {
            const std::array launchCommands{
                winrt::hstring{ L"cmd /c echo Loading... && copilot" },
                winrt::hstring{ L"copilot" }
            };
            const std::array sessionIds{
                winrt::hstring{ L"known-session-1" },
                winrt::hstring{ L"known-session-2" }
            };
            std::vector<ActionAndArgs> actions;
            for (uint32_t i = 0; i < launchCommands.size(); ++i)
            {
                const auto tab = page->_GetTabImpl(page->_tabs.GetAt(i));
                const auto paneId = tab->GetActiveTerminalControl().Connection().SessionId();
                Json::Value event;
                event["type"] = "event";
                event["method"] = "pane_agent_session_changed";
                event["params"]["pane_id"] = _formatPaneId(paneId);
                event["params"]["agent"] = "copilot";
                event["params"]["agent_session_id"] = winrt::to_string(sessionIds[i]);
                page->OnPaneAgentSessionChanged(winrt::to_hstring(Json::writeString(Json::StreamWriterBuilder{}, event)));

                NewTerminalArgs terminalArgs{};
                terminalArgs.SessionId(paneId);
                terminalArgs.Commandline(launchCommands[i]);
                if (i == 0)
                {
                    actions.emplace_back(ShortcutAction::NewTab, NewTabArgs{ terminalArgs });
                }
                else
                {
                    actions.emplace_back(ShortcutAction::SplitPane, SplitPaneArgs{ SplitDirection::Right, 0.5f, terminalArgs });
                }
            }

            // A launch string alone must not invent a binding for another pane.
            NewTerminalArgs unbound{};
            unbound.SessionId(::Microsoft::Console::Utils::CreateGuid());
            unbound.Commandline(L"cmd /c echo Resuming copilot session display-... && copilot --resume display-id");
            actions.emplace_back(ShortcutAction::NewTab, NewTabArgs{ unbound });
            VERIFY_ARE_EQUAL(2u, static_cast<unsigned int>(page->_paneAgentSessions.size()));

            page->_StampAgentResumeCommandlines(actions);
            WindowLayout layout{};
            layout.TabLayout(winrt::single_threaded_vector<ActionAndArgs>(std::move(actions)));
            const auto saved = WindowLayout::FromJson(WindowLayout::ToJson(layout)).TabLayout();
            VERIFY_ARE_EQUAL(3u, saved.Size());
            VERIFY_ARE_EQUAL(
                winrt::hstring{ LR"(cmd.exe /d /s /c "copilot --resume known-session-1")" },
                _getTerminalArgs(saved.GetAt(0)).Commandline());
            VERIFY_ARE_EQUAL(
                winrt::hstring{ LR"(cmd.exe /d /s /c "copilot --resume known-session-2")" },
                _getTerminalArgs(saved.GetAt(1)).Commandline());
            VERIFY_ARE_EQUAL(unbound.Commandline(), _getTerminalArgs(saved.GetAt(2)).Commandline());
        });
    }

    winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage> TabTests::_restoreBindingsSetup()
    {
        _createContentManager();
        winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage> page;
        TestOnUIThread([&]() {
            _windowProperties = winrt::make_self<winrt::TerminalApp::implementation::WindowProperties>();
            const winrt::TerminalApp::TerminalPage projected{ *_windowProperties, *_contentManager };
            page.copy_from(winrt::get_self<winrt::TerminalApp::implementation::TerminalPage>(projected));
            page->_isVerticalLayout = false;
            page->_tabView = winrt::MUX::Controls::TabView{};
            // These tests need pane identity, not a real shell process or the
            // application's first-run/startup UI.
            for (auto i = 0; i < 2; ++i)
            {
                const auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
                const auto connection = winrt::make_self<TestConnection>(
                    ::Microsoft::Console::Utils::CreateGuid(),
                    winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
                const winrt::Microsoft::Terminal::Control::TermControl control{ *settings, *settings, *connection };
                const auto content = winrt::make<winrt::TerminalApp::implementation::TerminalPaneContent>(
                    Profile{},
                    std::shared_ptr<winrt::TerminalApp::implementation::TerminalSettingsCache>{},
                    control);
                const auto tab = winrt::make_self<winrt::TerminalApp::implementation::Tab>(
                    std::make_shared<Pane>(content));
                page->_tabs.Append(*tab);
            }
        });
        return page;
    }

    void TabTests::RestoredSessionBindingsWaitForTheirHelperSubscription()
    {
        auto page = _restoreBindingsSetup();
        VERIFY_IS_NOT_NULL(page);

        TestOnUIThread([&]() {
            const auto firstTab = page->_GetTabImpl(page->_tabs.GetAt(0));
            const auto secondTab = page->_GetTabImpl(page->_tabs.GetAt(1));
            const auto firstPane = firstTab->GetActiveTerminalControl().Connection().SessionId();
            const auto secondPane = secondTab->GetActiveTerminalControl().Connection().SessionId();
            page->_pendingRestoredSessionBindings[firstPane] = { L"restored-first", L"copilot", L"C:\\repo" };
            page->_pendingRestoredSessionBindings[secondPane] = { L"restored-second", L"claude", L"C:\\other" };

            std::vector<Json::Value> births;
            const auto token = page->ProtocolVtSequenceReceived([&](auto&&, const winrt::hstring& payload) {
                Json::Value event;
                Json::CharReaderBuilder reader;
                std::string errors;
                std::istringstream stream{ winrt::to_string(payload) };
                VERIFY_IS_TRUE(Json::parseFromStream(reader, stream, &event, &errors));
                if (event["method"] == "session_born_bound")
                {
                    births.push_back(event["params"]);
                }
            });
            const auto revoke = wil::scope_exit([&]() { page->ProtocolVtSequenceReceived(token); });
            // Availability before subscription must not consume either binding.
            page->_NotifyRestoredSessionBindings(firstTab);
            page->_NotifyRestoredSessionBindings(secondTab);
            VERIFY_IS_TRUE(births.empty());
            const auto request = [&](const auto& tab, const std::string& windowId) {
                Json::Value event;
                event["method"] = "pane_agent_session_changed";
                event["params"]["event"] = "restore_bindings_requested";
                event["params"]["tab_id"] = winrt::to_string(tab->StableId());
                event["params"]["window_id"] = windowId;
                page->OnPaneAgentSessionChanged(winrt::to_hstring(Json::writeString(Json::StreamWriterBuilder{}, event)));
            };
            const auto windowId = std::to_string(page->_WindowProperties.WindowId());
            request(firstTab, "wrong-window");
            VERIFY_IS_TRUE(births.empty());
            VERIFY_ARE_EQUAL(2u, static_cast<unsigned int>(page->_pendingRestoredSessionBindings.size()));

            page->_startupActionReplayDepth = 2;
            request(firstTab, windowId);
            VERIFY_IS_TRUE(births.empty());
            VERIFY_IS_TRUE(page->_tabsAwaitingRestoredBindings.contains(firstTab->StableId()));
            page->_startupActionReplayDepth = 1;
            page->ProcessStartupActions({});
            VERIFY_IS_TRUE(births.empty());
            page->_startupActionReplayDepth = 0;
            page->ProcessStartupActions({});
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(births.size()));
            VERIFY_ARE_EQUAL(std::string{ "restored-first" }, births[0]["agent_session_id"].asString());
            VERIFY_ARE_EQUAL(winrt::to_string(::Microsoft::Console::Utils::GuidToPlainString(firstPane)), births[0]["pane_id"].asString());
            VERIFY_ARE_EQUAL(winrt::to_string(firstTab->StableId()), births[0]["tab_id"].asString());
            VERIFY_ARE_EQUAL(windowId, births[0]["window_id"].asString());
            VERIFY_ARE_EQUAL(std::string{ "C:\\repo" }, births[0]["cwd"].asString());
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_pendingRestoredSessionBindings.size()));

            request(firstTab, windowId);
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(births.size()));
            request(secondTab, windowId);
            VERIFY_ARE_EQUAL(2u, static_cast<unsigned int>(births.size()));
            VERIFY_ARE_EQUAL(std::string{ "restored-second" }, births[1]["agent_session_id"].asString());
            VERIFY_IS_TRUE(page->_pendingRestoredSessionBindings.empty());
        });
    }

    void TabTests::LateRestoredSessionBindingNotifiesAfterPaneAttachment()
    {
        auto page = _restoreBindingsSetup();
        TestOnUIThread([&]() {
            const auto tab = page->_GetTabImpl(page->_tabs.GetAt(0));
            const auto otherTab = page->_GetTabImpl(page->_tabs.GetAt(1));
            const auto windowId = std::to_string(page->_WindowProperties.WindowId());
            const auto request = [&]() {
                Json::Value event;
                event["params"]["event"] = "restore_bindings_requested";
                event["params"]["tab_id"] = winrt::to_string(tab->StableId());
                event["params"]["window_id"] = windowId;
                page->OnPaneAgentSessionChanged(winrt::to_hstring(Json::writeString(Json::StreamWriterBuilder{}, event)));
            };
            // The listener's initial handshake has already completed without
            // any bindings. There will be no second listener-ready event.
            request();
            const auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
            const auto paneId = ::Microsoft::Console::Utils::CreateGuid();
            const auto connection = winrt::make_self<TestConnection>(
                paneId, winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            const winrt::Microsoft::Terminal::Control::TermControl control{ *settings, *settings, *connection };
            const auto content = winrt::make<winrt::TerminalApp::implementation::TerminalPaneContent>(
                Profile{}, std::shared_ptr<winrt::TerminalApp::implementation::TerminalSettingsCache>{}, control);
            const auto pane = std::make_shared<Pane>(content);
            page->_pendingRestoredSessionBindings[paneId] = { L"late-session", L"copilot", L"C:\\repo" };
            unsigned int available = 0;
            unsigned int births = 0;
            const auto token = page->ProtocolVtSequenceReceived([&](auto&&, const winrt::hstring& payload) {
                Json::Value event;
                Json::CharReaderBuilder reader;
                std::string errors;
                std::istringstream stream{ winrt::to_string(payload) };
                VERIFY_IS_TRUE(Json::parseFromStream(reader, stream, &event, &errors));
                if (event["method"] == "restore_bindings_available")
                {
                    ++available;
                    VERIFY_ARE_EQUAL(winrt::to_string(tab->StableId()), event["params"]["tab_id"].asString());
                    VERIFY_ARE_EQUAL(windowId, event["params"]["window_id"].asString());
                    VERIFY_IS_TRUE(tab->GetRootPane()->FindPaneBySessionId(paneId) != nullptr);
                    request();
                }
                else if (event["method"] == "session_born_bound")
                {
                    ++births;
                    VERIFY_ARE_EQUAL(std::string{ "late-session" }, event["params"]["agent_session_id"].asString());
                }
            });
            const auto revoke = wil::scope_exit([&]() { page->ProtocolVtSequenceReceived(token); });
            page->_NotifyRestoredSessionBindings(tab);
            page->_NotifyRestoredSessionBindings(otherTab);
            VERIFY_ARE_EQUAL(0u, available);
            page->_tabContent = winrt::WUX::Controls::Grid{};
            page->_tabContent.Measure({ 1000, 1000 });
            page->_tabContent.Arrange({ 0, 0, 1000, 1000 });
            page->_SplitPane(tab, SplitDirection::Right, 0.5f, pane, false);
            VERIFY_ARE_EQUAL(1u, available);
            VERIFY_ARE_EQUAL(1u, births);
            VERIFY_IS_TRUE(page->_pendingRestoredSessionBindings.empty());
            request();
            page->_NotifyRestoredSessionBindings(tab);
            VERIFY_ARE_EQUAL(1u, available);
            VERIFY_ARE_EQUAL(1u, births);
        });
    }

    void TabTests::EndedRestoredSessionDoesNotReplayItsBinding()
    {
        auto page = _restoreBindingsSetup();
        VERIFY_IS_NOT_NULL(page);
        TestOnUIThread([&]() {
            const auto tab = page->_GetTabImpl(page->_tabs.GetAt(0));
            const auto pane = tab->GetActiveTerminalControl().Connection().SessionId();
            const auto paneId = winrt::to_string(::Microsoft::Console::Utils::GuidToString(pane));
            for (const auto state : { "closed", "failed" })
            {
                page->_pendingRestoredSessionBindings[pane] = { L"restored-session", L"copilot", L"C:\\repo" };
                page->_TryRaiseTerminalEndStateEvent(paneId, state);
                VERIFY_IS_TRUE(page->_pendingRestoredSessionBindings.empty());
            }
        });
    }

    // The agent pane used to be `closeOnExit: always`, so a helper that died
    // took its pane — and everything a restore needed — with it. It is now
    // `graceful`, which keeps the pane after a killed helper exactly like every
    // other profile, so the pane is still in the tree for a save to serialize.
    void TabTests::AgentRestoreRecordOutlivesTheAgentPane()
    {
        // Deliberately harness-free: this is a settings assertion, and going
        // through the page would tie it to `_initializeTerminalPage`.
        const auto settings = winrt::Microsoft::Terminal::Settings::Model::CascadiaSettings::LoadDefaults();
        VERIFY_IS_NOT_NULL(settings);

        winrt::Microsoft::Terminal::Settings::Model::Profile agentProfile{ nullptr };
        for (const auto& profile : settings.AllProfiles())
        {
            if (profile.Name() == L"Agent Pane")
            {
                agentProfile = profile;
                break;
            }
        }
        VERIFY_IS_NOT_NULL(agentProfile);
        VERIFY_ARE_EQUAL(
            winrt::Microsoft::Terminal::Settings::Model::CloseOnExitMode::Graceful,
            agentProfile.CloseOnExit());
    }

    // A shell pane survives its CLI being killed (`closeOnExit` only closes on a
    // graceful exit), so the binding that says how to resume that CLI has to
    // survive with it — right up until the pane itself closes.
    void TabTests::KilledCliKeepsAgentBindingUntilPaneCloses()
    {
        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);
            const auto rootPane = tab->GetRootPane();
            VERIFY_IS_NOT_NULL(rootPane);
            const auto control = rootPane->GetTerminalControl();
            VERIFY_IS_NOT_NULL(control);

            const auto paneSessionId = control.Connection().SessionId();
            const auto paneId = winrt::to_string(::Microsoft::Console::Utils::GuidToString(paneSessionId));

            page->_paneAgentSessions.clear();
            page->_panesWithEmittedTerminalEndState.clear();
            auto& binding = page->_paneAgentSessions[paneSessionId];
            binding.sessionId = L"agent-session-killed";
            binding.agent = L"copilot";
            binding.resumeCommandline = L"copilot --resume agent-session-killed";

            // The CLI was killed, not ended. The pane is still here, so the
            // binding must be too.
            page->_TryRaiseTerminalEndStateEvent(paneId, "failed");
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_paneAgentSessions.count(paneSessionId)));

            // The pane going away is what finally retires it.
            page->_NotifyPanesClosing(rootPane);
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_paneAgentSessions.count(paneSessionId)));
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
        // The resume rides the `commandline` every pane already persists, so
        // there is no agent-specific key in state.json to check.
        VERIFY_ARE_EQUAL(
            winrt::hstring{ ::Microsoft::Terminal::AgentPaneRestore::BuildResumeCommandline(
                L"copilot",
                L"agent-session-persisted") },
            terminalArgs.Commandline());
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
    // - connection: optional in-process connection for protocol tests that do not need window layout.
    // Return Value:
    // - <none>
    void TabTests::_initializeTerminalPage(winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage>& page,
                                           CascadiaSettings initialSettings,
                                           winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection connection,
                                           Grid layoutHost)
    {
        // A fresh TestHostApp has not completed onboarding. Its first layout
        // otherwise defers tab creation but still raises Initialized.
        const auto applicationState = ApplicationState::SharedInstance();
        const auto freCompleted = applicationState.AgentFreCompleted();
        const auto restoreFreCompleted = wil::scope_exit([&]() {
            applicationState.AgentFreCompleted(freCompleted);
        });
        applicationState.AgentFreCompleted(true);

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
            // Match SetSettings' first-load cache without its external
            // shell-integration reconciliation side effects.
            page->_terminalSettingsCache = std::make_shared<winrt::TerminalApp::implementation::TerminalSettingsCache>(initialSettings);
        });
        VERIFY_SUCCEEDED(result);

        VERIFY_IS_NOT_NULL(page);
        VERIFY_IS_NOT_NULL(page->_settings);

        ::details::Event waitForInitEvent;
        if (!waitForInitEvent.IsValid())
        {
            VERIFY_SUCCEEDED(HRESULT_FROM_WIN32(::GetLastError()));
        }
        if (!connection)
        {
            page->Initialized([&waitForInitEvent](auto&&, auto&&) {
                waitForInitEvent.Set();
            });
        }

        Log::Comment(L"Create() the TerminalPage");

        result = RunOnUIThread([&page, connection, layoutHost, this]() {
            VERIFY_IS_NOT_NULL(page);
            VERIFY_IS_NOT_NULL(page->_settings);
            page->Create();
            Log::Comment(L"Create()'d the page successfully");

            // Build a NewTab action, to make sure we start with one. The real
            // Terminal will always get one from AppCommandlineArgs.
            NewTerminalArgs newTerminalArgs{};
            if (connection)
            {
                const auto settings = winrt::make_self<ControlUnitTests::MockControlSettings>();
                const auto content = _contentManager->CreateCore(*settings, *settings, connection);
                newTerminalArgs.ContentId(content.Id());
                VERIFY_SUCCEEDED(page->_OpenNewTab(newTerminalArgs));
            }
            else
            {
                NewTabArgs args{ newTerminalArgs };
                ActionAndArgs newTabAction{ ShortcutAction::NewTab, args };
                // push the arg onto the front
                page->_startupActions.push_back(std::move(newTabAction));
            }
            Log::Comment(L"Added a single newTab action");

            if (connection)
            {
                // Protocol queries need a real tab/control, but not rendered-window startup.
                return;
            }

            auto app = ::winrt::Windows::UI::Xaml::Application::Current();

            winrt::TerminalApp::TerminalPage pp = *page;
            if (layoutHost)
            {
                layoutHost.Children().Append(pp);
                winrt::Windows::UI::Xaml::Window::Current().Content(layoutHost);
            }
            else
            {
                winrt::Windows::UI::Xaml::Window::Current().Content(pp);
            }
            winrt::Windows::UI::Xaml::Window::Current().Activate();
        });
        VERIFY_SUCCEEDED(result);

        if (!connection)
        {
            Log::Comment(L"Wait for the page to finish initializing...");
            VERIFY_SUCCEEDED(waitForInitEvent.Wait());
            Log::Comment(L"...Done");
        }

        result = RunOnUIThread([&page]() {
            // In the real app, this isn't a problem, but doesn't happen
            // reliably in the unit tests.
            Log::Comment(L"Ensure we set the first tab as the selected one.");
            VERIFY_ARE_EQUAL(1u, page->_tabs.Size());
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
    winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage> TabTests::_commonSetup(
        winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection connection,
        Grid layoutHost,
        std::optional<int32_t> historySize)
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

        if (historySize)
        {
            settings0.AllProfiles().GetAt(0).HistorySize(*historySize);
        }

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
        _initializeTerminalPage(page, settings0, connection, layoutHost);

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

            auto transferredSourcePane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            VERIFY_IS_NOT_NULL(transferredSourcePane);
            const auto transferredControl = transferredSourcePane->GetTerminalControl();
            VERIFY_IS_NOT_NULL(transferredControl);
            const auto contentId = transferredControl.ContentId();
            const auto newTerminalArgs = transferredSourcePane->GetContent().GetNewTerminalArgs(BuildStartupKind::Content).as<NewTerminalArgs>();
            const auto transferId = newTerminalArgs.AgentPaneTransferId();
            page->_manager.Detach(transferredControl);
            using DragStash = winrt::TerminalApp::implementation::AgentPaneDragStash;
            DragStash::Entry entry;
            entry.originalTabId = oldTabId;
            entry.sourceProfileGuid = sourceProfileGuid;
            entry.attachDisposition = DragStash::AttachDisposition::FirstPaneOfNewTab;
            entry.hidden = true;
            entry.sessionsView = true;
            entry.panePosition = L"left";
            entry.transferId = transferId;
            DragStash::Instance().Store(contentId, std::move(entry));
            auto cleanup = wil::scope_exit([&]() {
                DragStash::Instance().Take(contentId, transferId);
            });

            auto transferredPane = page->_MakeTerminalPane(newTerminalArgs, nullptr, nullptr);
            VERIFY_IS_NOT_NULL(transferredPane);
            VERIFY_IS_TRUE(transferredPane->IsAgentPane());

            VERIFY_IS_TRUE(focusedTab->FindAgentPane() != nullptr);
            VERIFY_IS_FALSE(focusedTab->AgentSourceProfileGuid().has_value());

            const auto agentContent = transferredPane->GetContent().try_as<winrt::TerminalApp::AgentPaneContent>();
            VERIFY_IS_NOT_NULL(agentContent);
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(agentContent);
            VERIFY_ARE_EQUAL(contentId, transferredPane->GetTerminalControl().ContentId());
            VERIFY_ARE_NOT_EQUAL(transferId, impl->TransferId());
            VERIFY_IS_TRUE(impl->AwaitingTransferredTabContent());
            VERIFY_IS_TRUE(impl->TakeHiddenAfterTransfer());
            VERIFY_IS_FALSE(impl->TakeHiddenAfterTransfer());
            VERIFY_IS_TRUE(impl->IsSessionsView());
            VERIFY_IS_TRUE(impl->GetAgentPanePosition() == L"left");
            VERIFY_IS_FALSE(DragStash::Instance().Take(contentId, transferId).has_value());
            VERIFY_IS_TRUE(impl->TakePendingRenameFromTabId() == oldTabId);
            VERIFY_IS_TRUE(impl->TransferSourceTabId() == oldTabId);
            const auto pendingSourceProfileGuid = impl->TakePendingAgentSourceProfileGuid();
            VERIFY_IS_TRUE(pendingSourceProfileGuid.has_value());
            VERIFY_IS_TRUE(pendingSourceProfileGuid.value() == sourceProfileGuid);
        });
    }

    void TabTests::TransferredAgentFirstPaneCompletesHiddenTab()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            using DragStash = winrt::TerminalApp::implementation::AgentPaneDragStash;
            // A real transferred core has already completed its first layout.
            // Attaching a never-displayed core skips that initialization.
            auto sourcePane = page->_WrapInAgentPaneContent(page->_GetFocusedTabImpl()->DetachRoot());
            VERIFY_IS_TRUE(sourcePane->IsAgentPane());
            const auto args = sourcePane->GetContent().GetNewTerminalArgs(BuildStartupKind::Content).as<NewTerminalArgs>();
            const auto contentId = args.ContentId();
            const auto transferId = args.AgentPaneTransferId();
            page->_manager.Detach(sourcePane->GetTerminalControl());
            DragStash::Entry entry;
            entry.originalTabId = L"source-tab";
            entry.attachDisposition = DragStash::AttachDisposition::FirstPaneOfNewTab;
            entry.hidden = true;
            entry.sessionsView = true;
            entry.panePosition = L"left";
            entry.transferId = transferId;
            DragStash::Instance().Store(contentId, std::move(entry));
            auto cleanup = wil::scope_exit([&]() {
                DragStash::Instance().Take(contentId, transferId);
            });

            const auto transferredPane = page->_MakeTerminalPane(args, nullptr, nullptr);
            VERIFY_IS_NOT_NULL(transferredPane);
            const auto agentContent = transferredPane->GetContent().as<winrt::TerminalApp::AgentPaneContent>();
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(agentContent);
            const auto newTransferId = impl->TransferId();
            const auto newTab = page->_GetTabImpl(page->_CreateNewTabFromPane(transferredPane));
            VERIFY_IS_NOT_NULL(newTab);
            VERIFY_IS_TRUE(newTab->GetRootPane() == transferredPane);
            VERIFY_IS_TRUE(newTab->GetActivePane() == transferredPane);
            VERIFY_IS_TRUE(impl->AwaitingTransferredTabContent());
            VERIFY_IS_FALSE(newTab->HasStashedAgentPane());

            const auto normalPane = page->_MakePane(nullptr, nullptr, nullptr);
            const auto normalContent = normalPane->GetContent();
            const auto normalContentId = normalPane->GetTerminalControl().ContentId();
            page->_SplitPane(newTab, SplitDirection::Right, 0.5f, normalPane);

            VERIFY_ARE_EQUAL(2, newTab->GetLeafPaneCount());
            VERIFY_IS_FALSE(newTab->GetRootPane()->IsAgentPane());
            const auto agentLeaf = newTab->FindAgentPane();
            VERIFY_IS_NOT_NULL(agentLeaf);
            VERIFY_IS_TRUE(agentLeaf != newTab->GetRootPane());
            VERIFY_IS_TRUE(agentLeaf->GetContent() == agentContent);
            VERIFY_ARE_EQUAL(contentId, agentLeaf->GetTerminalControl().ContentId());
            VERIFY_IS_TRUE(newTab->HasStashedAgentPane());
            VERIFY_IS_TRUE(agentLeaf->IsHidden());
            VERIFY_IS_FALSE(impl->AwaitingTransferredTabContent());
            VERIFY_IS_FALSE(impl->TakeHiddenAfterTransfer());
            VERIFY_IS_TRUE(impl->IsSessionsView());
            VERIFY_IS_TRUE(impl->GetAgentPanePosition() == L"left");
            VERIFY_ARE_EQUAL(newTransferId, impl->TransferId());
            const auto activePane = newTab->GetActivePane();
            VERIFY_IS_NOT_NULL(activePane);
            VERIFY_IS_FALSE(activePane->IsAgentPane());
            VERIFY_IS_TRUE(activePane->GetContent() == normalContent);
            VERIFY_ARE_EQUAL(normalContentId, activePane->GetTerminalControl().ContentId());

            VERIFY_IS_TRUE(newTab->RestoreStashedAgentPane(SplitDirection::Left));
            VERIFY_IS_TRUE(newTab->GetActivePane() == agentLeaf);
            VERIFY_IS_FALSE(agentLeaf->IsHidden());
            const auto rejectedPane = page->_MakePane(nullptr, nullptr, nullptr);
            page->_SplitPane(newTab, SplitDirection::Right, 0.5f, rejectedPane);
            VERIFY_ARE_EQUAL(2, newTab->GetLeafPaneCount());
            VERIFY_IS_TRUE(newTab->FindAgentPane() == agentLeaf);
            VERIFY_IS_TRUE(newTab->FindAgentPaneContent() == agentContent);
            VERIFY_ARE_EQUAL(contentId, agentLeaf->GetTerminalControl().ContentId());
            rejectedPane->Shutdown();
        });
    }

    winrt::TerminalApp::implementation::SharedWtaLease TabTests::_acquireIsolatedAgentLease()
    {
        // Retirement coroutines hold a raw owner through the session-close
        // grace period. This isolated, never-spawned owner outlives all tests.
        static auto* const owner = new winrt::TerminalApp::implementation::SharedWta;
        std::lock_guard lock{ owner->_mtx };
        VERIFY_IS_FALSE(owner->_process.is_valid());
        VERIFY_IS_TRUE(owner->_cachedWtaPath.empty());
        VERIFY_ARE_EQUAL(size_t{ 0 }, owner->_activeRefCount);
        return owner->_AcquireLeaseLocked();
    }

    TransferTabSnapshot TabTests::_snapshotTransferTab(
        const winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage>& page,
        const winrt::com_ptr<winrt::TerminalApp::implementation::Tab>& tab)
    {
        TransferTabSnapshot snapshot;
        snapshot.tab = tab;
        snapshot.root = tab->GetRootPane();
        snapshot.activePane = tab->GetActivePane();
        snapshot.stableId = tab->StableId();
        snapshot.actions = ActionAndArgs::Serialize(winrt::single_threaded_vector<ActionAndArgs>(tab->BuildStartupActions(BuildStartupKind::Content)));
        snapshot.root->WalkTree([&](const auto& pane) {
            if (const auto control = pane->GetTerminalControl())
            {
                TransferLeafSnapshot leaf;
                leaf.pane = pane;
                leaf.content = pane->GetContent();
                leaf.control = control;
                leaf.contentId = control.ContentId();
                leaf.connection = control.Connection();
                leaf.core = page->_manager.TryLookupCore(leaf.contentId);
                VERIFY_IS_NOT_NULL(leaf.core);
                leaf.core.Closed([closed = leaf.closed](auto&&, auto&&) { ++*closed; });
                snapshot.leaves.emplace_back(std::move(leaf));
            }
        });
        return snapshot;
    }

    std::unique_ptr<ContentTransferFixture> TabTests::_createContentTransferFixture(bool agentFirst, bool hidden, bool freshReceiver, bool twoLeaves, std::optional<int32_t> historySize)
    {
        auto fixture = std::make_unique<ContentTransferFixture>();
        auto cleanup = wil::scope_exit([&]() {
            RunOnUIThread([&]() { fixture.reset(); });
        });
        TestOnUIThread([&]() {
            fixture->host = Grid{};
            fixture->host.RowDefinitions().Append(RowDefinition{});
            fixture->host.RowDefinitions().Append(RowDefinition{});
        });
        fixture->source = _commonSetup(nullptr, fixture->host, historySize);
        fixture->hidden = hidden;
        std::vector<std::shared_ptr<::details::Event>> initialized;
        const auto trackLayout = [&](const winrt::Microsoft::Terminal::Control::TermControl& control) {
            const auto ready = std::make_shared<::details::Event>();
            VERIFY_IS_TRUE(ready->IsValid());
            control.Initialized([ready](auto&&, auto&&) { ready->Set(); });
            initialized.push_back(ready);
        };
        const auto newConnection = [&]() {
            winrt::guid id;
            VERIFY_SUCCEEDED(CoCreateGuid(reinterpret_cast<GUID*>(&id)));
            auto connection = winrt::make_self<TestConnection>(id, winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            fixture->connections.push_back(connection);
            return connection;
        };
        const auto waitForLayout = [&]() {
            for (const auto& ready : initialized)
            {
                VERIFY_ARE_EQUAL(static_cast<DWORD>(WAIT_OBJECT_0), WaitForSingleObject(ready->m_handle, 10000));
            }
            initialized.clear();
        };

        TestOnUIThread([&]() {
            const auto& source = fixture->source;
            const auto tab = source->_GetFocusedTabImpl();
            tab->SuppressAgentPrewarm();
            source->Width(1200);
            source->Height(600);
            source->UpdateLayout();
            const auto primary = tab->GetActiveTerminalControl();
            primary.Connection(*newConnection());

            if (!twoLeaves)
            {
                const auto normal = source->_MakePane(nullptr, nullptr, *newConnection());
                trackLayout(normal->GetTerminalControl());
                VERIFY_IS_TRUE(source->_SplitPane(tab, SplitDirection::Right, 0.35f, normal));
            }
            source->UpdateLayout();
            tab->GetRootPane()->WalkTree([&](const auto& pane) {
                if (pane->GetTerminalControl() == primary)
                {
                    VERIFY_IS_TRUE(tab->FocusPane(pane->Id().value()));
                }
            });
            const auto agentPane = source->_WrapInAgentPaneContent(source->_MakePane(nullptr, nullptr, *newConnection()));
            fixture->agent = agentPane->GetContent().as<winrt::TerminalApp::AgentPaneContent>();
            fixture->agentContentId = agentPane->GetTerminalControl().ContentId();
            trackLayout(agentPane->GetTerminalControl());
            VERIFY_IS_TRUE(source->_SplitPane(tab, agentFirst ? SplitDirection::Left : SplitDirection::Right, 0.4f, agentPane));

            const auto properties = winrt::make<winrt::TerminalApp::implementation::WindowProperties>();
            const winrt::TerminalApp::TerminalPage projected{ properties, source->_manager };
            fixture->destination.copy_from(winrt::get_self<winrt::TerminalApp::implementation::TerminalPage>(projected));
            const auto& destination = fixture->destination;
            destination->_settings = source->_settings;
            destination->_terminalSettingsCache = std::make_shared<winrt::TerminalApp::implementation::TerminalSettingsCache>(destination->_settings);
            destination->Create();
            if (freshReceiver)
            {
                // Drive the real first-layout handler in a deterministic order
                // relative to the host's registration acknowledgement.
                destination->_layoutUpdatedRevoker.revoke();
            }
            destination->Width(1200);
            destination->Height(600);

            Grid::SetRow(*destination, 1);
            fixture->host.Children().Append(*destination);
            fixture->host.UpdateLayout();
            if (!freshReceiver)
            {
                const auto destinationPane = destination->_MakePane(nullptr, nullptr, *newConnection());
                trackLayout(destinationPane->GetTerminalControl());
                destination->_CreateNewTabFromPane(destinationPane);
                const auto destinationTab = destination->_GetFocusedTabImpl();
                destinationTab->SuppressAgentPrewarm();
                const auto sibling = destination->_MakePane(nullptr, nullptr, *newConnection());
                trackLayout(sibling->GetTerminalControl());
                VERIFY_IS_TRUE(destination->_SplitPane(destinationTab, SplitDirection::Right, 0.5f, sibling));
            }
            fixture->host.UpdateLayout();
        });
        waitForLayout();

        if (!freshReceiver)
        {
            TestOnUIThread([&]() {
                const auto pane = fixture->destination->_MakePane(nullptr, nullptr, *newConnection());
                trackLayout(pane->GetTerminalControl());
                fixture->destination->_CreateNewTabFromPane(pane);
                fixture->destination->_GetFocusedTabImpl()->SuppressAgentPrewarm();
                fixture->host.UpdateLayout();
            });
            waitForLayout();
        }

        TestOnUIThread([&]() {
            const auto tab = fixture->source->_GetFocusedTabImpl();
            const auto agent = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(fixture->agent);
            agent->AdoptLifetime({ _acquireIsolatedAgentLease(), fixture->source->_manager.TryLookupCore(fixture->agentContentId) });
            // A real content owner is required here, not a metadata-only stash entry.
            VERIFY_IS_TRUE(agent->HasLifetime());
            tab->SetAgentOverride(L"custom:transfer-regression", L"transfer-model", fixture->agentCustomCommand, L"wsl", L"TransferDistro");
            tab->AgentPanePositionOverride(winrt::hstring{ L"left" });
            fixture->agentRestoreIdentity = fixture->source->_GetAgentPaneIdentity(tab.get());
            VERIFY_ARE_NOT_EQUAL(fixture->source->_settings.GlobalSettings().EffectiveAcpAgent(), fixture->agentRestoreIdentity);
            agent->SetAgentRestoreIdentity(fixture->agentRestoreIdentity, fixture->agentCustomCommand);
            agent->SetAgentSessionOwner(fixture->agentRestoreIdentity);
            agent->SetAgentSessionId(L"transaction-original-session");
            agent->SetAgentRestoreExecutable(L"C:\\Test Tools\\wta-transfer.exe");
            agent->SetSessionsView(true);
            agent->SetAgentPanePosition(L"left");
            fixture->agentRestoreCommandline = fixture->agent.GetNewTerminalArgs(BuildStartupKind::Persist).as<NewTerminalArgs>().Commandline();
            tab->AgentSourceProfileGuid(fixture->source->_settings.AllProfiles().GetAt(1).Guid());
            fixture->agentGeneration = agent->TransferId();
            if (hidden)
            {
                tab->StashAgentPane();
            }
            else
            {
                // Stash already chooses a normal pane and defers its XAML focus.
                // Do not select a different target before that callback runs.
                tab->GetRootPane()->WalkTree([&](const auto& pane) {
                    if (pane->GetTerminalControl() && !pane->IsAgentPane())
                    {
                        tab->FocusPane(pane->Id().value());
                    }
                });
            }
            VERIFY_IS_FALSE(tab->GetActivePane()->IsAgentPane());
            fixture->original = _snapshotTransferTab(fixture->source, tab);
            VERIFY_ARE_EQUAL(twoLeaves ? size_t{ 2 } : size_t{ 3 }, fixture->original.leaves.size());
            const auto actions = tab->BuildStartupActions(BuildStartupKind::Content);
            VERIFY_ARE_EQUAL(agentFirst, _getTerminalArgs(actions.front()).ContentId() == fixture->agentContentId);
            if (!freshReceiver)
            {
                fixture->destination->_SelectTab(0);
                for (const auto& destinationTab : fixture->destination->_tabs)
                {
                    fixture->destinationTabs.push_back(_snapshotTransferTab(fixture->destination, fixture->destination->_GetTabImpl(destinationTab)));
                }
                VERIFY_ARE_EQUAL(size_t{ 2 }, fixture->destinationTabs.size());
            }
            for (const auto& page : { fixture->source, fixture->destination })
            {
                page->ProtocolVtSequenceReceived([events = fixture->events](auto&&, const winrt::hstring& json) {
                    Json::Value event;
                    Json::CharReaderBuilder builder;
                    std::string errors;
                    std::istringstream stream{ winrt::to_string(json) };
                    if (Json::parseFromStream(builder, stream, &event, &errors))
                    {
                        events->push_back(std::move(event));
                    }
                });
            }
        });
        cleanup.release();
        return fixture;
    }

    void TabTests::_verifyTransferTabUnchanged(
        const winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage>& page,
        const TransferTabSnapshot& snapshot)
    {
        VERIFY_IS_TRUE(page->_GetTabIndex(*snapshot.tab).has_value());
        VERIFY_ARE_EQUAL(snapshot.stableId, snapshot.tab->StableId());
        VERIFY_IS_TRUE(snapshot.root == snapshot.tab->GetRootPane());
        VERIFY_IS_TRUE(snapshot.activePane == snapshot.tab->GetActivePane());
        VERIFY_ARE_EQUAL(snapshot.actions, ActionAndArgs::Serialize(winrt::single_threaded_vector<ActionAndArgs>(snapshot.tab->BuildStartupActions(BuildStartupKind::Content))));
        for (const auto& leaf : snapshot.leaves)
        {
            VERIFY_IS_TRUE(leaf.pane->GetContent() == leaf.content);
            VERIFY_IS_TRUE(leaf.pane->GetTerminalControl() == leaf.control);
            VERIFY_ARE_EQUAL(leaf.contentId, leaf.control.ContentId());
            VERIFY_IS_TRUE(leaf.control.Connection() == leaf.connection);
            VERIFY_IS_TRUE(page->_manager.TryLookupCore(leaf.contentId) == leaf.core);
            VERIFY_IS_TRUE(leaf.control.TransferState() == winrt::Microsoft::Terminal::Control::ContentTransferState::Owned);
            VERIFY_ARE_EQUAL(0u, leaf.closed->load());
        }
    }

    void TabTests::_verifyTransferRollback(const ContentTransferFixture& fixture)
    {
        VERIFY_ARE_EQUAL(1u, fixture.source->_tabs.Size());
        VERIFY_IS_TRUE(fixture.source->_GetFocusedTabImpl() == fixture.original.tab);
        _verifyTransferTabUnchanged(fixture.source, fixture.original);
        const auto agent = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(fixture.agent);
        VERIFY_IS_TRUE(fixture.original.tab->FindAgentPaneContent() == fixture.agent);
        VERIFY_IS_TRUE(agent->HasLifetime());
        VERIFY_ARE_EQUAL(fixture.agentGeneration, agent->TransferId());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"transaction-original-session" }, agent->AgentSessionId());
        VERIFY_ARE_EQUAL(fixture.agentRestoreCommandline, fixture.agent.GetNewTerminalArgs(BuildStartupKind::Persist).as<NewTerminalArgs>().Commandline());
        VERIFY_IS_TRUE(agent->IsSessionsView());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"left" }, agent->GetAgentPanePosition());
        VERIFY_ARE_EQUAL(fixture.hidden, fixture.original.tab->FindAgentPane()->IsHidden());
        VERIFY_IS_TRUE(fixture.original.tab->AgentSourceProfileGuid() == fixture.source->_settings.AllProfiles().GetAt(1).Guid());
        _verifyTransferredAgentRestoreState(fixture, fixture.source, fixture.original.tab);
        VERIFY_ARE_EQUAL(fixture.destinationTabs.size(), static_cast<size_t>(fixture.destination->_tabs.Size()));
        for (size_t i = 0; i < fixture.destinationTabs.size(); ++i)
        {
            VERIFY_IS_TRUE(fixture.destination->_tabs.GetAt(static_cast<uint32_t>(i)) == *fixture.destinationTabs[i].tab);
            _verifyTransferTabUnchanged(fixture.destination, fixture.destinationTabs[i]);
        }
        if (!fixture.destinationTabs.empty())
        {
            VERIFY_IS_TRUE(fixture.destination->_GetFocusedTabImpl() == fixture.destinationTabs.front().tab);
        }
        for (const auto& connection : fixture.connections)
        {
            VERIFY_ARE_EQUAL(0u, connection->CloseCount());
        }
        for (const auto& event : *fixture.events)
        {
            VERIFY_ARE_NOT_EQUAL(std::string{ "tab_renamed" }, event["method"].asString());
            VERIFY_ARE_NOT_EQUAL(std::string{ "tab_closed" }, event["method"].asString());
            if (event["method"].asString() == "connection_state")
            {
                VERIFY_ARE_NOT_EQUAL(std::string{ "closed" }, event["params"]["state"].asString());
                VERIFY_ARE_NOT_EQUAL(std::string{ "failed" }, event["params"]["state"].asString());
            }
        }
    }

    winrt::TerminalApp::RequestMoveContentArgs TabTests::_requestContentTransfer(const ContentTransferFixture& fixture)
    {
        winrt::TerminalApp::RequestMoveContentArgs request{ nullptr };
        const auto token = fixture.source->RequestMoveContent([&](auto&&, const winrt::TerminalApp::RequestMoveContentArgs& args) {
            VERIFY_IS_NULL(request);
            request = args;
        });
        const auto revoke = wil::scope_exit([&]() { fixture.source->RequestMoveContent(token); });
        VERIFY_IS_TRUE(fixture.source->_MoveTab(fixture.original.tab, MoveTabArgs{ L"transaction-destination", MoveTabDirection::None }));
        VERIFY_IS_NOT_NULL(request);
        VERIFY_ARE_NOT_EQUAL(uint64_t{ 0 }, request.TransferId());
        VERIFY_ARE_EQUAL(fixture.original.actions, request.Content());
        return request;
    }

    void TabTests::_verifyTransferredAgentRestoreState(
        const ContentTransferFixture& fixture,
        const winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage>& page,
        const winrt::com_ptr<winrt::TerminalApp::implementation::Tab>& tab)
    {
        VERIFY_IS_TRUE(tab->HasAgentOverride());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:transfer-regression" }, tab->AgentIdOverride());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"transfer-model" }, tab->AgentModelOverride());
        VERIFY_ARE_EQUAL(fixture.agentCustomCommand, tab->AgentCustomCommandOverride());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"wsl" }, tab->AgentSourceOverride());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"TransferDistro" }, tab->AgentWslDistroOverride());
        VERIFY_IS_TRUE(tab->AgentPanePositionOverride().has_value());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"left" }, tab->AgentPanePositionOverride().value());
        // Persist immediately, without any helper status replay. Refresh also
        // verifies that the copied session owner still matches the tab override.
        page->_RefreshAgentRestoreIdentity(tab.get());
        const auto content = tab->FindAgentPaneContent();
        VERIFY_ARE_EQUAL(winrt::hstring{ L"transaction-original-session" }, content.AgentSessionId());
        const auto args = content.GetNewTerminalArgs(BuildStartupKind::Persist).as<NewTerminalArgs>();
        VERIFY_ARE_EQUAL(fixture.agentRestoreCommandline, args.Commandline());
        int argc = 0;
        const wil::unique_hlocal_ptr<PWSTR[]> argv{ CommandLineToArgvW(args.Commandline().c_str(), &argc) };
        VERIFY_IS_NOT_NULL(argv.get());
        VERIFY_IS_TRUE(argc > 0);
        VERIFY_ARE_EQUAL(std::wstring{ L"C:\\Test Tools\\wta-transfer.exe" }, std::wstring{ argv.get()[0] });
        std::vector<std::wstring> tokens;
        for (int i = 0; i < argc; ++i)
        {
            tokens.emplace_back(argv.get()[i]);
        }
        const auto fields = ::Microsoft::Terminal::AgentPaneRestore::ParsePaneCommandline(tokens);
        VERIFY_ARE_EQUAL(std::wstring{ L"transaction-original-session" }, fields.sessionId);
        VERIFY_ARE_EQUAL(std::wstring{ fixture.agentRestoreIdentity }, fields.agentIdentity);
        VERIFY_ARE_EQUAL(std::wstring{ fixture.agentCustomCommand }, fields.customCommand);
        VERIFY_ARE_EQUAL(std::wstring{ L"sessions" }, fields.view);
    }

    void TabTests::_verifyTransferCommitted(const ContentTransferFixture& fixture)
    {
        VERIFY_ARE_EQUAL(0u, fixture.source->_tabs.Size());
        VERIFY_ARE_EQUAL(fixture.destinationTabs.size() + 1, static_cast<size_t>(fixture.destination->_tabs.Size()));
        for (const auto& tab : fixture.destinationTabs)
        {
            _verifyTransferTabUnchanged(fixture.destination, tab);
        }
        const auto moved = fixture.destination->_GetFocusedTabImpl();
        VERIFY_ARE_NOT_EQUAL(fixture.original.stableId, moved->StableId());
        VERIFY_ARE_EQUAL(static_cast<int>(fixture.original.leaves.size()), moved->GetLeafPaneCount());
        std::vector<uint64_t> received;
        moved->GetRootPane()->WalkTree([&](const auto& pane) {
            if (const auto control = pane->GetTerminalControl())
            {
                const auto found = std::find_if(fixture.original.leaves.begin(), fixture.original.leaves.end(), [&](const auto& leaf) {
                    return leaf.contentId == control.ContentId();
                });
                VERIFY_IS_TRUE(found != fixture.original.leaves.end());
                VERIFY_IS_TRUE(std::find(received.begin(), received.end(), control.ContentId()) == received.end());
                received.push_back(control.ContentId());
                VERIFY_IS_TRUE(control.Connection() == found->connection);
                VERIFY_ARE_EQUAL(found->connection.SessionId(), control.Connection().SessionId());
                VERIFY_IS_TRUE(control != found->control);
                VERIFY_IS_TRUE(found->control.TransferState() == winrt::Microsoft::Terminal::Control::ContentTransferState::Closed);
                VERIFY_IS_TRUE(control.TransferState() == winrt::Microsoft::Terminal::Control::ContentTransferState::Owned);
                VERIFY_IS_TRUE(fixture.destination->_manager.TryLookupCore(found->contentId) == found->core);
                VERIFY_ARE_EQUAL(0u, found->closed->load());
            }
        });
        VERIFY_ARE_EQUAL(fixture.original.leaves.size(), received.size());
        const auto newAgent = moved->FindAgentPaneContent();
        VERIFY_IS_NOT_NULL(newAgent);
        VERIFY_IS_TRUE(newAgent != fixture.agent);
        const auto newImpl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(newAgent);
        VERIFY_IS_TRUE(newImpl->HasLifetime());
        VERIFY_IS_FALSE(winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(fixture.agent)->HasLifetime());
        VERIFY_ARE_EQUAL(fixture.agentContentId, newAgent.GetTermControl().ContentId());
        VERIFY_ARE_NOT_EQUAL(fixture.agentGeneration, newImpl->TransferId());
        VERIFY_ARE_EQUAL(fixture.hidden, moved->FindAgentPane()->IsHidden());
        VERIFY_IS_TRUE(newImpl->IsSessionsView());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"left" }, newImpl->GetAgentPanePosition());
        VERIFY_IS_TRUE(moved->AgentSourceProfileGuid() == fixture.source->_settings.AllProfiles().GetAt(1).Guid());
        _verifyTransferredAgentRestoreState(fixture, fixture.destination, moved);
        size_t renamed = 0;
        for (const auto& event : *fixture.events)
        {
            VERIFY_ARE_NOT_EQUAL(std::string{ "tab_closed" }, event["method"].asString());
            if (event["method"].asString() == "connection_state")
            {
                VERIFY_ARE_NOT_EQUAL(std::string{ "closed" }, event["params"]["state"].asString());
                VERIFY_ARE_NOT_EQUAL(std::string{ "failed" }, event["params"]["state"].asString());
            }
            if (event["method"].asString() == "tab_renamed")
            {
                ++renamed;
                VERIFY_ARE_EQUAL(winrt::to_string(fixture.original.stableId), event["params"]["old_tab_id"].asString());
                VERIFY_ARE_EQUAL(winrt::to_string(moved->StableId()), event["params"]["new_tab_id"].asString());
            }
        }
        VERIFY_ARE_EQUAL(size_t{ 1 }, renamed);
        for (const auto& connection : fixture.connections)
        {
            VERIFY_ARE_EQUAL(0u, connection->CloseCount());
        }
    }

    void TabTests::_closeContentTransferFixture(ContentTransferFixture& fixture, bool verify)
    {
        fixture.destination->_contentTransferTestHook = {};
        for (const auto& page : { fixture.source, fixture.destination })
        {
            while (page->_tabs.Size())
            {
                page->_RemoveTab(page->_tabs.GetAt(0));
            }
        }
        if (verify)
        {
            for (const auto& connection : fixture.connections)
            {
                VERIFY_ARE_EQUAL(1u, connection->CloseCount());
            }
            for (const auto& leaf : fixture.original.leaves)
            {
                VERIFY_ARE_EQUAL(1u, leaf.closed->load());
                VERIFY_IS_NULL(fixture.source->_manager.TryLookupCore(leaf.contentId));
            }
            for (const auto& tab : fixture.destinationTabs)
            {
                for (const auto& leaf : tab.leaves)
                {
                    VERIFY_ARE_EQUAL(1u, leaf.closed->load());
                    VERIFY_IS_NULL(fixture.destination->_manager.TryLookupCore(leaf.contentId));
                }
            }
        }
    }

    void TabTests::_verifyContentTransferFailure(bool agentFirst, bool hidden, TransferStage stage)
    {
        auto fixture = _createContentTransferFixture(agentFirst, hidden);
        const auto cleanup = wil::scope_exit([&]() {
            RunOnUIThread([&]() {
                _closeContentTransferFixture(*fixture, false);
                fixture.reset();
            });
        });
        winrt::TerminalApp::RequestMoveContentArgs request{ nullptr };
        TestOnUIThread([&]() {
            request = _requestContentTransfer(*fixture);
            _verifyTransferRollback(*fixture);
            bool faultReached = false;
            fixture->destination->_contentTransferTestHook = [&](TransferStage current, uint64_t contentId, uint32_t actionIndex) {
                if (current != stage ||
                    (stage == TransferStage::ControlAttached && contentId != fixture->agentContentId) ||
                    (stage == TransferStage::BeforeSplitInsertion && actionIndex < 2))
                {
                    return;
                }
                faultReached = true;
                if (stage == TransferStage::BeforeClaim)
                {
                    winrt::TerminalApp::implementation::ContentTransfer::Expire(request.TransferId());
                    return;
                }
                if (stage == TransferStage::ControlAttached)
                {
                    VERIFY_ARE_EQUAL(agentFirst, actionIndex == 0);
                }
                if (stage == TransferStage::BeforeSplitInsertion)
                {
                    VERIFY_IS_TRUE(fixture->destination->_GetFocusedTabImpl()->GetLeafPaneCount() >= 2);
                }
                winrt::throw_hresult(E_ABORT);
            };
            const auto clearHook = wil::scope_exit([&]() { fixture->destination->_contentTransferTestHook = {}; });
            VERIFY_IS_FALSE(fixture->destination->AttachContent(ActionAndArgs::Deserialize(request.Content()), 1, request.TransferId()));
            VERIFY_IS_TRUE(faultReached);
            fixture->destination->_contentTransferTestHook = {};
            _verifyTransferRollback(*fixture);
            VERIFY_IS_FALSE(fixture->destination->AttachContent(ActionAndArgs::Deserialize(request.Content()), 1, request.TransferId()));
            _verifyTransferRollback(*fixture);
        });
        for (const auto& connection : fixture->connections)
        {
            VERIFY_IS_FALSE(connection->WaitForClose(100));
        }
        TestOnUIThread([&]() {
            _verifyTransferRollback(*fixture);
            const auto retry = _requestContentTransfer(*fixture);
            VERIFY_ARE_NOT_EQUAL(request.TransferId(), retry.TransferId());
            winrt::TerminalApp::implementation::ContentTransfer::Expire(request.TransferId());
            VERIFY_IS_TRUE(fixture->destination->AttachContent(ActionAndArgs::Deserialize(retry.Content()), 1, retry.TransferId()));
            _verifyTransferCommitted(*fixture);
            VERIFY_IS_FALSE(fixture->destination->AttachContent(ActionAndArgs::Deserialize(retry.Content()), 1, retry.TransferId()));
            winrt::TerminalApp::implementation::ContentTransfer::Expire(request.TransferId());
            _verifyTransferCommitted(*fixture);
            fixture->host.UpdateLayout();
        });
        TestOnUIThread([&]() {
            _verifyTransferCommitted(*fixture);
            _closeContentTransferFixture(*fixture, true);
        });
    }

    void TabTests::ContentTransferAgentFirstExpiryRollsBack()
    {
        _verifyContentTransferFailure(true, false, TransferStage::BeforeClaim);
    }

    void TabTests::ContentTransferAgentLaterExpiryRollsBack()
    {
        _verifyContentTransferFailure(false, true, TransferStage::BeforeClaim);
    }

    void TabTests::ContentTransferAgentFirstSetupFailureRollsBack()
    {
        _verifyContentTransferFailure(true, true, TransferStage::ControlAttached);
    }

    void TabTests::ContentTransferAgentLaterSetupFailureRollsBack()
    {
        _verifyContentTransferFailure(false, false, TransferStage::ControlAttached);
    }

    void TabTests::ContentTransferAgentFirstInsertionFailureRollsBack()
    {
        _verifyContentTransferFailure(true, false, TransferStage::BeforeFirstPaneInsertion);
    }

    void TabTests::ContentTransferAgentLaterInsertionFailureRollsBack()
    {
        _verifyContentTransferFailure(false, true, TransferStage::BeforeFirstPaneInsertion);
    }

    void TabTests::ContentTransferAgentFirstLaterSplitFailureRollsBack()
    {
        _verifyContentTransferFailure(true, true, TransferStage::BeforeSplitInsertion);
    }

    void TabTests::ContentTransferAgentLaterLaterSplitFailureRollsBack()
    {
        _verifyContentTransferFailure(false, false, TransferStage::BeforeSplitInsertion);
    }

    void TabTests::_verifyContentTransferStartupGate(bool layoutFirst)
    {
        auto fixture = _createContentTransferFixture(layoutFirst, true, true);
        const auto cleanup = wil::scope_exit([&]() {
            RunOnUIThread([&]() {
                _closeContentTransferFixture(*fixture, false);
                fixture.reset();
            });
        });
        TestOnUIThread([&]() {
            const auto request = _requestContentTransfer(*fixture);
            const auto& destination = fixture->destination;
            const auto actions = ActionAndArgs::Deserialize(request.Content());
            destination->SetStartupActions({ actions.begin(), actions.end() });
            destination->SetStartupTransfer(request.TransferId());
            VERIFY_IS_TRUE(destination->_startupState == winrt::TerminalApp::implementation::StartupState::NotInitialized);
            if (layoutFirst)
            {
                destination->_OnFirstLayout(nullptr, nullptr);
                VERIFY_IS_TRUE(destination->_startupState == winrt::TerminalApp::implementation::StartupState::InStartup);
                _verifyTransferRollback(*fixture);
                destination->ContentTransferReceiverReady();
            }
            else
            {
                destination->ContentTransferReceiverReady();
                VERIFY_IS_TRUE(destination->_startupState == winrt::TerminalApp::implementation::StartupState::NotInitialized);
                _verifyTransferRollback(*fixture);
                destination->_OnFirstLayout(nullptr, nullptr);
            }
            VERIFY_IS_TRUE(destination->_startupState == winrt::TerminalApp::implementation::StartupState::Initialized);
            VERIFY_ARE_EQUAL(uint64_t{ 0 }, destination->_startupTransferId);
            _verifyTransferCommitted(*fixture);
            destination->ContentTransferReceiverReady();
            destination->_OnFirstLayout(nullptr, nullptr);
            _verifyTransferCommitted(*fixture);
            fixture->host.UpdateLayout();
            _closeContentTransferFixture(*fixture, true);
        });
    }

    void TabTests::ContentTransferFirstLayoutWaitsForReceiver()
    {
        _verifyContentTransferStartupGate(true);
    }

    void TabTests::ContentTransferReceiverWaitsForFirstLayout()
    {
        _verifyContentTransferStartupGate(false);
    }

    void TabTests::ContentTransferExpiredStartupClosesEmptyReceiver()
    {
        auto fixture = _createContentTransferFixture(true, true, true);
        const auto cleanup = wil::scope_exit([&]() {
            RunOnUIThread([&]() {
                _closeContentTransferFixture(*fixture, false);
                fixture.reset();
            });
        });
        TestOnUIThread([&]() {
            const auto applicationState = ApplicationState::SharedInstance();
            const auto freCompleted = applicationState.AgentFreCompleted();
            const auto restoreFreCompleted = wil::scope_exit([&]() {
                applicationState.AgentFreCompleted(freCompleted);
            });
            applicationState.AgentFreCompleted(true);

            const auto request = _requestContentTransfer(*fixture);
            const auto& destination = fixture->destination;
            const auto actions = ActionAndArgs::Deserialize(request.Content());
            destination->SetStartupActions({ actions.begin(), actions.end() });
            destination->SetStartupTransfer(request.TransferId());

            unsigned int closeRequests = 0;
            const auto closeToken = destination->CloseWindowRequested([&](auto&&, auto&&) {
                ++closeRequests;
            });
            const auto revokeClose = wil::scope_exit([&]() {
                destination->CloseWindowRequested(closeToken);
            });
            unsigned int receiveAttempts = 0;
            destination->_contentTransferTestHook = [&](TransferStage stage, uint64_t, uint32_t) {
                if (stage == TransferStage::BeforeClaim)
                {
                    ++receiveAttempts;
                    winrt::TerminalApp::implementation::ContentTransfer::Expire(request.TransferId());
                }
            };
            const auto clearHook = wil::scope_exit([&]() { destination->_contentTransferTestHook = {}; });

            destination->_OnFirstLayout(nullptr, nullptr);
            VERIFY_IS_TRUE(destination->_startupState == winrt::TerminalApp::implementation::StartupState::InStartup);
            VERIFY_ARE_EQUAL(0u, receiveAttempts);
            VERIFY_ARE_EQUAL(0u, closeRequests);
            _verifyTransferRollback(*fixture);

            destination->ContentTransferReceiverReady();
            VERIFY_ARE_EQUAL(1u, receiveAttempts);
            VERIFY_ARE_EQUAL(1u, closeRequests);
            VERIFY_ARE_EQUAL(0u, destination->_tabs.Size());
            VERIFY_ARE_EQUAL(uint64_t{ 0 }, destination->_startupTransferId);
            VERIFY_IS_TRUE(destination->_startupState == winrt::TerminalApp::implementation::StartupState::Initialized);
            _verifyTransferRollback(*fixture);

            destination->ContentTransferReceiverReady();
            destination->_OnFirstLayout(nullptr, nullptr);
            VERIFY_ARE_EQUAL(1u, receiveAttempts);
            VERIFY_ARE_EQUAL(1u, closeRequests);
            _verifyTransferRollback(*fixture);
            _closeContentTransferFixture(*fixture, true);
        });
    }

    void TabTests::_verifyContentTransferSourceRejection(bool rejectWindow)
    {
        auto fixture = _createContentTransferFixture(rejectWindow, rejectWindow);
        const auto cleanup = wil::scope_exit([&]() {
            RunOnUIThread([&]() {
                _closeContentTransferFixture(*fixture, false);
                fixture.reset();
            });
        });
        TestOnUIThread([&]() {
            winrt::TerminalApp::RequestMoveContentArgs rejected{ nullptr };
            if (rejectWindow)
            {
                const auto token = fixture->source->RequestMoveContent([&](auto&&, const winrt::TerminalApp::RequestMoveContentArgs& args) {
                    rejected = args;
                    winrt::throw_hresult(E_ABORT);
                });
                const auto revoke = wil::scope_exit([&]() { fixture->source->RequestMoveContent(token); });
                VERIFY_IS_TRUE(fixture->source->_MoveTab(fixture->original.tab, MoveTabArgs{ L"new", MoveTabDirection::None }));
                VERIFY_IS_NOT_NULL(rejected);
            }
            else
            {
                VERIFY_IS_TRUE(fixture->source->_MoveTab(fixture->original.tab, MoveTabArgs{ L"new", MoveTabDirection::None }));
            }
            _verifyTransferRollback(*fixture);
            if (rejected)
            {
                // Event-handler failures do not acknowledge a transfer. Its
                // pending deadline must expire without touching source content.
                winrt::TerminalApp::implementation::ContentTransfer::Expire(rejected.TransferId());
                VERIFY_IS_FALSE(fixture->destination->AttachContent(ActionAndArgs::Deserialize(rejected.Content()), 1, rejected.TransferId()));
                _verifyTransferRollback(*fixture);
            }
            const auto retry = _requestContentTransfer(*fixture);
            VERIFY_IS_TRUE(fixture->destination->AttachContent(ActionAndArgs::Deserialize(retry.Content()), 1, retry.TransferId()));
            _verifyTransferCommitted(*fixture);
            _closeContentTransferFixture(*fixture, true);
        });
    }

    void TabTests::ContentTransferMissingMoveHandlerPreservesSource()
    {
        _verifyContentTransferSourceRejection(false);
    }

    void TabTests::ContentTransferRejectedWindowPreservesSource()
    {
        _verifyContentTransferSourceRejection(true);
    }

    void TabTests::_waitForContentTransferReviewUI(const std::function<bool()>& predicate)
    {
        ::details::Event settled;
        DispatcherTimer timer{ nullptr };
        winrt::event_token tick{};
        unsigned int consecutive = 0;
        const auto cleanup = wil::scope_exit([&]() {
            RunOnUIThread([&]() {
                if (timer)
                {
                    timer.Stop();
                    timer.Tick(tick);
                    timer = nullptr;
                }
            });
        });
        TestOnUIThread([&]() {
            timer = DispatcherTimer{};
            // Observe stable UI state across both 8ms scrollbar throttles,
            // rather than sleeping and assuming their callbacks have run.
            timer.Interval(std::chrono::milliseconds{ 20 });
            tick = timer.Tick([&](auto&&, auto&&) {
                consecutive = predicate() ? consecutive + 1 : 0;
                if (consecutive == 3)
                {
                    timer.Stop();
                    settled.Set();
                }
            });
            timer.Start();
        });
        VERIFY_ARE_EQUAL(static_cast<DWORD>(WAIT_OBJECT_0), WaitForSingleObject(settled.m_handle, 10000));
    }

    void TabTests::_verifyContentTransferReviewZoom(bool hidden, bool zoomed, bool freshReceiver, bool twoLeaves)
    {
        auto fixture = _createContentTransferFixture(true, hidden, freshReceiver, twoLeaves);
        const auto cleanup = wil::scope_exit([&]() {
            RunOnUIThread([&]() {
                _closeContentTransferFixture(*fixture, false);
                fixture.reset();
            });
        });
        // Let the stash's deferred focus run before zooming its chosen shell.
        _waitForContentTransferReviewUI([&]() {
            const auto active = fixture->original.tab->GetActivePane();
            return active == fixture->original.activePane && active && !active->IsAgentPane();
        });
        TestOnUIThread([&]() {
            const auto tab = fixture->original.tab;
            VERIFY_ARE_EQUAL(twoLeaves ? 2 : 3, tab->GetLeafPaneCount());
            if (!twoLeaves)
            {
                // Focus outside the hidden agent's sibling so the deferred
                // stash callback must yield to the tab's final focus action.
                tab->GetRootPane()->WalkTree([&](const auto& pane) {
                    if (pane->GetTerminalControl() && !pane->IsAgentPane() && pane != fixture->original.activePane)
                    {
                        VERIFY_IS_TRUE(tab->FocusPane(pane->Id().value()));
                        return true;
                    }
                    return false;
                });
            }
            VERIFY_IS_FALSE(tab->GetActivePane()->IsAgentPane());
            VERIFY_ARE_EQUAL(hidden, tab->FindAgentPane()->IsHidden());
            if (zoomed)
            {
                fixture->source->_HandleTogglePaneZoom(nullptr, ActionEventArgs{});
            }
            VERIFY_ARE_EQUAL(zoomed, tab->IsZoomed());
            fixture->host.UpdateLayout();
            fixture->original = _snapshotTransferTab(fixture->source, tab);
            const auto actions = tab->BuildStartupActions(BuildStartupKind::Content);
            VERIFY_IS_TRUE(actions.front().Action() == ShortcutAction::NewTab);
            VERIFY_ARE_EQUAL(fixture->agentContentId, _getTerminalArgs(actions.front()).ContentId());
            VERIFY_ARE_EQUAL(fixture->agentGeneration, _getTerminalArgs(actions.front()).AgentPaneTransferId());
            if (zoomed)
            {
                VERIFY_IS_TRUE(actions.back().Action() == ShortcutAction::TogglePaneZoom);
                VERIFY_IS_TRUE(actions[actions.size() - 2].Action() == ShortcutAction::FocusPane);
                VERIFY_IS_TRUE(tab->Content() == tab->GetActivePane()->GetRootElement());
            }
            _verifyTransferRollback(*fixture);
        });
        uint64_t shellId = 0;
        TestOnUIThread([&]() {
            shellId = fixture->original.activePane->GetTerminalControl().ContentId();
            const auto request = _requestContentTransfer(*fixture);
            const auto& destination = fixture->destination;
            if (freshReceiver)
            {
                const auto applicationState = ApplicationState::SharedInstance();
                const auto freCompleted = applicationState.AgentFreCompleted();
                const auto restore = wil::scope_exit([&]() { applicationState.AgentFreCompleted(freCompleted); });
                applicationState.AgentFreCompleted(true);
                const auto actions = ActionAndArgs::Deserialize(request.Content());
                destination->SetStartupActions({ actions.begin(), actions.end() });
                destination->SetStartupTransfer(request.TransferId());
                destination->_OnFirstLayout(nullptr, nullptr);
                VERIFY_IS_TRUE(destination->_startupState == winrt::TerminalApp::implementation::StartupState::InStartup);
                _verifyTransferRollback(*fixture);
                destination->ContentTransferReceiverReady();
                if (fixture->source->_tabs.Size() != 0)
                {
                    _verifyTransferRollback(*fixture);
                }
                // Outcome first: a valid hidden, zoomed tab must not roll back.
                VERIFY_ARE_EQUAL(0u, fixture->source->_tabs.Size());
                VERIFY_IS_TRUE(destination->_startupState == winrt::TerminalApp::implementation::StartupState::Initialized);
            }
            else
            {
                const auto accepted = destination->AttachContent(ActionAndArgs::Deserialize(request.Content()), 1, request.TransferId());
                if (!accepted)
                {
                    _verifyTransferRollback(*fixture);
                }
                VERIFY_IS_TRUE(accepted);
            }
            _verifyTransferCommitted(*fixture);
            const auto moved = destination->_GetFocusedTabImpl();
            VERIFY_ARE_EQUAL(shellId, moved->GetActiveTerminalControl().ContentId());
            VERIFY_IS_FALSE(moved->GetActivePane()->IsAgentPane());
            VERIFY_ARE_EQUAL(zoomed, moved->IsZoomed());
            fixture->host.UpdateLayout();
        });
        _waitForContentTransferReviewUI([&]() {
            const auto moved = fixture->destination->_GetFocusedTabImpl();
            const auto control = moved ? moved->GetActiveTerminalControl() : nullptr;
            const auto agent = moved ? moved->FindAgentPane() : nullptr;
            return control && agent && control.ContentId() == shellId &&
                   moved->IsZoomed() == zoomed && agent->IsHidden() == hidden;
        });
        TestOnUIThread([&]() { _closeContentTransferFixture(*fixture, true); });
    }

    void TabTests::ContentTransferReviewHiddenZoomedTabMovesToExistingPage()
    {
        _verifyContentTransferReviewZoom(true, true, false);
    }

    void TabTests::ContentTransferReviewHiddenZoomedTabMovesToFreshReceiver()
    {
        _verifyContentTransferReviewZoom(true, true, true);
    }

    void TabTests::ContentTransferReviewHiddenTabMovesWithoutZoom()
    {
        _verifyContentTransferReviewZoom(true, false, false);
    }

    void TabTests::ContentTransferReviewVisibleZoomedTabMoves()
    {
        _verifyContentTransferReviewZoom(false, true, false);
    }

    void TabTests::ContentTransferReviewHiddenZoomedNestedTabKeepsFinalFocus()
    {
        _verifyContentTransferReviewZoom(true, true, false, false);
    }

    void TabTests::_verifyContentTransferReviewScroll(bool transfer, bool reject)
    {
        auto fixture = _createContentTransferFixture(false, false, false, false, 1000);
        const auto cleanup = wil::scope_exit([&]() {
            RunOnUIThread([&]() {
                _closeContentTransferFixture(*fixture, false);
                fixture.reset();
            });
        });
        winrt::Microsoft::Terminal::Control::TermControl control{ nullptr };
        Controls::Primitives::ScrollBar scrollbar{ nullptr };
        TestOnUIThread([&]() {
            control = fixture->original.tab->GetActiveTerminalControl();
            scrollbar = control.FindName(L"ScrollBar").as<Controls::Primitives::ScrollBar>();
            const auto found = std::find_if(fixture->connections.begin(), fixture->connections.end(), [&](const auto& connection) {
                return *connection == control.Connection();
            });
            VERIFY_IS_TRUE(found != fixture->connections.end());
            std::u16string output;
            for (unsigned int i = 0; i < 500; ++i)
            {
                output += u"content-transfer scrollback fixture\r\n";
            }
            // Feed the real terminal parser, not a shell or the echo input path.
            (*found)->TerminalOutput.raise(winrt::array_view<const char16_t>{ output.data(), output.data() + output.size() });
        });
        _waitForContentTransferReviewUI([&]() {
            return scrollbar.Maximum() > 100 && scrollbar.Value() == control.ScrollOffset() && control.ScrollOffset() > 100;
        });
        constexpr int expectedOffset = 37;
        TestOnUIThread([&]() { control.ScrollViewport(expectedOffset); });
        _waitForContentTransferReviewUI([&]() {
            return control.ScrollOffset() == expectedOffset && scrollbar.Value() == expectedOffset;
        });
        TestOnUIThread([&]() {
            fixture->original = _snapshotTransferTab(fixture->source, fixture->original.tab);
            VERIFY_ARE_EQUAL(expectedOffset, control.ScrollOffset());
            if (transfer)
            {
                const auto request = _requestContentTransfer(*fixture);
                bool suspended = false;
                fixture->destination->_contentTransferTestHook = [&](TransferStage stage, uint64_t, uint32_t) {
                    if (reject && stage == TransferStage::BeforeFirstPaneInsertion)
                    {
                        suspended = control.TransferState() == winrt::Microsoft::Terminal::Control::ContentTransferState::Suspended;
                        winrt::throw_hresult(E_ABORT);
                    }
                };
                const auto clearHook = wil::scope_exit([&]() { fixture->destination->_contentTransferTestHook = {}; });
                const auto accepted = fixture->destination->AttachContent(ActionAndArgs::Deserialize(request.Content()), 1, request.TransferId());
                // Capture before layout, output, input, or another dispatcher turn
                // can conceal the rollback's viewport mutation.
                const auto actualOffset = control.ScrollOffset();
                Log::Comment(NoThrowString().Format(L"Transfer viewport: before=%d after=%d", expectedOffset, actualOffset));
                Log::Comment(NoThrowString().Format(L"Transfer accepted=%d suspended at rejection=%d", accepted, suspended));
                VERIFY_ARE_EQUAL(!reject, accepted);
                if (reject)
                {
                    VERIFY_IS_TRUE(suspended);
                    _verifyTransferRollback(*fixture);
                }
                else
                {
                    _verifyTransferCommitted(*fixture);
                    control = fixture->destination->_GetFocusedTabImpl()->GetActiveTerminalControl();
                    scrollbar = control.FindName(L"ScrollBar").as<Controls::Primitives::ScrollBar>();
                    fixture->host.UpdateLayout();
                }
                VERIFY_ARE_EQUAL(expectedOffset, actualOffset);
            }
            if (!transfer || reject)
            {
                _verifyTransferRollback(*fixture);
            }
        });
        _waitForContentTransferReviewUI([&]() {
            return control.ScrollOffset() == expectedOffset && scrollbar.Value() == expectedOffset;
        });
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(expectedOffset, control.ScrollOffset());
            if (!transfer || reject)
            {
                _verifyTransferRollback(*fixture);
            }
            _closeContentTransferFixture(*fixture, true);
        });
    }

    void TabTests::ContentTransferReviewRollbackPreservesScrollOffset()
    {
        _verifyContentTransferReviewScroll(true);
    }

    void TabTests::ContentTransferReviewScrollOffsetWithoutTransferIsStable()
    {
        _verifyContentTransferReviewScroll(false);
    }

    void TabTests::ContentTransferReviewSuccessfulMovePreservesScrollOffset()
    {
        _verifyContentTransferReviewScroll(true, false);
    }

    void TabTests::_verifyContentTransferReviewSuppression(bool singlePane, bool destinationSuppressed)
    {
        auto fixture = _createContentTransferFixture(false, false, false, !singlePane);
        const auto cleanup = wil::scope_exit([&]() {
            RunOnUIThread([&]() {
                _closeContentTransferFixture(*fixture, false);
                fixture.reset();
            });
        });
        TestOnUIThread([&]() {
            const auto tab = fixture->original.tab;
            tab->AllowAgentPrewarm();
            VERIFY_IS_FALSE(tab->AgentPrewarmSuppressed());
            // Use the real user-close handler, including pane removal, rather
            // than just setting the suppression bit on an artificial shell tab.
            fixture->source->_HandleClosePaneRequested(tab->FindAgentPane());
            VERIFY_IS_TRUE(tab->AgentPrewarmSuppressed());
        });
        _waitForContentTransferReviewUI([&]() { return fixture->original.tab->FindAgentPane() == nullptr; });
        TestOnUIThread([&]() {
            const auto sourceTab = fixture->original.tab;
            VERIFY_IS_TRUE(sourceTab->AgentPrewarmSuppressed());
            VERIFY_IS_TRUE(sourceTab->FindAgentPane() == nullptr);
            VERIFY_ARE_EQUAL(singlePane ? 2 : 1, sourceTab->GetLeafPaneCount());
            fixture->original = _snapshotTransferTab(fixture->source, sourceTab);
            const auto destinationTab = fixture->destination->_GetFocusedTabImpl();
            if (singlePane && !destinationSuppressed)
            {
                destinationTab->AllowAgentPrewarm();
            }
            // Inspect the authoritative gate before the queued low-priority
            // initializer. Always suppress it before yielding, including when
            // the regression assertion fails; no real helper may be started.
            const auto guardPrewarm = wil::scope_exit([&]() {
                for (const auto& tab : fixture->destination->_tabs)
                {
                    fixture->destination->_GetTabImpl(tab)->SuppressAgentPrewarm();
                }
            });
            winrt::TerminalApp::RequestMoveContentArgs request{ nullptr };
            if (singlePane)
            {
                const auto token = fixture->source->RequestMoveContent([&](auto&&, const winrt::TerminalApp::RequestMoveContentArgs& args) { request = args; });
                const auto revoke = wil::scope_exit([&]() { fixture->source->RequestMoveContent(token); });
                MovePaneArgs args{ 0, L"transaction-destination" };
                VERIFY_IS_TRUE(fixture->source->_MovePane(args));
                VERIFY_IS_NOT_NULL(request);
                VERIFY_IS_TRUE(ActionAndArgs::Deserialize(request.Content()).GetAt(0).Action() == ShortcutAction::SplitPane);
            }
            else
            {
                // A one-leaf tree is still a whole-tab operation: use _MoveTab.
                request = _requestContentTransfer(*fixture);
                VERIFY_IS_TRUE(ActionAndArgs::Deserialize(request.Content()).GetAt(0).Action() == ShortcutAction::NewTab);
            }
            VERIFY_IS_TRUE(fixture->destination->AttachContent(ActionAndArgs::Deserialize(request.Content()), 0, request.TransferId()));
            const auto moved = fixture->destination->_GetFocusedTabImpl();
            VERIFY_IS_TRUE(moved->FindAgentPane() == nullptr);
            if (singlePane)
            {
                VERIFY_IS_TRUE(moved == destinationTab);
                VERIFY_ARE_EQUAL(3, moved->GetLeafPaneCount());
                VERIFY_ARE_EQUAL(1, sourceTab->GetLeafPaneCount());
                VERIFY_IS_TRUE(sourceTab->AgentPrewarmSuppressed());
            }
            else
            {
                VERIFY_ARE_EQUAL(0u, fixture->source->_tabs.Size());
                VERIFY_ARE_EQUAL(1, moved->GetLeafPaneCount());
                VERIFY_ARE_EQUAL(fixture->original.leaves.front().contentId, moved->GetActiveTerminalControl().ContentId());
                VERIFY_IS_TRUE(moved->GetActiveTerminalControl().Connection() == fixture->original.leaves.front().connection);
            }
            Log::Comment(NoThrowString().Format(L"Prewarm suppression: wholeTab=%d source=%d destination=%d",
                                                !singlePane,
                                                sourceTab->AgentPrewarmSuppressed(),
                                                moved->AgentPrewarmSuppressed()));
            VERIFY_ARE_EQUAL(!singlePane || destinationSuppressed, moved->AgentPrewarmSuppressed());
        });
    }

    void TabTests::ContentTransferReviewClosedAgentSuppressionMovesWithTab()
    {
        _verifyContentTransferReviewSuppression(false);
    }

    void TabTests::ContentTransferReviewSinglePaneKeepsDestinationSuppression()
    {
        _verifyContentTransferReviewSuppression(true);
    }

    void TabTests::ContentTransferReviewSinglePaneKeepsSuppressedDestination()
    {
        _verifyContentTransferReviewSuppression(true, true);
    }

    void TabTests::AttachContentStopsAfterRejectedFirstAction()
    {
        const auto connection = winrt::make_self<TestConnection>(winrt::guid{},
                                                                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
        auto page = _commonSetup(*connection);
        TestOnUIThread([&]() {
            const auto originalTab = page->_GetFocusedTabImpl();
            const auto originalRoot = originalTab->GetRootPane();
            const auto originalContent = originalRoot->GetContent();
            ActionAndArgs rejected;
            rejected.Action(ShortcutAction::NewTab);
            rejected.Args(NewTabArgs{ NewTerminalArgs{ -1 } });
            ActionAndArgs following;
            following.Action(ShortcutAction::SplitPane);
            following.Args(SplitPaneArgs{ SplitType::Duplicate });
            auto actions = winrt::single_threaded_vector<ActionAndArgs>({ rejected, following });

            page->AttachContent(actions, 0);

            VERIFY_ARE_EQUAL(1u, page->_tabs.Size());
            VERIFY_IS_TRUE(page->_GetFocusedTabImpl() == originalTab);
            VERIFY_ARE_EQUAL(1, originalTab->GetLeafPaneCount());
            VERIFY_IS_TRUE(originalTab->GetRootPane() == originalRoot);
            VERIFY_IS_TRUE(originalRoot->GetContent() == originalContent);
            VERIFY_ARE_EQUAL(0u, connection->CloseCount());
        });
    }

    void TabTests::TransferredAgentContentRejectsStaleAndFailedAttach()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            using DragStash = winrt::TerminalApp::implementation::AgentPaneDragStash;
            const auto focusedTab = page->_GetFocusedTabImpl();
            auto destinationAgentPane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            destinationAgentPane->IsAgentPane(true);
            page->_SplitPane(focusedTab, SplitDirection::Left, 0.5f, destinationAgentPane);

            auto sourcePane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            const auto oldArgs = sourcePane->GetContent().GetNewTerminalArgs(BuildStartupKind::Content).as<NewTerminalArgs>();
            const auto contentId = oldArgs.ContentId();
            const auto oldTransferId = oldArgs.AgentPaneTransferId();
            page->_manager.Detach(sourcePane->GetTerminalControl());
            DragStash::Entry firstEntry;
            firstEntry.attachDisposition = DragStash::AttachDisposition::FirstPaneOfNewTab;
            firstEntry.transferId = oldTransferId;
            DragStash::Instance().Store(contentId, std::move(firstEntry));
            uint64_t currentTransferId = oldTransferId;
            auto cleanup = wil::scope_exit([&]() {
                DragStash::Instance().Take(contentId, currentTransferId);
            });
            auto firstReceive = page->_MakeTerminalPane(oldArgs, nullptr, nullptr);
            VERIFY_IS_NOT_NULL(firstReceive);
            const auto currentArgs = firstReceive->GetContent().GetNewTerminalArgs(BuildStartupKind::MovePane).as<NewTerminalArgs>();
            currentTransferId = currentArgs.AgentPaneTransferId();
            VERIFY_ARE_NOT_EQUAL(oldTransferId, currentTransferId);
            VERIFY_ARE_EQUAL(contentId, currentArgs.ContentId());
            page->_manager.Detach(firstReceive->GetTerminalControl());
            DragStash::Entry currentEntry;
            currentEntry.attachDisposition = DragStash::AttachDisposition::FirstPaneOfNewTab;
            currentEntry.originalTabId = L"second-source-tab";
            currentEntry.transferId = currentTransferId;
            DragStash::Instance().Store(contentId, std::move(currentEntry));

            VERIFY_THROWS_SPECIFIC(
                page->_MakeTerminalPane(oldArgs, nullptr, nullptr),
                winrt::hresult_error,
                [](const winrt::hresult_error& error) { return error.code() == E_ILLEGAL_METHOD_CALL; });
            const auto missingGenerationArgs = currentArgs.Copy().as<NewTerminalArgs>();
            missingGenerationArgs.AgentPaneTransferId(0);
            VERIFY_THROWS_SPECIFIC(
                page->_MakeTerminalPane(missingGenerationArgs, nullptr, nullptr),
                winrt::hresult_error,
                [](const winrt::hresult_error& error) { return error.code() == E_ILLEGAL_METHOD_CALL; });

            auto unavailablePane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            const auto unavailableArgs = unavailablePane->GetContent().GetNewTerminalArgs(BuildStartupKind::Content).as<NewTerminalArgs>();
            const auto unavailableContentId = unavailableArgs.ContentId();
            const auto unavailableTransferId = unavailableArgs.AgentPaneTransferId();
            unavailablePane->Shutdown();
            VERIFY_IS_NULL(page->_manager.TryLookupCore(unavailableContentId));
            DragStash::Entry unavailableEntry;
            unavailableEntry.transferId = unavailableTransferId;
            DragStash::Instance().Store(unavailableContentId, std::move(unavailableEntry));
            auto cleanupUnavailable = wil::scope_exit([&]() {
                DragStash::Instance().Take(unavailableContentId, unavailableTransferId);
            });
            VERIFY_THROWS_SPECIFIC(
                page->_MakeTerminalPane(unavailableArgs, nullptr, nullptr),
                winrt::hresult_error,
                [](const winrt::hresult_error& error) { return error.code() == E_INVALIDARG; });
            VERIFY_ARE_EQUAL(unavailableContentId, unavailableArgs.ContentId());
            VERIFY_IS_FALSE(DragStash::Instance().Take(unavailableContentId, unavailableTransferId).has_value());
            VERIFY_IS_TRUE(focusedTab->FindAgentPane() == destinationAgentPane);

            const auto finalReceive = page->_MakeTerminalPane(currentArgs, nullptr, nullptr);
            VERIFY_IS_NOT_NULL(finalReceive);
            VERIFY_IS_TRUE(finalReceive->IsAgentPane());
            VERIFY_ARE_EQUAL(contentId, finalReceive->GetTerminalControl().ContentId());
            const auto finalContent = finalReceive->GetContent().as<winrt::TerminalApp::AgentPaneContent>();
            const auto finalImpl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(finalContent);
            VERIFY_IS_TRUE(finalImpl->TakePendingRenameFromTabId() == L"second-source-tab");
            VERIFY_IS_TRUE(focusedTab->FindAgentPane() == destinationAgentPane);
            VERIFY_THROWS_SPECIFIC(
                page->_MakeTerminalPane(currentArgs, nullptr, nullptr),
                winrt::hresult_error,
                [](const winrt::hresult_error& error) { return error.code() == E_ILLEGAL_METHOD_CALL; });
            VERIFY_ARE_EQUAL(contentId, finalReceive->GetTerminalControl().ContentId());
        });
    }

    void TabTests::ClosingAgentPaneSuppressesPrewarm()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto focusedTab = page->_GetFocusedTabImpl();
            auto agentPane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            agentPane->IsAgentPane(true);
            page->_SplitPane(focusedTab, SplitDirection::Down, 0.3f, agentPane);
            VERIFY_IS_FALSE(focusedTab->AgentPrewarmSuppressed());
            page->_HandleClosePaneRequested(agentPane);
            VERIFY_IS_TRUE(focusedTab->AgentPrewarmSuppressed());
        });

        TestOnUIThread([&]() {
            const auto focusedTab = page->_GetFocusedTabImpl();
            VERIFY_IS_TRUE(focusedTab->FindAgentPane() == nullptr);
            VERIFY_ARE_EQUAL(1, focusedTab->GetLeafPaneCount());
            VERIFY_IS_TRUE(focusedTab->AgentPrewarmSuppressed());
            page->_tabsAwaitingPrewarm.emplace_back(focusedTab->get_weak());
            page->_PrewarmAgentPanesAfterStartup();
            VERIFY_IS_TRUE(page->_tabsAwaitingPrewarm.empty());
            VERIFY_IS_TRUE(focusedTab->FindAgentPane() == nullptr);
            VERIFY_IS_TRUE(focusedTab->AgentPrewarmSuppressed());

            // Model an explicit successful reopen without launching a helper.
            focusedTab->AllowAgentPrewarm();
            VERIFY_IS_FALSE(focusedTab->AgentPrewarmSuppressed());
            auto replacement = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            replacement->IsAgentPane(true);
            page->_SplitPane(focusedTab, SplitDirection::Down, 0.3f, replacement);
            focusedTab->StashAgentPane();
            VERIFY_IS_TRUE(focusedTab->HasStashedAgentPane());
            VERIFY_IS_FALSE(focusedTab->AgentPrewarmSuppressed());
        });
    }

    void TabTests::AgentPaneTeardownAllowsSynchronousRecreation()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            const auto normalContent = tab->GetRootPane()->GetContent();
            const auto normalContentId = tab->GetRootPane()->GetTerminalControl().ContentId();
            auto agentPane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            VERIFY_IS_TRUE(agentPane->IsAgentPane());
            page->_SplitPane(tab, SplitDirection::Down, 0.3f, agentPane);

            for (const bool hidden : { false, true })
            {
                if (hidden)
                {
                    tab->StashAgentPane();
                }
                VERIFY_ARE_EQUAL(hidden, tab->HasStashedAgentPane());
                const auto oldContent = tab->FindAgentPaneContent();
                VERIFY_IS_NOT_NULL(oldContent);
                const auto oldContentId = oldContent.GetTermControl().ContentId();
                const auto oldTransferId = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(oldContent)->TransferId();

                // Rebuilds reuse the tree immediately, without a dispatcher tick
                // to finish a close animation.
                page->_TeardownAgentPane(tab);
                VERIFY_IS_TRUE(tab->FindAgentPane() == nullptr);
                VERIFY_IS_NULL(tab->FindAgentPaneContent());
                VERIFY_ARE_EQUAL(1, tab->GetLeafPaneCount());
                VERIFY_IS_FALSE(tab->GetRootPane()->IsAgentPane());
                VERIFY_IS_TRUE(tab->GetRootPane()->GetContent() == normalContent);
                VERIFY_ARE_EQUAL(normalContentId, tab->GetRootPane()->GetTerminalControl().ContentId());
                VERIFY_IS_NULL(page->_manager.TryLookupCore(oldContentId));

                const auto replacement = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
                const auto replacementContent = replacement->GetContent().as<winrt::TerminalApp::AgentPaneContent>();
                page->_SplitPane(tab, SplitDirection::Down, 0.3f, replacement);
                VERIFY_ARE_EQUAL(2, tab->GetLeafPaneCount());
                VERIFY_IS_TRUE(tab->FindAgentPane() == replacement);
                VERIFY_IS_TRUE(tab->FindAgentPaneContent() == replacementContent);
                VERIFY_IS_FALSE(tab->GetRootPane()->IsAgentPane());
                VERIFY_IS_FALSE(tab->HasStashedAgentPane());
                VERIFY_ARE_NOT_EQUAL(oldContentId, replacementContent.GetTermControl().ContentId());
                VERIFY_ARE_NOT_EQUAL(oldTransferId, winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(replacementContent)->TransferId());
                VERIFY_IS_FALSE(tab->AgentPrewarmSuppressed());
            }
        });
    }

    NewTerminalArgs TabTests::_storeOwnedAgentTransfer(
        const winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage>& page,
        const TrackedAgentCore& tracked)
    {
        using Lifetime = winrt::TerminalApp::implementation::AgentPaneLifetime;
        using DragStash = winrt::TerminalApp::implementation::AgentPaneDragStash;
        NewTerminalArgs attachArgs;
        attachArgs.ContentId(tracked.core.Id());
        const auto source = page->_WrapInAgentPaneContent(page->_MakeTerminalPane(attachArgs, nullptr, nullptr));
        const auto content = source->GetContent().as<winrt::TerminalApp::AgentPaneContent>();
        const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(content);
        impl->AdoptLifetime(Lifetime{ {}, tracked.core });
        const auto args = content.GetNewTerminalArgs(BuildStartupKind::Content).as<NewTerminalArgs>();
        page->_manager.Detach(content.GetTermControl());
        DragStash::Entry entry;
        entry.originalTabId = L"owning-source-tab";
        entry.attachDisposition = DragStash::AttachDisposition::FirstPaneOfNewTab;
        entry.transferId = impl->TransferId();
        entry.lifetime = impl->TakeLifetime();
        DragStash::Instance().Store(args.ContentId(), std::move(entry));
        auto abortTransfer = wil::scope_exit([&]() {
            DragStash::Instance().Take(args.ContentId(), args.AgentPaneTransferId());
        });
        source->Shutdown();
        content.Close();
        VERIFY_ARE_EQUAL(0u, tracked.connection->CloseCount());
        VERIFY_ARE_EQUAL(0u, tracked.closeEvents->load());
        VERIFY_IS_TRUE(page->_manager.TryLookupCore(args.ContentId()) == tracked.core);
        abortTransfer.release();
        return args;
    }

    void TabTests::AgentPaneLifetimeMovesAndDestroysRealCoresOnce()
    {
        using Lifetime = winrt::TerminalApp::implementation::AgentPaneLifetime;
        _createContentManager();
        std::optional<TrackedAgentCore> retained;
        std::optional<TrackedAgentCore> displaced;
        std::optional<Lifetime> owner;
        DWORD uiThreadId{};
        TestOnUIThread([&]() {
            uiThreadId = GetCurrentThreadId();
            retained.emplace(*_contentManager);
            displaced.emplace(*_contentManager);
            Lifetime source{ {}, retained->core };
            Lifetime moved{ std::move(source) };
            owner.emplace(Lifetime{ {}, displaced->core });
            *owner = std::move(moved);
            source.Close();
            moved.Close();
        });
        VERIFY_IS_TRUE(displaced->connection->WaitForClose());
        VERIFY_ARE_EQUAL(1u, displaced->connection->CloseCount());
        VERIFY_ARE_EQUAL(1u, displaced->closeEvents->load());
        VERIFY_ARE_NOT_EQUAL(uiThreadId, displaced->connection->CloseThreadId());
        TestOnUIThread([&]() {
            VERIFY_IS_NULL(_contentManager->TryLookupCore(displaced->core.Id()));
            VERIFY_IS_TRUE(_contentManager->TryLookupCore(retained->core.Id()) == retained->core);
            VERIFY_ARE_EQUAL(0u, retained->connection->CloseCount());
            VERIFY_ARE_EQUAL(0u, retained->closeEvents->load());
            owner.reset();
        });
        VERIFY_IS_TRUE(retained->connection->WaitForClose());
        TestOnUIThread([&]() {
            VERIFY_IS_NULL(_contentManager->TryLookupCore(retained->core.Id()));
            VERIFY_ARE_EQUAL(1u, retained->connection->CloseCount());
            VERIFY_ARE_EQUAL(1u, retained->closeEvents->load());
            VERIFY_ARE_NOT_EQUAL(uiThreadId, retained->connection->CloseThreadId());
            VERIFY_ARE_EQUAL(1u, displaced->closeEvents->load());
            retained.reset();
            displaced.reset();
        });
    }

    void TabTests::AgentPaneLifetimeReceiveClosesRealCoreOnce()
    {
        using DragStash = winrt::TerminalApp::implementation::AgentPaneDragStash;
        const auto seed = winrt::make_self<TestConnection>(winrt::guid{},
                                                         winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
        auto page = _commonSetup(*seed);
        std::optional<TrackedAgentCore> tracked;
        TestOnUIThread([&]() {
            tracked.emplace(*_contentManager);
            const auto args = _storeOwnedAgentTransfer(page, *tracked);
            auto cleanup = wil::scope_exit([&]() {
                DragStash::Instance().Take(args.ContentId(), args.AgentPaneTransferId());
            });
            const auto received = page->_MakeTerminalPane(args, nullptr, nullptr);
            const auto content = received->GetContent().as<winrt::TerminalApp::AgentPaneContent>();
            VERIFY_IS_TRUE(content.GetTermControl().Connection() == *tracked->connection);
            VERIFY_ARE_EQUAL(0u, tracked->connection->CloseCount());
            VERIFY_IS_FALSE(DragStash::Instance().Take(args.ContentId(), args.AgentPaneTransferId()).has_value());
            received->Shutdown();
            content.Close();
            received->Shutdown();
            VERIFY_IS_NULL(page->_manager.TryLookupCore(args.ContentId()));
        });
        VERIFY_IS_TRUE(tracked->connection->WaitForClose());
        VERIFY_ARE_EQUAL(1u, tracked->connection->CloseCount());
        VERIFY_ARE_EQUAL(1u, tracked->closeEvents->load());
        VERIFY_ARE_EQUAL(0u, seed->CloseCount());
        TestOnUIThread([&]() { tracked.reset(); });
    }

    void TabTests::AgentPaneLifetimeFailedReceiveRetiresRealCore()
    {
        using DragStash = winrt::TerminalApp::implementation::AgentPaneDragStash;
        const auto seed = winrt::make_self<TestConnection>(winrt::guid{},
                                                         winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
        auto page = _commonSetup(*seed);
        std::optional<TrackedAgentCore> tracked;
        DWORD uiThreadId{};
        TestOnUIThread([&]() {
            uiThreadId = GetCurrentThreadId();
            tracked.emplace(*_contentManager);
            const auto args = _storeOwnedAgentTransfer(page, *tracked);
            auto cleanup = wil::scope_exit([&]() {
                DragStash::Instance().Take(args.ContentId(), args.AgentPaneTransferId());
            });
            // The receiver cannot resolve this core, but the transfer still
            // owns a live core registered with the source ContentManager.
            page->_manager = winrt::make<winrt::TerminalApp::implementation::ContentManager>();
            VERIFY_THROWS_SPECIFIC(
                page->_MakeTerminalPane(args, nullptr, nullptr),
                winrt::hresult_error,
                [](const winrt::hresult_error& error) { return error.code() == E_INVALIDARG; });
            VERIFY_IS_FALSE(DragStash::Instance().Take(args.ContentId(), args.AgentPaneTransferId()).has_value());
        });
        VERIFY_IS_TRUE(tracked->connection->WaitForClose());
        TestOnUIThread([&]() {
            VERIFY_IS_NULL(_contentManager->TryLookupCore(tracked->core.Id()));
            VERIFY_ARE_EQUAL(1u, tracked->connection->CloseCount());
            VERIFY_ARE_EQUAL(1u, tracked->closeEvents->load());
            VERIFY_ARE_NOT_EQUAL(uiThreadId, tracked->connection->CloseThreadId());
            VERIFY_ARE_EQUAL(0u, seed->CloseCount());
            tracked.reset();
        });
    }

    void TabTests::AgentPaneLifetimeTimeoutRetiresOnlyAbandonedRealCore()
    {
        using Lifetime = winrt::TerminalApp::implementation::AgentPaneLifetime;
        using DragStash = winrt::TerminalApp::implementation::AgentPaneDragStash;
        _createContentManager();
        std::optional<TrackedAgentCore> abandoned;
        std::optional<TrackedAgentCore> claimed;
        std::optional<TrackedAgentCore> replacement;
        std::optional<DragStash::Entry> claimedOwner;
        std::optional<DragStash::Entry> replacementOwner;
        DWORD uiThreadId{};
        auto cleanup = wil::scope_exit([&]() {
            if (abandoned)
            {
                DragStash::Instance().Take(abandoned->core.Id(), 1);
            }
            if (claimed)
            {
                DragStash::Instance().Take(claimed->core.Id(), 2);
            }
            if (replacement)
            {
                DragStash::Instance().Take(replacement->core.Id(), 3);
                DragStash::Instance().Take(replacement->core.Id(), 4);
            }
        });
        TestOnUIThread([&]() {
            uiThreadId = GetCurrentThreadId();
            abandoned.emplace(*_contentManager);
            claimed.emplace(*_contentManager);
            replacement.emplace(*_contentManager);
            const auto store = [](const TrackedAgentCore& tracked, const uint64_t transferId) {
                DragStash::Entry entry;
                entry.transferId = transferId;
                entry.lifetime = Lifetime{ {}, tracked.core };
                DragStash::Instance().Store(tracked.core.Id(), std::move(entry));
            };
            store(*claimed, 2);
            DragStash::ExpireAfterTimeout(claimed->core.Id(), 2);
            claimedOwner = DragStash::Instance().Take(claimed->core.Id(), 2);
            VERIFY_IS_TRUE(claimedOwner.has_value());
            VERIFY_IS_FALSE(DragStash::Instance().Take(claimed->core.Id(), 2).has_value());

            store(*replacement, 3);
            DragStash::ExpireAfterTimeout(replacement->core.Id(), 3);
            auto oldTransfer = DragStash::Instance().Take(replacement->core.Id(), 3);
            VERIFY_IS_TRUE(oldTransfer.has_value());
            oldTransfer->transferId = 4;
            DragStash::Instance().Store(replacement->core.Id(), std::move(*oldTransfer));
            VERIFY_IS_FALSE(DragStash::Instance().Take(replacement->core.Id(), 3).has_value());

            // Arm the positive control last. This is the real production
            // two-minute coroutine, not a manual Take standing in for expiry.
            store(*abandoned, 1);
            DragStash::ExpireAfterTimeout(abandoned->core.Id(), 1);
            VERIFY_ARE_EQUAL(0u, claimed->connection->CloseCount());
            VERIFY_ARE_EQUAL(0u, replacement->connection->CloseCount());
        });

        VERIFY_IS_TRUE(abandoned->connection->WaitForClose(150000));
        // Keep both negative controls alive beyond the real timer deadline,
        // allowing delayed thread-pool callbacks to expose an erroneous close.
        VERIFY_IS_FALSE(claimed->connection->WaitForClose(3000));
        VERIFY_IS_FALSE(replacement->connection->WaitForClose(3000));
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(1u, abandoned->connection->CloseCount());
            VERIFY_ARE_EQUAL(1u, abandoned->closeEvents->load());
            VERIFY_ARE_NOT_EQUAL(uiThreadId, abandoned->connection->CloseThreadId());
            VERIFY_IS_NULL(_contentManager->TryLookupCore(abandoned->core.Id()));
            VERIFY_IS_FALSE(DragStash::Instance().Take(abandoned->core.Id(), 1).has_value());
            VERIFY_IS_TRUE(_contentManager->TryLookupCore(claimed->core.Id()) == claimed->core);
            VERIFY_IS_TRUE(_contentManager->TryLookupCore(replacement->core.Id()) == replacement->core);
            VERIFY_ARE_EQUAL(0u, claimed->closeEvents->load());
            VERIFY_ARE_EQUAL(0u, replacement->closeEvents->load());
            replacementOwner = DragStash::Instance().Take(replacement->core.Id(), 4);
            VERIFY_IS_TRUE(replacementOwner.has_value());
            claimedOwner.reset();
            replacementOwner.reset();
        });
        VERIFY_IS_TRUE(claimed->connection->WaitForClose());
        VERIFY_IS_TRUE(replacement->connection->WaitForClose());
        TestOnUIThread([&]() {
            VERIFY_IS_NULL(_contentManager->TryLookupCore(claimed->core.Id()));
            VERIFY_IS_NULL(_contentManager->TryLookupCore(replacement->core.Id()));
            VERIFY_ARE_EQUAL(1u, claimed->connection->CloseCount());
            VERIFY_ARE_EQUAL(1u, replacement->connection->CloseCount());
            VERIFY_ARE_EQUAL(1u, claimed->closeEvents->load());
            VERIFY_ARE_EQUAL(1u, replacement->closeEvents->load());
            abandoned.reset();
            claimed.reset();
            replacement.reset();
        });
    }

    void TabTests::SourceTerminalPaneSkipsAgentPane()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto focusedTab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(focusedTab);

            const auto terminalPane = focusedTab->GetActivePane();
            VERIFY_IS_NOT_NULL(terminalPane);
            VERIFY_IS_FALSE(terminalPane->IsAgentPane());

            // The real agent pane runs the hidden "Agent Pane" profile, which
            // is `closeOnExit: always`. Stand in for it with a profile that is
            // not the tab's (and not the global default) so the assertions
            // below can tell the two apart.
            NewTerminalArgs agentArgs{ 1 };
            auto agentPane = page->_WrapInAgentPaneContent(page->_MakePane(agentArgs, nullptr, nullptr));
            VERIFY_IS_NOT_NULL(agentPane);
            agentPane->IsAgentPane(true);

            // `_SplitPane` focuses the new pane, exactly like clicking into the
            // agent pane or driving its session picker does.
            page->_SplitPane(focusedTab, SplitDirection::Down, 0.3f, agentPane);
            VERIFY_IS_NOT_NULL(focusedTab->GetActivePane());
            VERIFY_IS_TRUE(focusedTab->GetActivePane()->IsAgentPane());

            // This is the state that used to leak the agent pane's identity
            // into protocol tabs, delegate agent selection, and delegate cwd.
            const auto focusedProfile = focusedTab->GetFocusedProfile();
            VERIFY_IS_NOT_NULL(focusedProfile);
            VERIFY_ARE_EQUAL(L"profile1", focusedProfile.Name());

            const auto sourcePane = page->_SourceTerminalPaneForTab(focusedTab);
            VERIFY_IS_NOT_NULL(sourcePane);
            VERIFY_IS_FALSE(sourcePane->IsAgentPane());
            VERIFY_IS_TRUE(sourcePane == terminalPane);

            // The source pane is deliberately not `_lastActive`, so resolving
            // its profile must not go through `Pane::GetFocusedProfile()` —
            // that returns null here, which is what silently dropped the
            // profile's `commandPaletteAgent`.
            VERIFY_IS_NULL(sourcePane->GetFocusedProfile());

            const auto sourceProfile = page->_SourceTerminalProfileForTab(focusedTab);
            VERIFY_IS_NOT_NULL(sourceProfile);
            VERIFY_ARE_EQUAL(L"profile0", sourceProfile.Name());
        });
    }

    void TabTests::AgentPaneIndicatorsIgnoreAgentPaneInPaneCount()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto focusedTab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(focusedTab);

            auto agentPane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            VERIFY_IS_NOT_NULL(agentPane);
            agentPane->IsAgentPane(true);
            page->_SplitPane(focusedTab, SplitDirection::Down, 0.3f, agentPane);

            int shellPaneCount = 0;
            focusedTab->GetRootPane()->WalkTree([&](const auto& pane) {
                if (pane->GetContent() && !pane->IsAgentPane())
                {
                    ++shellPaneCount;
                    VERIFY_IS_FALSE(pane->_focusBorderEnabled);
                    VERIFY_IS_TRUE(!pane->_agentChip ||
                                   pane->_agentChip.Visibility() == Visibility::Collapsed);
                }
                if (pane->IsAgentPane())
                {
                    VERIFY_IS_FALSE(pane->_focusBorderEnabled);
                }
            });
            VERIFY_ARE_EQUAL(1, shellPaneCount);

            const auto agentPaneId = agentPane->Id();
            VERIFY_IS_TRUE(agentPaneId.has_value());
            const auto shellPane = page->_SourceTerminalPaneForTab(focusedTab);
            VERIFY_IS_NOT_NULL(shellPane);
            VERIFY_IS_TRUE(shellPane->Id().has_value());
            VERIFY_IS_TRUE(focusedTab->FocusPane(shellPane->Id().value()));

            page->_SplitPane(
                focusedTab,
                SplitDirection::Right,
                0.5f,
                page->_MakePane(nullptr, page->_GetFocusedTab(), nullptr));
            const auto secondShellPane = focusedTab->GetActivePane();
            VERIFY_IS_NOT_NULL(secondShellPane);
            VERIFY_IS_FALSE(secondShellPane->IsAgentPane());
            VERIFY_IS_TRUE(focusedTab->FocusPane(agentPaneId.value()));

            shellPaneCount = 0;
            int visibleChipCount = 0;
            focusedTab->GetRootPane()->WalkTree([&](const auto& pane) {
                if (pane->GetContent() && !pane->IsAgentPane())
                {
                    ++shellPaneCount;
                    VERIFY_IS_TRUE(pane->_focusBorderEnabled);
                    if (pane->_agentChip &&
                        pane->_agentChip.Visibility() == Visibility::Visible)
                    {
                        ++visibleChipCount;
                    }
                }
                if (pane->IsAgentPane())
                {
                    VERIFY_IS_FALSE(pane->_focusBorderEnabled);
                }
            });
            VERIFY_ARE_EQUAL(2, shellPaneCount);
            VERIFY_ARE_EQUAL(1, visibleChipCount);

            const auto verifyChipTarget = [&](const std::shared_ptr<Pane>& expectedTarget) {
                int visibleChips = 0;
                focusedTab->GetRootPane()->WalkTree([&](const auto& pane) {
                    if (pane->_agentChip &&
                        pane->_agentChip.Visibility() == Visibility::Visible)
                    {
                        ++visibleChips;
                        VERIFY_IS_TRUE(pane == expectedTarget);
                    }
                });
                VERIFY_ARE_EQUAL(1, visibleChips);
            };

            // A protocol override must move the chip to the requested terminal.
            focusedTab->SetAgentChipOverride(shellPane->GetSessionId());
            verifyChipTarget(shellPane);
            focusedTab->SetAgentChipOverride(secondShellPane->GetSessionId());
            verifyChipTarget(secondShellPane);
            focusedTab->SetAgentChipOverride(std::nullopt);
            verifyChipTarget(secondShellPane);

            // Hiding one of the two shell panes makes the target unambiguous.
            VERIFY_IS_TRUE(secondShellPane->Id().has_value());
            VERIFY_IS_TRUE(focusedTab->FocusPane(secondShellPane->Id().value()));
            focusedTab->HidePane();
            focusedTab->GetRootPane()->WalkTree([&](const auto& pane) {
                if (!pane->IsHidden() &&
                    pane->GetContent().try_as<winrt::TerminalApp::TerminalPaneContent>())
                {
                    VERIFY_IS_FALSE(pane->_focusBorderEnabled);
                    VERIFY_IS_TRUE(!pane->_agentChip ||
                                   pane->_agentChip.Visibility() == Visibility::Collapsed);
                }
            });

            // Restoring the shell must immediately restore multi-pane focus
            // borders, even though ShowPane does not move focus.
            focusedTab->ShowPane();
            shellPaneCount = 0;
            visibleChipCount = 0;
            focusedTab->GetRootPane()->WalkTree([&](const auto& pane) {
                if (!pane->IsHidden() &&
                    pane->GetContent().try_as<winrt::TerminalApp::TerminalPaneContent>())
                {
                    ++shellPaneCount;
                    VERIFY_IS_TRUE(pane->_focusBorderEnabled);
                    if (pane->_agentChip &&
                        pane->_agentChip.Visibility() == Visibility::Visible)
                    {
                        ++visibleChipCount;
                    }
                }
            });
            VERIFY_ARE_EQUAL(2, shellPaneCount);
            VERIFY_ARE_EQUAL(1, visibleChipCount);
        });
    }

    void TabTests::AgentPaneIndicatorsIgnoreNonTerminalPanes()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto focusedTab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(focusedTab);

            auto agentPane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            VERIFY_IS_NOT_NULL(agentPane);
            agentPane->IsAgentPane(true);
            page->_SplitPane(focusedTab, SplitDirection::Down, 0.3f, agentPane);

            const auto shellPane = page->_SourceTerminalPaneForTab(focusedTab);
            VERIFY_IS_NOT_NULL(shellPane);
            VERIFY_IS_TRUE(shellPane->Id().has_value());
            VERIFY_IS_TRUE(focusedTab->FocusPane(shellPane->Id().value()));

            const auto snippetsArgs = BaseContentArgs{ L"snippets" };
            const auto snippetsPane = page->_MakePane(snippetsArgs, page->_GetFocusedTab(), nullptr);
            VERIFY_IS_NOT_NULL(snippetsPane);
            page->_SplitPane(focusedTab, SplitDirection::Right, 0.5f, snippetsPane);

            VERIFY_IS_TRUE(agentPane->Id().has_value());
            VERIFY_IS_TRUE(focusedTab->FocusPane(agentPane->Id().value()));

            int terminalPaneCount = 0;
            int nonTerminalPaneCount = 0;
            int visibleChipCount = 0;
            focusedTab->GetRootPane()->WalkTree([&](const auto& pane) {
                if (pane->GetContent().try_as<winrt::TerminalApp::TerminalPaneContent>())
                {
                    ++terminalPaneCount;
                    VERIFY_IS_FALSE(pane->_focusBorderEnabled);
                }
                else if (pane->GetContent() && !pane->IsAgentPane())
                {
                    ++nonTerminalPaneCount;
                    VERIFY_IS_TRUE(pane->_focusBorderEnabled);
                }

                if (pane->_agentChip &&
                    pane->_agentChip.Visibility() == Visibility::Visible)
                {
                    ++visibleChipCount;
                }
            });

            VERIFY_ARE_EQUAL(1, terminalPaneCount);
            VERIFY_ARE_EQUAL(1, nonTerminalPaneCount);
            VERIFY_ARE_EQUAL(0, visibleChipCount);
        });
    }

    void TabTests::AgentPaneIndicatorsRefreshAfterNonActivePaneClose()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto focusedTab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(focusedTab);

            auto agentPane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            VERIFY_IS_NOT_NULL(agentPane);
            agentPane->IsAgentPane(true);
            page->_SplitPane(focusedTab, SplitDirection::Down, 0.3f, agentPane);

            const auto remainingShellPane = page->_SourceTerminalPaneForTab(focusedTab);
            VERIFY_IS_NOT_NULL(remainingShellPane);
            VERIFY_IS_TRUE(remainingShellPane->Id().has_value());
            VERIFY_IS_TRUE(focusedTab->FocusPane(remainingShellPane->Id().value()));

            page->_SplitPane(
                focusedTab,
                SplitDirection::Right,
                0.5f,
                page->_MakePane(nullptr, page->_GetFocusedTab(), nullptr));
            const auto closingShellPane = focusedTab->GetActivePane();
            VERIFY_IS_NOT_NULL(closingShellPane);

            VERIFY_IS_TRUE(agentPane->Id().has_value());
            VERIFY_IS_TRUE(focusedTab->FocusPane(agentPane->Id().value()));
            page->_HandleClosePaneRequested(closingShellPane);
            VERIFY_ARE_EQUAL(2, focusedTab->GetLeafPaneCount());

            // The closed pane event is raised before Pane finishes collapsing
            // its parent, so no synchronous recomputation can observe the
            // final one-terminal tree.
            VERIFY_IS_TRUE(remainingShellPane->_focusBorderEnabled);

            focusedTab->_UpdateAgentPaneIndicators();

            int terminalPaneCount = 0;
            int visibleChipCount = 0;
            focusedTab->GetRootPane()->WalkTree([&](const auto& pane) {
                if (pane->GetContent().try_as<winrt::TerminalApp::TerminalPaneContent>())
                {
                    ++terminalPaneCount;
                    VERIFY_IS_FALSE(pane->_focusBorderEnabled);
                }
                if (pane->_agentChip &&
                    pane->_agentChip.Visibility() == Visibility::Visible)
                {
                    ++visibleChipCount;
                }
            });

            VERIFY_ARE_EQUAL(1, terminalPaneCount);
            VERIFY_ARE_EQUAL(0, visibleChipCount);
        });
    }

    void TabTests::_verifyBuildStartupActionsContentPreservesAgentOwnership(const SplitDirection splitDirection,
                                                                           const bool hidden)
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto focusedTab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(focusedTab);

            const auto oldTabId = focusedTab->StableId();
            const auto sourceProfileGuid = winrt::guid{ L"{6239a42c-7777-49a3-80bd-e8fdd045185c}" };
            focusedTab->AgentSourceProfileGuid(sourceProfileGuid);

            auto agentPane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            VERIFY_IS_NOT_NULL(agentPane);
            agentPane->IsAgentPane(true);
            page->_SplitPane(focusedTab, splitDirection, 0.5f, agentPane);
            const auto agentContent = agentPane->GetContent().as<winrt::TerminalApp::AgentPaneContent>();
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(agentContent);
            impl->SetAgentSessionId(L"live-agent-session");
            impl->SetSessionsView(true);
            impl->SetAgentPanePosition(L"left");
            if (hidden)
            {
                focusedTab->StashAgentPane();
            }

            const auto agentContentArgs = agentPane->GetContent().GetNewTerminalArgs(BuildStartupKind::Content).try_as<NewTerminalArgs>();
            VERIFY_IS_NOT_NULL(agentContentArgs);
            const auto agentContentId = agentContentArgs.ContentId();
            VERIFY_ARE_NOT_EQUAL(0ull, agentContentId);
            const auto transferId = impl->TransferId();
            VERIFY_ARE_NOT_EQUAL(0ull, transferId);
            const auto core = page->_manager.TryLookupCore(agentContentId);
            VERIFY_IS_NOT_NULL(core);

            for (const auto kind : { BuildStartupKind::Content, BuildStartupKind::MovePane, BuildStartupKind::Content })
            {
                const auto actions = focusedTab->BuildStartupActions(kind);
                VERIFY_IS_TRUE(actions.size() >= 2);
                VERIFY_ARE_EQUAL(ShortcutAction::NewTab, actions.at(0).Action());
                const auto firstPaneArgs = _getTerminalArgs(actions.at(0));
                VERIFY_IS_NOT_NULL(firstPaneArgs);
                NewTerminalArgs serializedAgentArgs{ nullptr };
                if (splitDirection == SplitDirection::Left)
                {
                    VERIFY_ARE_EQUAL(agentContentId, firstPaneArgs.ContentId());
                    serializedAgentArgs = firstPaneArgs;
                }
                else
                {
                    VERIFY_ARE_NOT_EQUAL(agentContentId, firstPaneArgs.ContentId());
                    VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actions.at(1).Action());
                    serializedAgentArgs = _getTerminalArgs(actions.at(1));
                }
                VERIFY_IS_NOT_NULL(serializedAgentArgs);
                VERIFY_ARE_EQUAL(agentContentId, serializedAgentArgs.ContentId());
                VERIFY_IS_TRUE(serializedAgentArgs.Type() == (hidden ? L"agentStashed" : L"agent"));
                VERIFY_ARE_EQUAL(transferId, serializedAgentArgs.AgentPaneTransferId());
                VERIFY_IS_FALSE(winrt::TerminalApp::implementation::AgentPaneDragStash::Instance().Take(agentContentId, transferId).has_value());
                VERIFY_IS_TRUE(focusedTab->FindAgentPane() == agentPane);
                VERIFY_IS_TRUE(focusedTab->FindAgentPaneContent() == agentContent);
                VERIFY_IS_TRUE(page->_manager.TryLookupCore(agentContentId) == core);
                VERIFY_ARE_EQUAL(agentContentId, agentContent.GetTermControl().ContentId());
                VERIFY_ARE_EQUAL(hidden, focusedTab->HasStashedAgentPane());
                VERIFY_IS_TRUE(focusedTab->StableId() == oldTabId);
                VERIFY_IS_TRUE(focusedTab->AgentSourceProfileGuid().value() == sourceProfileGuid);
                VERIFY_IS_TRUE(impl->AgentSessionId() == L"live-agent-session");
                VERIFY_IS_TRUE(impl->IsSessionsView());
                VERIFY_IS_TRUE(impl->GetAgentPanePosition() == L"left");
                VERIFY_IS_FALSE(focusedTab->AgentPrewarmSuppressed());
            }
            if (hidden)
            {
                VERIFY_IS_TRUE(focusedTab->RestoreStashedAgentPane(splitDirection));
                VERIFY_IS_FALSE(focusedTab->HasStashedAgentPane());
                VERIFY_IS_TRUE(focusedTab->FindAgentPaneContent() == agentContent);
                VERIFY_ARE_EQUAL(agentContentId, agentContent.GetTermControl().ContentId());
                VERIFY_ARE_EQUAL(transferId, impl->TransferId());
                VERIFY_IS_TRUE(impl->AgentSessionId() == L"live-agent-session");
            }
        });
    }

    void TabTests::AgentPaneTransferIdentityRoundTripsWithContent()
    {
        TestOnUIThread([]() {
            NewTerminalArgs args;
            args.ContentId(7);
            args.AgentPaneTransferId(11);
            args.SetContentType(winrt::hstring{ ::Microsoft::Terminal::AgentPaneRestore::PaneType });
            ActionAndArgs action;
            action.Action(ShortcutAction::NewTab);
            action.Args(NewTabArgs{ args });
            const auto json = ActionAndArgs::Serialize(winrt::single_threaded_vector<ActionAndArgs>({ action }));
            const auto actions = ActionAndArgs::Deserialize(json);
            VERIFY_ARE_EQUAL(uint32_t{ 1 }, actions.Size());
            const auto restored = _getTerminalArgs(actions.GetAt(0));
            VERIFY_IS_NOT_NULL(restored);
            VERIFY_ARE_EQUAL(uint64_t{ 7 }, restored.ContentId());
            VERIFY_ARE_EQUAL(uint64_t{ 11 }, restored.AgentPaneTransferId());
            VERIFY_IS_TRUE(args.Equals(restored));
            const auto copy = args.Copy().as<NewTerminalArgs>();
            VERIFY_IS_TRUE(args.Equals(copy));
            VERIFY_ARE_EQUAL(args.Hash(), copy.Hash());
            copy.AgentPaneTransferId(12);
            VERIFY_IS_FALSE(args.Equals(copy));
        });
    }

    void TabTests::AgentPaneTransferIdentityIsNotPersistedByDefault()
    {
        TestOnUIThread([]() {
            NewTerminalArgs args;
            ActionAndArgs action;
            action.Action(ShortcutAction::NewTab);
            action.Args(NewTabArgs{ args });
            const auto json = winrt::to_string(ActionAndArgs::Serialize(winrt::single_threaded_vector<ActionAndArgs>({ action })));
            VERIFY_IS_TRUE(json.find("__agentPaneTransfer") == std::string::npos);
        });
    }

    void TabTests::BuildStartupActionsContentPreservesAgentFirstPaneOwnership()
    {
        _verifyBuildStartupActionsContentPreservesAgentOwnership(SplitDirection::Left);
    }

    void TabTests::BuildStartupActionsContentPreservesAgentLaterSplitOwnership()
    {
        _verifyBuildStartupActionsContentPreservesAgentOwnership(SplitDirection::Right);
    }

    void TabTests::BuildStartupActionsContentPreservesHiddenAgentIdentity()
    {
        _verifyBuildStartupActionsContentPreservesAgentOwnership(SplitDirection::Right, true);
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

            auto transferredSourcePane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            VERIFY_IS_NOT_NULL(transferredSourcePane);
            const auto transferredControl = transferredSourcePane->GetTerminalControl();
            VERIFY_IS_NOT_NULL(transferredControl);
            const auto contentId = transferredControl.ContentId();
            const auto newTerminalArgs = transferredSourcePane->GetContent().GetNewTerminalArgs(BuildStartupKind::Content).as<NewTerminalArgs>();
            const auto transferId = newTerminalArgs.AgentPaneTransferId();
            page->_manager.Detach(transferredControl);

            using DragStash = winrt::TerminalApp::implementation::AgentPaneDragStash;
            DragStash::Entry entry;
            entry.originalTabId = oldTabId;
            entry.sourceProfileGuid = sourceProfileGuid;
            entry.attachDisposition = DragStash::AttachDisposition::ExistingTabSplit;
            entry.panePosition = L"right";
            entry.transferId = transferId;
            DragStash::Instance().Store(contentId, std::move(entry));
            auto cleanup = wil::scope_exit([&]() {
                DragStash::Instance().Take(contentId, transferId);
            });

            auto transferredPane = page->_MakeTerminalPane(newTerminalArgs, nullptr, nullptr);
            VERIFY_IS_NOT_NULL(transferredPane);
            VERIFY_IS_TRUE(transferredPane->IsAgentPane());

            VERIFY_IS_TRUE(focusedTab->FindAgentPane() == nullptr);
            VERIFY_IS_TRUE(focusedTab->AgentSourceProfileGuid().has_value());
            VERIFY_IS_TRUE(focusedTab->AgentSourceProfileGuid().value() == sourceProfileGuid);

            const auto agentContent = transferredPane->GetContent().try_as<winrt::TerminalApp::AgentPaneContent>();
            VERIFY_IS_NOT_NULL(agentContent);
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(agentContent);
            VERIFY_ARE_EQUAL(contentId, transferredPane->GetTerminalControl().ContentId());
            VERIFY_ARE_NOT_EQUAL(transferId, impl->TransferId());
            VERIFY_IS_FALSE(impl->AwaitingTransferredTabContent());
            VERIFY_IS_FALSE(impl->TakeHiddenAfterTransfer());
            VERIFY_IS_TRUE(impl->GetAgentPanePosition() == L"right");
            VERIFY_IS_FALSE(DragStash::Instance().Take(contentId, transferId).has_value());
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
                    if (Json::parseFromStream(readerBuilder, stream, &event, &errors) &&
                        (event["method"].asString() == "tab_renamed" || event["method"].asString() == "set_agent_state"))
                    {
                        protocolEvents.emplace_back(std::move(event));
                    }
                });
            auto revokeProtocolEvents = wil::scope_exit([&]() {
                page->ProtocolVtSequenceReceived(token);
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
            VERIFY_IS_NULL(impl->GetRoot().FindName(L"AgentYoloStatusText"));
            VERIFY_ARE_EQUAL(2u, protocolEvents.size());
            VERIFY_IS_TRUE(protocolEvents[0]["method"].asString() == "tab_renamed");
            VERIFY_IS_TRUE(protocolEvents[0]["params"]["old_tab_id"].asString() == winrt::to_string(oldTabId));
            VERIFY_IS_TRUE(protocolEvents[0]["params"]["new_tab_id"].asString() == winrt::to_string(focusedTab->StableId()));
            VERIFY_IS_TRUE(protocolEvents[1]["method"].asString() == "set_agent_state");
            VERIFY_IS_TRUE(protocolEvents[1]["params"]["tab_id"].asString() == winrt::to_string(focusedTab->StableId()));
            VERIFY_IS_TRUE(protocolEvents[1]["params"]["view"].asString() == "chat");
            VERIFY_IS_TRUE(protocolEvents[1]["params"]["pane_open"].asBool());
            VERIFY_IS_TRUE(impl->TransferSourceTabId().empty());

            impl->SetAgentSessionId(L"old-session");
            impl->SetYoloControlOwner(L"manual");
            impl->SetAgentSessionId(L"new-session");
            VERIFY_IS_TRUE(impl->YoloControlOwner().empty());

            sendStatus(focusedTab->StableId(), "model-b");

            VERIFY_IS_TRUE(impl->GetAgentModel() == L"model-b");
            VERIFY_ARE_EQUAL(2u, protocolEvents.size());

        });
    }

    void TabTests::PendingAgentOpenSurvivesStartupProjection()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto focusedTab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(focusedTab);

            auto agentPane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            VERIFY_IS_NOT_NULL(agentPane);
            agentPane->IsAgentPane(true);
            page->_SplitPane(focusedTab, SplitDirection::Left, 0.5f, agentPane);

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

            page->_RequestAgentStateForTab(focusedTab, "chat", /*pane_open*/ true);
            VERIFY_ARE_EQUAL(1u, protocolEvents.size());

            Json::Value stale{ Json::objectValue };
            stale["type"] = "event";
            stale["method"] = "agent_state_changed";
            stale["params"]["tab_id"] = winrt::to_string(focusedTab->StableId());
            stale["params"]["agent_session_id"] = "agent-session-1";
            stale["params"]["yolo_control_owner"] = "manual";
            stale["params"]["view"] = "chat";
            stale["params"]["pane_open"] = false;
            Json::StreamWriterBuilder writerBuilder;
            writerBuilder["indentation"] = "";
            page->OnAgentStateChanged(winrt::to_hstring(Json::writeString(writerBuilder, stale)));
            VERIFY_IS_FALSE(focusedTab->HasStashedAgentPane());
            const auto agentContent = focusedTab->FindAgentPaneContent();
            VERIFY_IS_NOT_NULL(agentContent);
            const auto agentImpl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(agentContent);
            VERIFY_IS_TRUE(agentImpl->AgentSessionId() == L"agent-session-1");
            VERIFY_IS_TRUE(agentImpl->YoloControlOwner() == L"manual");

            Json::Value ready{ Json::objectValue };
            ready["type"] = "event";
            ready["method"] = "agent_status";
            ready["params"]["agent_id"] = "copilot";
            ready["params"]["name"] = "";
            ready["params"]["state"] = "connecting";
            ready["params"]["tab_id"] = winrt::to_string(focusedTab->StableId());
            page->OnAgentStatusChanged(winrt::to_hstring(Json::writeString(writerBuilder, ready)));

            VERIFY_ARE_EQUAL(2u, protocolEvents.size());
            VERIFY_IS_TRUE(protocolEvents[1]["method"].asString() == "set_agent_state");
            VERIFY_IS_TRUE(protocolEvents[1]["params"]["view"].asString() == "chat");
            VERIFY_IS_TRUE(protocolEvents[1]["params"]["pane_open"].asBool());
            page->ProtocolVtSequenceReceived(token);
        });
    }

    void TabTests::InitialSessionsViewSurvivesStartupProjection()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto focusedTab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(focusedTab);

            auto agentPane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            VERIFY_IS_NOT_NULL(agentPane);
            agentPane->IsAgentPane(true);
            page->_SplitPane(focusedTab, SplitDirection::Left, 0.5f, agentPane);
            const auto content = focusedTab->FindAgentPaneContent();
            VERIFY_IS_NOT_NULL(content);
            content.SetSessionsView(true);

            Json::Value stale{ Json::objectValue };
            stale["type"] = "event";
            stale["method"] = "agent_state_changed";
            stale["params"]["tab_id"] = winrt::to_string(focusedTab->StableId());
            stale["params"]["view"] = "chat";
            stale["params"]["pane_open"] = false;
            Json::StreamWriterBuilder writerBuilder;
            writerBuilder["indentation"] = "";
            page->OnAgentStateChanged(winrt::to_hstring(Json::writeString(writerBuilder, stale)));

            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(content);
            VERIFY_IS_TRUE(impl->IsSessionsView());
            VERIFY_IS_FALSE(focusedTab->HasStashedAgentPane());

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

            Json::Value ready{ Json::objectValue };
            ready["type"] = "event";
            ready["method"] = "agent_status";
            ready["params"]["agent_id"] = "copilot";
            ready["params"]["name"] = "";
            ready["params"]["state"] = "connecting";
            ready["params"]["tab_id"] = winrt::to_string(focusedTab->StableId());
            page->OnAgentStatusChanged(winrt::to_hstring(Json::writeString(writerBuilder, ready)));

            VERIFY_ARE_EQUAL(1u, protocolEvents.size());
            VERIFY_IS_TRUE(protocolEvents[0]["method"].asString() == "set_agent_state");
            VERIFY_IS_TRUE(protocolEvents[0]["params"]["view"].asString() == "sessions");
            VERIFY_IS_TRUE(protocolEvents[0]["params"]["pane_open"].asBool());
            page->ProtocolVtSequenceReceived(token);
        });
    }

    void TabTests::AgentReadyRuntimeConfigIncludesCurrentYoloState()
    {
        winrt::TerminalApp::implementation::TerminalPage::AgentRuntimeConfigSnapshot config;
        config.yoloEnabled = false;
        config.yoloPolicyBlocked = true;

        const auto payload = winrt::TerminalApp::implementation::TerminalPage::_BuildAgentReadyRuntimeConfigPayload(
            "tab-a",
            "42",
            config);

        VERIFY_ARE_EQUAL("tab-a", payload["tab_id"].asString());
        VERIFY_ARE_EQUAL("42", payload["window_id"].asString());
        VERIFY_IS_TRUE(payload["automatic_yolo_target"].isBool());
        VERIFY_IS_FALSE(payload["automatic_yolo_target"].asBool());
        VERIFY_IS_TRUE(payload["yolo_enabled"].isBool());
        VERIFY_IS_FALSE(payload["yolo_enabled"].asBool());
        VERIFY_IS_TRUE(payload["yolo_policy_blocked"].isBool());
        VERIFY_IS_TRUE(payload["yolo_policy_blocked"].asBool());
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
