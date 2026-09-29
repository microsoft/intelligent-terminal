// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include "../TerminalApp/TerminalPage.h"
#include "../TerminalApp/TerminalWindow.h"
#include "../TerminalApp/SettingsLoadEventArgs.h"
#include "../TerminalApp/MinMaxCloseControl.h"
#include "../TerminalApp/TabRowControl.h"
#include "../TerminalApp/TabStrip.h"
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
#include "../inc/AgentPaneBackend.h"
#include "../UnitTests_Control/MockControlSettings.h"
#include "CppWinrtTailored.h"

#include <cmath>
#include <winrt/Windows.UI.Xaml.Automation.h>
#include <winrt/Windows.UI.Xaml.Automation.Peers.h>
#include <winrt/Windows.UI.Xaml.Automation.Provider.h>
#include <winrt/Windows.UI.Xaml.Media.Imaging.h>
#include <winrt/Windows.UI.Xaml.Shapes.h>

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
        winrt::TerminalApp::implementation::Tab::AgentOverrideOrigin agentOverrideOrigin{
            winrt::TerminalApp::implementation::Tab::AgentOverrideOrigin::User
        };
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
        TEST_METHOD(FreTabModeSelectionDoesNotMutateSettings);
        TEST_METHOD(FreIllustrationsFollowThemeWithoutChangingChrome);
        TEST_METHOD(EmptyTabLayoutChangeCompletesBeforeStartup);
        TEST_METHOD(VerticalRailVisibilityRestoresWidth);
        TEST_METHOD(VerticalRailCollapseRestoresWidth);
        TEST_METHOD(VerticalTabChromeBackgroundTracksTheme);
        TEST_METHOD(NewTabButtonSharesChromeBackdrop);
        TEST_METHOD(VerticalTabStripBindsBackground);
        TEST_METHOD(VerticalTabHistorySharesBackdrop);
        TEST_METHOD(LiveTabLayoutRoundTripPreservesState);
        TEST_METHOD(LiveTabLayoutLatestRequestWins);
        TEST_METHOD(TabLayoutSwitchMenuTracksOrientation);
        TEST_METHOD(VerticalTabStripPreservesClosePolicy);
        TEST_METHOD(VerticalTabStripCollapsedItemsPreserveSelection);
        TEST_METHOD(VerticalTabStripHostsPaneGroups);
        TEST_METHOD(VerticalTabSelectionPreservesPresentation);
        TEST_METHOD(VerticalTabIconChangesUpdatePresentation);
        TEST_METHOD(VerticalTabThemeChangesDoNotReprojectPanes);
        TEST_METHOD(VerticalTabColorsFollowSidebarTheme);
        TEST_METHOD(VerticalTabStripUsesNativeInteractionStates);
        TEST_METHOD(VerticalTabGroupingIgnoresAgentPane);
        TEST_METHOD(AgentViewFiltersSplitPaneChildren);
        TEST_METHOD(VerticalTabSearchMatchesCommittedTitle);
        TEST_METHOD(VerticalTabSearchTracksActivePaneMetadata);
        TEST_METHOD(VerticalTabSearchUiState);
        TEST_METHOD(LiteralSearchHighlighting);
        TEST_METHOD(VerticalTabHistoryButtonOpensView);
        TEST_METHOD(VerticalTabHistoryCloseStopsRefresh);
        TEST_METHOD(VerticalTabFilterContainsOnlyMetadata);
        TEST_METHOD(VerticalTabHistoryStatusText);
        TEST_METHOD(SessionRegistryStatusDeltaUpdatesCaches);
        TEST_METHOD(BottomBarSessionsButtonFollowsLayout);
        TEST_METHOD(BottomBarSessionsButtonDispatchesExistingAction);
        TEST_METHOD(BottomBarSessionsButtonTracksVisibleView);
        TEST_METHOD(VerticalTabHistoryRelativeAge);
        TEST_METHOD(VerticalTabHistoryMetadataLayout);
        TEST_METHOD(VerticalTabHistoryWslDistroMetadata);
        TEST_METHOD(VerticalTabHistoryCurrentSessionTracksPane);
        TEST_METHOD(VerticalTabHistoryCurrentSessionColors);
        TEST_METHOD(VerticalTabHistoryAgentIcons);
        TEST_METHOD(VerticalTabHistoryEndedPresentation);
        TEST_METHOD(VerticalTabHistoryUnfinishedFirst);
        TEST_METHOD(VerticalTabHistoryStatusStyles);
        TEST_METHOD(VerticalTabHistoryProtocolActivationPreservesView);
        TEST_METHOD(VerticalTabHistoryForegroundProtocolCreationExitsView);
        TEST_METHOD(VerticalTabHistoryActivationCompletionPreservesView);
        TEST_METHOD(VerticalTabHistoryActivationKeepsRows);
        TEST_METHOD(VerticalTabHistoryStartupLoading);
        TEST_METHOD(VerticalTabHistoryLoadingAndErrorsKeepRows);
        TEST_METHOD(VerticalTabHistoryTelemetryWaitsForReady);
        TEST_METHOD(VerticalTabHistorySnapshotRejectsMalformedResponse);
        TEST_METHOD(VerticalTabHistoryTitlesUseFirstLine);
        TEST_METHOD(VerticalTabHistoryIgnoresStaleLoadingResult);
        TEST_METHOD(VerticalTabHistoryRefreshPreservesCollection);
        TEST_METHOD(VerticalTabHistoryRefreshPreservesScroll);
        TEST_METHOD(VerticalTabHistorySearchProjection);
        TEST_METHOD(VerticalTabHistoryPreservesLiveSearch);
        TEST_METHOD(VerticalTabHistoryClosePreservesForegroundSelection);
        TEST_METHOD(VerticalTabHistoryCloseCancelsRefresh);
        TEST_METHOD(VerticalTabHistoryRefreshBackoff);
        TEST_METHOD(VerticalTabHistoryRefreshDuringActivation);
        TEST_METHOD(VerticalTabHistoryRefreshAfterReopen);
        TEST_METHOD(WindowActivationToleratesTabWithoutStatus);
        TEST_METHOD(AgentTabClassificationTracksSession);
        TEST_METHOD(CliAgentClassifiesTab);
        TEST_METHOD(VisibleFieldsControlRichTabComposition);
        TEST_METHOD(RichTabMetadataSelectionIsLimitedToTwo);
        TEST_METHOD(RichTabMetadataIsVisibleOnlyInVerticalLayout);
        TEST_METHOD(RichTabMetadataExpandsVerticalRow);
        TEST_METHOD(RichTabManifestAcceptsCamelCaseFieldIds);
        TEST_METHOD(RichTabRequestIncludesFirstPartyFields);
        TEST_METHOD(VisibleFieldsDoNotFilterTabs);

        TEST_METHOD(CreateSimpleTerminalXamlType);
        TEST_METHOD(CreateTerminalMuxXamlType);

        TEST_METHOD(CreateTerminalPage);
        TEST_METHOD(PaneContextPropagatesCaptureFailure);
        TEST_METHOD(AgentSessionRestoreRequiresPersistedBufferPath);
        TEST_METHOD(InteractiveResumeCommandPublishesSessionLifecycle);
        TEST_METHOD(AgentPaneRestoreRecordRoundTrips);
        TEST_METHOD(AgentPaneRestorePreservesSettingsBinding);
        TEST_METHOD(RestoredAgentSelectionBecomesExplicitOnlyAfterUserChoice);
        TEST_METHOD(PersistedLayoutAgentSessionsReceiveRestorePaths);
        TEST_METHOD(PaneAgentSessionBindingRequiresPaneIdentity);
        TEST_METHOD(AgentPaneRestoreDoesNotRequireAgentSession);
        TEST_METHOD(PaneAgentSessionEndClearsAgentBinding);
        TEST_METHOD(KeepRunningAcceptsPlainTerminalTabs);
        TEST_METHOD(KeepRunningMenuIsFirstAndVerticalOnly);
        TEST_METHOD(KeepRunningMenuTogglesOwningTab);
        TEST_METHOD(KeepRunningBadgeFitsLongTitle);
        TEST_METHOD(KeepRunningBadgeCentersAcrossRichTabRows);
        TEST_METHOD(KeepRunningMixedTabCloseRestoresSameContent);
        TEST_METHOD(KeepRunningDirectPaneCloseTerminates);
        TEST_METHOD(KeepRunningDetachedPaneCloseDiscardsGroup);
        TEST_METHOD(KeepRunningPageProjectionDoesNotRewriteManagerBinding);
        TEST_METHOD(KeepRunningCliExitRetainsDetachedShell);
        TEST_METHOD(KeepRunningReattachClaimAndRollback);
        TEST_METHOD(KeepRunningCloseAllPreservesAttachedAndRestoringTabs);
        TEST_METHOD(KeepRunningCloseAllRechecksGroupsAfterNotifications);
        TEST_METHOD(KeepRunningFailedRestorePreservesGroup);
        TEST_METHOD(KeepRunningConnectionExitPreservesTabLayout);
        TEST_METHOD(KeepRunningPreservesAssistantAndLayout);
        TEST_METHOD(KeepRunningHeadlessProtocolRetainsPaneRouting);
        TEST_METHOD(KeepRunningFocusReattachesOriginalTab);
        TEST_METHOD(KeepRunningFocusPreservesFailedRestore);
        TEST_METHOD(KeepRunningWindowCloseIsIdempotent);
        TEST_METHOD(KeepRunningStartupWaitsForHostRegistration);
        TEST_METHOD(KeepRunningStartupRestoresBatchAfterLayout);
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
        TEST_METHOD(ContentTransferRestoredAgentBindingKeepsOrigin);
        TEST_METHOD(ContentTransferReviewVisibleZoomedTabMoves);
        TEST_METHOD(ContentTransferReviewHiddenZoomedNestedTabKeepsFinalFocus);
        TEST_METHOD(ContentTransferReviewRollbackPreservesScrollOffset);
        TEST_METHOD(ContentTransferReviewScrollOffsetWithoutTransferIsStable);
        TEST_METHOD(ContentTransferReviewSuccessfulMovePreservesScrollOffset);
        TEST_METHOD(ContentTransferReviewClosedAgentSuppressionMovesWithTab);
        TEST_METHOD(ContentTransferReviewSinglePaneKeepsDestinationSuppression);
        TEST_METHOD(ContentTransferReviewSinglePaneKeepsSuppressedDestination);
        TEST_METHOD(DetachLastTerminalPaneClosesAgentOnlyTab);
        TEST_METHOD(DetachTerminalPanePreservesRemainingFocus);
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
        TEST_METHOD(SessionsDisabledHintFollowsViewAndSettings);
        TEST_METHOD(SessionsDisabledHintUpdatesWhileStashed);
        TEST_METHOD(SessionsDisabledHintWrapsAndPreservesTerminalGrid);
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
        void _verifyContentTransferReviewZoom(bool hidden, bool zoomed, bool freshReceiver, bool twoLeaves = true, bool restoredAgent = false);
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
            std::optional<int32_t> historySize = std::nullopt,
            bool verticalLayout = false);
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

    class HistoryTestView
    {
    public:
        HistoryTestView()
        {
            TestOnUIThread([&]() {
                strip = winrt::TerminalApp::TabStrip{};
                previousContent = Window::Current().Content();
                Window::Current().Content(strip);
                Window::Current().Activate();
                strip.HistoryActive(true);
                strip.UpdateLayout();
            });
        }

        ~HistoryTestView()
        {
            LOG_IF_FAILED(RunOnUIThread([&]() {
                Window::Current().Content(previousContent);
                strip = nullptr;
            }));
        }

        void Search(const winrt::hstring& query)
        {
            ::details::Event changed;
            TextBox box{ nullptr };
            winrt::event_token token{};
            const auto revoke = wil::scope_exit([&]() {
                LOG_IF_FAILED(RunOnUIThread([&]() {
                    if (box)
                    {
                        box.TextChanged(token);
                    }
                }));
            });
            TestOnUIThread([&]() {
                box = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip)->HistorySearchTextBox();
                token = box.TextChanged([&](auto&&, auto&&) {
                    if (box.Text() == query)
                    {
                        changed.Set();
                    }
                });
                if (box.Text() == query)
                {
                    changed.Set();
                }
                else
                {
                    box.Text(query);
                }
            });
            VERIFY_ARE_EQUAL(static_cast<DWORD>(WAIT_OBJECT_0), WaitForSingleObject(changed.m_handle, 10000));
        }

        winrt::TerminalApp::TabStrip strip{ nullptr };

    private:
        UIElement previousContent{ nullptr };
    };

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
        VERIFY_ARE_EQUAL(std::wstring{ L"copilot" }, Restore::ParseResumeCommandline(L"  copilot --resume agent-session-2  ").agent);
        VERIFY_ARE_EQUAL(std::wstring{ L"agent-session-2" }, Restore::ParseResumeCommandline(L"copilot --resume agent-session-2").sessionId);
        VERIFY_ARE_EQUAL(std::wstring{ L"copilot" }, Restore::ParseResumeCommandline(L"copilot --resume=agent-session-2").agent);
        VERIFY_ARE_EQUAL(std::wstring{ L"agent-session-2" }, Restore::ParseResumeCommandline(L"copilot --resume=agent-session-2").sessionId);
        VERIFY_IS_TRUE(Restore::ParseResumeCommandline(L"copilot --resume agent-session-2; calc.exe").agent.empty());
        VERIFY_IS_TRUE(Restore::ParseResumeCommandline(L"copilot --resume bad id").agent.empty());

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

    void TabTests::InteractiveResumeCommandPublishesSessionLifecycle()
    {
        const winrt::guid paneId{ L"{5d9cc4ac-1a11-4bb0-99ca-82a71e94fa77}" };
        const auto connection = winrt::make_self<TestConnection>(
            paneId,
            winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
        const auto page = _commonSetup(*connection);

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);
            const auto control = tab->GetActiveTerminalControl();
            const auto paneIdText = _formatPaneId(paneId);
            const auto tabId = winrt::to_string(tab->StableId());
            std::vector<Json::Value> events;
            const auto token = page->ProtocolVtSequenceReceived([&](auto&&, const winrt::hstring& payload) {
                Json::Value event;
                Json::CharReaderBuilder reader;
                std::istringstream stream{ winrt::to_string(payload) };
                std::string errors;
                if (Json::parseFromStream(reader, stream, &event, &errors))
                {
                    events.emplace_back(std::move(event));
                }
            });
            const auto revoke = wil::scope_exit([&]() {
                page->ProtocolVtSequenceReceived(token);
            });

            page->_TryPublishInteractiveResumeBinding(
                control,
                paneIdText,
                tabId,
                L"copilot",
                L"resumed-session");

            VERIFY_ARE_EQUAL(size_t{ 1 }, events.size());
            VERIFY_ARE_EQUAL(std::string{ "session_born_bound" }, events[0]["method"].asString());
            VERIFY_ARE_EQUAL(std::string{ "resumed-session" }, events[0]["params"]["agent_session_id"].asString());
            VERIFY_ARE_EQUAL(std::string{ "copilot" }, events[0]["params"]["agent"].asString());
            VERIFY_ARE_EQUAL(paneIdText, events[0]["params"]["pane_id"].asString());
            VERIFY_ARE_EQUAL(tabId, events[0]["params"]["tab_id"].asString());
            VERIFY_IS_TRUE(page->_paneAgentSessions.contains(paneId));
            VERIFY_IS_TRUE(page->_interactiveResumeSessions.contains(paneId));

            page->_CompleteInteractiveResumeBinding(paneIdText, tabId);

            VERIFY_ARE_EQUAL(size_t{ 2 }, events.size());
            VERIFY_ARE_EQUAL(std::string{ "agent_event" }, events[1]["method"].asString());
            VERIFY_ARE_EQUAL(std::string{ "agent.session.end" }, events[1]["params"]["event"].asString());
            VERIFY_ARE_EQUAL(std::string{ "resumed-session" }, events[1]["params"]["agent_session_id"].asString());
            VERIFY_IS_FALSE(page->_paneAgentSessions.contains(paneId));
            VERIFY_IS_FALSE(page->_interactiveResumeSessions.contains(paneId));
        });
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
        VERIFY_IS_TRUE(commandline.find(L"--agent-override") == std::wstring::npos);

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

    void TabTests::AgentPaneRestorePreservesSettingsBinding()
    {
        namespace Restore = ::Microsoft::Terminal::AgentPaneRestore;
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;

        auto page = _commonSetup();
        VERIFY_IS_NOT_NULL(page);
        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            const auto globals = page->_settings.GlobalSettings();
            const auto profile = page->_ResolveAgentSourceProfile(tab, page->_settings);
            VERIFY_IS_NOT_NULL(profile);
            tab->SuppressAgentPrewarm();

            struct Case
            {
                const wchar_t* name;
                const wchar_t* globalAgent;
                const wchar_t* profileBackend;
                const wchar_t* savedAgent;
                const wchar_t* savedCustomCommand;
                bool expectedOverride;
                bool expectedGlobalFollower;
            };
            const Case cases[]{
                { L"global follower", L"copilot", L"", L"copilot", L"", false, true },
                { L"changed global agent initially keeps session owner", L"claude", L"", L"copilot", L"", true, false },
                { L"profile backend remains profile-bound", L"copilot", L"host:copilot", L"copilot", L"", false, false },
                { L"WSL profile remains profile-bound", L"copilot", L"wsl:Ubuntu:copilot", L"wsl:Ubuntu:copilot", L"", false, false },
                { L"changed WSL distro initially keeps session owner", L"copilot", L"wsl:Debian:copilot", L"wsl:Ubuntu:copilot", L"", true, false },
                { L"restored WSL owner differs from global Host", L"copilot", L"", L"wsl:Ubuntu:copilot", L"", true, false },
                { L"custom global follower", L"custom:restore-test", L"", L"custom:restore-test", L"custom-agent --acp", false, true },
                { L"changed custom command initially keeps session owner", L"custom:restore-test", L"", L"custom:restore-test", L"old-agent --acp", true, false },
            };

            for (const auto& test : cases)
            {
                Log::Comment(test.name);
                tab->ClearAgentOverride();
                globals.AcpAgent(test.globalAgent);
                globals.AcpModel(L"new-settings-model");
                globals.AcpCustomCommand(test.globalAgent == std::wstring_view{ L"custom:restore-test" } ? L"custom-agent --acp" : L"");
                profile.AgentPaneBackend(test.profileBackend);

                Restore::Fields fields;
                fields.sessionId = L"restored-session";
                fields.view = Restore::ChatView;
                fields.agentIdentity = test.savedAgent;
                fields.customCommand = test.savedCustomCommand;
                NewTerminalArgs args;
                args.SetContentType(winrt::hstring{ Restore::StashedPaneType });
                args.Commandline(winrt::hstring{ Restore::BuildPaneCommandline(L"wta.exe", fields) });

                // Stop at the existing prewarm gate after applying the real
                // restore binding, without launching a helper or agent CLI.
                VERIFY_IS_FALSE(page->_RestoreAgentPaneFromLayout(tab, args, SplitDirection::Automatic, 0.5f));
                VERIFY_ARE_EQUAL(test.expectedOverride, tab->HasAgentOverride());
                VERIFY_ARE_EQUAL(test.expectedOverride, tab->HasRestoredAgentOverride());
                VERIFY_IS_FALSE(tab->HasExplicitAgentOverride());
                VERIFY_ARE_EQUAL(winrt::hstring{ test.savedAgent }, page->_GetAgentPaneIdentity(tab.get()));
                VERIFY_ARE_EQUAL(winrt::hstring{ test.savedCustomCommand }, page->_GetAgentPaneCustomCommand(tab.get()));

                const auto binding = page->_ResolveAgentPaneSettingsBindingForTab(tab);
                VERIFY_ARE_EQUAL(!test.expectedOverride && std::wstring_view{ test.profileBackend }.empty(), binding.followsGlobalAgent);
                VERIFY_ARE_EQUAL(test.expectedGlobalFollower, binding.followsGlobalAcpModel);
                VERIFY_ARE_EQUAL(
                    test.expectedGlobalFollower,
                    page->_IsAgentPaneModelHotUpdateTarget(binding, State::Connected, true, true));
                if (test.expectedGlobalFollower)
                {
                    VERIFY_ARE_EQUAL(std::wstring{ L"new-settings-model" }, binding.acpModel);
                }

                globals.AcpModel(L"later-settings-model");
                const auto settingsBinding = page->_ResolveAgentPaneSettingsBindingForTab(tab, true);
                const auto profileBackend = ::Microsoft::Terminal::Settings::Model::AgentPaneBackend::Parse(test.profileBackend);
                VERIFY_ARE_EQUAL(!profileBackend.has_value(), settingsBinding.followsGlobalAgent);
                VERIFY_ARE_EQUAL(!profileBackend.has_value(), settingsBinding.followsGlobalAcpModel);
                VERIFY_ARE_EQUAL(
                    profileBackend ? profileBackend->agentId : std::wstring{ test.globalAgent },
                    settingsBinding.agentId);
                VERIFY_ARE_EQUAL(
                    profileBackend && profileBackend->source == ::Microsoft::Terminal::Settings::Model::AgentPaneBackendSource::Wsl ?
                        std::wstring{ L"wsl" } :
                        std::wstring{ L"host" },
                    settingsBinding.agentSource);
                if (!profileBackend)
                {
                    VERIFY_ARE_EQUAL(std::wstring{ L"later-settings-model" }, settingsBinding.acpModel);
                }
                VERIFY_ARE_EQUAL(winrt::hstring{ test.savedAgent }, page->_GetAgentPaneIdentity(tab.get()));
                VERIFY_ARE_EQUAL(test.expectedOverride, tab->HasRestoredAgentOverride());
            }
        });
    }

    void TabTests::RestoredAgentSelectionBecomesExplicitOnlyAfterUserChoice()
    {
        using Tab = winrt::TerminalApp::implementation::Tab;
        auto page = _commonSetup();
        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            page->_settings.GlobalSettings().AcpAgent(L"claude");
            tab->SetAgentOverride(L"copilot", {}, {}, L"host", {}, Tab::AgentOverrideOrigin::Restore);
            VERIFY_IS_TRUE(tab->HasRestoredAgentOverride());
            VERIFY_IS_FALSE(tab->HasExplicitAgentOverride());
            VERIFY_ARE_EQUAL(std::wstring{ L"claude" }, page->_ResolveAgentPaneSettingsBindingForTab(tab, true).agentId);

            Json::Value evt;
            evt["params"]["window_id"] = std::to_string(page->_WindowProperties.WindowId());
            evt["params"]["tab_id"] = winrt::to_string(tab->StableId());
            evt["params"]["agent_id"] = "copilot";
            evt["params"]["agent_source"] = "host";
            Json::StreamWriterBuilder writer;
            writer["indentation"] = "";
            page->OnAgentSwitchRequested(winrt::to_hstring(Json::writeString(writer, evt)));

            VERIFY_IS_FALSE(tab->HasRestoredAgentOverride());
            VERIFY_IS_TRUE(tab->HasExplicitAgentOverride());
            VERIFY_ARE_EQUAL(std::wstring{ L"copilot" }, page->_ResolveAgentPaneSettingsBindingForTab(tab, true).agentId);
            tab->ClearAgentOverride();
            VERIFY_IS_FALSE(tab->HasExplicitAgentOverride());
            VERIFY_IS_FALSE(tab->HasRestoredAgentOverride());
        });
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

    static winrt::hstring _keepRunningHook(const winrt::guid& paneId, std::string_view name, std::string_view session = "keep-running-session", std::string_view agent = "copilot")
    {
        Json::Value event;
        event["params"]["pane_id"] = _formatPaneId(paneId);
        event["params"]["event"] = std::string{ name };
        event["params"]["agent_session_id"] = std::string{ session };
        event["params"]["agent"] = std::string{ agent };
        return winrt::to_hstring(Json::writeString(Json::StreamWriterBuilder{}, event));
    }

    void TabTests::KeepRunningAcceptsPlainTerminalTabs()
    {
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        const winrt::guid id{ L"{13f7aa41-8837-473e-92a3-f1e682ab1001}" };
        const auto connection = winrt::make_self<TestConnection>(id, State::Connected);
        const auto page = _commonSetup(*connection);
        TestOnUIThread([&]() {
            const winrt::guid tabId{ page->_GetFocusedTabImpl()->StableId() };
            VERIFY_IS_TRUE(page->CanKeepTabRunning(tabId));
            VERIFY_IS_FALSE(page->IsTabKeepRunning(tabId));
            page->SetTabKeepRunning(tabId, true);
            VERIFY_IS_TRUE(page->IsTabKeepRunning(tabId));
            page->OnPaneAgentSessionChanged(_keepRunningHook(id, "agent.session.start"));
            page->OnPaneAgentSessionChanged(_keepRunningHook(id, "agent.prompt.submit", "nested-session"));
            page->OnPaneAgentSessionChanged(_keepRunningHook(id, "agent.session.end", "stale-session"));
            page->OnPaneAgentSessionChanged(_keepRunningHook(id, "agent.session.end"));
            VERIFY_IS_TRUE(page->IsTabKeepRunning(tabId));
            page->OnPaneAgentSessionChanged(_keepRunningHook(id, "agent.session.start", "new-session"));
            VERIFY_IS_TRUE(page->IsTabKeepRunning(tabId));
            page->SetTabKeepRunning(tabId, false);
            VERIFY_IS_FALSE(page->IsTabKeepRunning(tabId));
            VERIFY_IS_FALSE(page->CanKeepTabRunning(id));
            VERIFY_THROWS(page->SetTabKeepRunning(id, true), winrt::hresult_error);
            VERIFY_IS_FALSE(page->CanKeepTabRunning(winrt::guid{}));
        });
    }

    void TabTests::KeepRunningMixedTabCloseRestoresSameContent()
    {
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        const winrt::guid keptId{ L"{13f7aa41-8837-473e-92a3-f1e682ab1002}" };
        const winrt::guid closedId{ L"{13f7aa41-8837-473e-92a3-f1e682ab1003}" };
        const auto kept = winrt::make_self<TestConnection>(keptId, State::Connected);
        const auto closed = winrt::make_self<TestConnection>(closedId, State::Connected);
        const auto page = _commonSetup(*kept);
        std::vector<ConnectionStateEventRecord> events;
        TestOnUIThread([&]() {
            page->_settings.GlobalSettings().ConfirmOnClose(ConfirmOnClose::Never);
            const auto eventToken = page->ProtocolVtSequenceReceived([&](auto&&, const auto& json) { _recordConnectionStateEvent(json, events); });
            const auto revoke = wil::scope_exit([&]() noexcept { page->ProtocolVtSequenceReceived(eventToken); });
            const auto tab = page->_GetFocusedTabImpl();
            const auto control = tab->GetRootPane()->GetTerminalControl();
            const auto contentId = control.ContentId();
            const auto content = page->_manager.TryLookupCore(contentId);
            const auto stableId = tab->StableId();
            const winrt::guid groupId{ stableId };
            const auto otherPane = page->_MakeTerminalPane(NewTerminalArgs{}, nullptr, *closed);
            VERIFY_IS_TRUE(page->_SplitPane(tab, SplitDirection::Right, 0.5f, otherPane));
            page->OnPaneAgentSessionChanged(_keepRunningHook(keptId, "agent.session.start"));
            page->OnPaneAgentSessionChanged(_keepRunningHook(closedId, "agent.session.start"));
            page->_manager.OnPaneAgentSessionChanged(_keepRunningHook(keptId, "agent.session.start"));
            page->_manager.OnPaneAgentSessionChanged(_keepRunningHook(closedId, "agent.session.start"));
            page->SetTabKeepRunning(groupId, true);
            page->_HandleCloseTabRequested(*tab, true);

            VERIFY_ARE_EQUAL(0u, page->_tabs.Size());
            VERIFY_ARE_EQUAL(0u, kept->CloseCount());
            VERIFY_ARE_EQUAL(0u, closed->CloseCount());
            VERIFY_IS_TRUE(page->_manager.HasKeptSessions());
            VERIFY_ARE_EQUAL(1u, page->_manager.KeptGroups().Size());
            VERIFY_IS_TRUE(page->_manager.KeptGroups().HasKey(groupId));
            VERIFY_IS_TRUE(_statesForPane(events, _formatPaneId(keptId)).empty());
            VERIFY_IS_TRUE(_statesForPane(events, _formatPaneId(closedId)).empty());
            VERIFY_IS_TRUE(page->_previouslyClosedPanesAndTabs.empty());
            const std::u16string output{ u"still running while detached\r\n" };
            kept->TerminalOutput.raise(winrt::array_view<const char16_t>{ output.data(), output.data() + output.size() });
            VERIFY_IS_TRUE(std::wstring_view{ content.Core().ReadEntireBuffer() }.find(L"still running while detached") != std::wstring_view::npos);

            VERIFY_IS_TRUE(page->RestoreKeptGroup(groupId));
            const auto restored = page->_GetFocusedTabImpl();
            VERIFY_ARE_EQUAL(stableId, restored->StableId());
            VERIFY_ARE_EQUAL(2, restored->GetLeafPaneCount());
            const auto restoredControl = restored->GetRootPane()->FindPaneBySessionId(keptId)->GetTerminalControl();
            VERIFY_ARE_EQUAL(contentId, restoredControl.ContentId());
            VERIFY_IS_TRUE(content == page->_manager.TryLookupCore(restoredControl.ContentId()));
            VERIFY_IS_TRUE(restoredControl.Connection() == *kept);
            VERIFY_ARE_EQUAL(keptId, restoredControl.Connection().SessionId());
            VERIFY_IS_TRUE(std::wstring_view{ restoredControl.ReadEntireBuffer() }.find(L"still running while detached") != std::wstring_view::npos);
            VERIFY_IS_TRUE(page->IsTabKeepRunning(groupId));
            VERIFY_IS_TRUE(restored->TabStatus().IsKeepRunning());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Turn off keep running" }, restored->_keepRunningMenuItem.Text());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"\xE711" }, restored->_keepRunningMenuItem.Icon().as<FontIcon>().Glyph());
            VERIFY_IS_FALSE(page->_manager.HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, kept->CloseCount());
            VERIFY_THROWS(page->RestoreKeptGroup(groupId), winrt::hresult_error);
            restored->Close();
        });
    }

    void TabTests::KeepRunningMenuIsFirstAndVerticalOnly()
    {
        const auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);
        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            const auto item = tab->_keepRunningMenuItem;
            const auto menu = tab->TabViewItem().ContextFlyout().as<MenuFlyout>();
            VERIFY_IS_TRUE(menu.Items().GetAt(0) == item);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Keep tab running" }, item.Text());
            const auto icon = item.Icon().as<FontIcon>();
            VERIFY_ARE_EQUAL(winrt::hstring{ L"\xE8EE" }, icon.Glyph());
            const auto badge = tab->_headerControl.FindName(L"HeaderKeepRunningIcon").as<FontIcon>();
            VERIFY_ARE_EQUAL(icon.Glyph(), badge.Glyph());
            VERIFY_ARE_EQUAL(icon.FontFamily().Source(), badge.FontFamily().Source());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Keep tab running" },
                             winrt::Windows::UI::Xaml::Automation::AutomationProperties::GetName(badge));
            const winrt::hstring tooltip{ L"Keep this tab running in the background after closing the tab or window." };
            VERIFY_ARE_EQUAL(tooltip, winrt::unbox_value<winrt::hstring>(ToolTipService::GetToolTip(item)));
            VERIFY_ARE_EQUAL(tooltip, winrt::Windows::UI::Xaml::Automation::AutomationProperties::GetHelpText(item));
            VERIFY_ARE_EQUAL(Visibility::Visible, item.Visibility());
            VERIFY_IS_TRUE(item.IsEnabled());
            VERIFY_IS_FALSE(tab->TabStatus().IsKeepRunning());

            const winrt::guid id{ tab->StableId() };
            page->SetTabKeepRunning(id, true);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Turn off keep running" }, item.Text());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"\xE711" }, icon.Glyph());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"\xE8EE" }, badge.Glyph());
            const winrt::hstring turnOffTooltip{ L"This tab will no longer stay running after you close the tab or window." };
            VERIFY_ARE_EQUAL(turnOffTooltip, winrt::unbox_value<winrt::hstring>(ToolTipService::GetToolTip(item)));
            VERIFY_ARE_EQUAL(turnOffTooltip, winrt::Windows::UI::Xaml::Automation::AutomationProperties::GetHelpText(item));
            VERIFY_IS_TRUE(tab->TabStatus().IsKeepRunning());
            tab->SetVerticalTabLayout(false);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, item.Visibility());
            VERIFY_IS_FALSE(item.IsEnabled());
            VERIFY_IS_TRUE(page->IsTabKeepRunning(id));
            VERIFY_IS_TRUE(tab->TabStatus().IsKeepRunning());
            tab->SetVerticalTabLayout(true);
            VERIFY_ARE_EQUAL(Visibility::Visible, item.Visibility());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Turn off keep running" }, item.Text());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"\xE711" }, icon.Glyph());
            page->SetTabKeepRunning(id, false);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Keep tab running" }, item.Text());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"\xE8EE" }, icon.Glyph());
            VERIFY_ARE_EQUAL(tooltip, winrt::unbox_value<winrt::hstring>(ToolTipService::GetToolTip(item)));
            VERIFY_ARE_EQUAL(tooltip, winrt::Windows::UI::Xaml::Automation::AutomationProperties::GetHelpText(item));
            VERIFY_IS_FALSE(tab->TabStatus().IsKeepRunning());

            const auto root = tab->_rootPane;
            tab->_rootPane = nullptr;
            tab->_UpdateKeepRunningMenuItem();
            VERIFY_ARE_EQUAL(Visibility::Collapsed, item.Visibility());
            VERIFY_IS_FALSE(item.IsEnabled());
            tab->_rootPane = root;
            tab->_UpdateKeepRunningMenuItem();
        });
    }

    void TabTests::KeepRunningMenuTogglesOwningTab()
    {
        using namespace winrt::Windows::UI::Xaml::Automation;
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        const auto connection = winrt::make_self<TestConnection>(winrt::guid{ L"{13f7aa41-8837-473e-92a3-f1e682ab1030}" }, State::Connected);
        const auto page = _commonSetup(*connection, nullptr, std::nullopt, true);
        TestOnUIThread([&]() {
            const auto owner = page->_GetFocusedTabImpl();
            const auto control = owner->GetRootPane()->GetTerminalControl();
            const auto contentId = control.ContentId();
            const auto siblingPane = page->_MakePane(nullptr, nullptr, nullptr);
            page->_CreateNewTabFromPane(siblingPane);
            const auto focused = page->_GetFocusedTabImpl();
            VERIFY_IS_TRUE(owner != focused);
            const Peers::MenuFlyoutItemAutomationPeer peer{ owner->_keepRunningMenuItem };
            const auto invoke = peer.GetPattern(Peers::PatternInterface::Invoke).as<Provider::IInvokeProvider>();
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Keep tab running" }, peer.GetName());
            invoke.Invoke();
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Turn off keep running" }, peer.GetName());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"\xE711" }, owner->_keepRunningMenuItem.Icon().as<FontIcon>().Glyph());
            VERIFY_IS_TRUE(page->IsTabKeepRunning(winrt::guid{ owner->StableId() }));
            VERIFY_IS_TRUE(owner->TabStatus().IsKeepRunning());
            VERIFY_IS_FALSE(focused->KeepRunning());
            VERIFY_IS_FALSE(focused->TabStatus().IsKeepRunning());
            invoke.Invoke();
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Keep tab running" }, peer.GetName());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"\xE8EE" }, owner->_keepRunningMenuItem.Icon().as<FontIcon>().Glyph());
            VERIFY_IS_FALSE(owner->KeepRunning());
            VERIFY_IS_FALSE(owner->TabStatus().IsKeepRunning());
            VERIFY_IS_FALSE(page->IsTabKeepRunning(winrt::guid{ owner->StableId() }));
            VERIFY_ARE_EQUAL(2u, page->_tabs.Size());
            VERIFY_IS_TRUE(page->_GetFocusedTabImpl() == focused);
            VERIFY_IS_TRUE(owner->GetRootPane()->GetTerminalControl() == control);
            VERIFY_ARE_EQUAL(contentId, control.ContentId());
            VERIFY_ARE_EQUAL(State::Connected, connection->State());
            VERIFY_ARE_EQUAL(0u, connection->CloseCount());
            VERIFY_IS_FALSE(page->_manager.HasKeptSessions());
        });
    }

    void TabTests::KeepRunningBadgeFitsLongTitle()
    {
        const auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);
        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            const auto header = tab->_headerControl;
            const auto badge = header.FindName(L"HeaderKeepRunningIcon").as<FontIcon>();
            tab->SetTabText(winrt::hstring{ std::wstring(240, L'W') });
            tab->KeepRunning(true);
            header.Width(120);
            header.Measure({ 120, 32 });
            header.Arrange({ 0, 0, 120, 32 });
            header.UpdateLayout();
            VERIFY_ARE_EQUAL(Visibility::Visible, badge.Visibility());
            VERIFY_IS_TRUE(badge.ActualWidth() > 0);
            const auto position = badge.TransformToVisual(header).TransformPoint({ 0, 0 });
            VERIFY_IS_TRUE(position.X >= 0);
            VERIFY_IS_TRUE(position.X + badge.ActualWidth() <= header.ActualWidth());

            tab->KeepRunning(false);
            header.UpdateLayout();
            VERIFY_ARE_EQUAL(Visibility::Collapsed, badge.Visibility());
        });
    }

    void TabTests::KeepRunningBadgeCentersAcrossRichTabRows()
    {
        const auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);
        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            const auto header = tab->_headerControl;
            const auto layout = header.FindName(L"HeaderLayout").as<Grid>();
            const auto badge = header.FindName(L"HeaderKeepRunningIcon").as<FontIcon>();
            const auto metadata = header.FindName(L"HeaderMetadataTextBlock").as<winrt::TerminalApp::HighlightedTextControl>();
            tab->SetTabText(winrt::hstring{ std::wstring(240, L'W') });
            tab->KeepRunning(true);
            VERIFY_ARE_EQUAL(2, Grid::GetRowSpan(badge));
            VERIFY_ARE_EQUAL(2, Grid::GetColumn(badge));
            VERIFY_ARE_EQUAL(8.0, badge.Margin().Left);
            VERIFY_ARE_EQUAL(VerticalAlignment::Center, badge.VerticalAlignment());

            constexpr double tolerance = 1.0;
            for (const auto width : { 120.0f, 240.0f })
            {
                double singleLineX = 0;
                double singleLineHeight = 0;
                for (const auto text : { L"", L"main - a long metadata line that must truncate", L"main\n2 changes" })
                {
                    ::Microsoft::Terminal::RichTab::Provider::Presentation presentation;
                    presentation.text = text;
                    tab->SetRichTabPresentation(presentation);
                    header.Width(width);
                    header.Measure({ width, 100 });
                    header.Arrange({ 0, 0, width, header.DesiredSize().Height });
                    header.UpdateLayout();

                    const auto position = badge.TransformToVisual(layout).TransformPoint({ 0, 0 });
                    VERIFY_IS_TRUE(badge.ActualHeight() > 0);
                    VERIFY_IS_TRUE(std::abs(position.Y + badge.ActualHeight() / 2 - layout.ActualHeight() / 2) <= tolerance);
                    VERIFY_IS_TRUE(position.X + badge.ActualWidth() <= layout.ActualWidth() + tolerance);
                    if (presentation.text.empty())
                    {
                        singleLineX = position.X;
                        singleLineHeight = layout.ActualHeight();
                        VERIFY_ARE_EQUAL(Visibility::Collapsed, metadata.Visibility());
                    }
                    else
                    {
                        VERIFY_ARE_EQUAL(Visibility::Visible, metadata.Visibility());
                        VERIFY_IS_TRUE(layout.ActualHeight() > singleLineHeight);
                        VERIFY_IS_TRUE(std::abs(position.X - singleLineX) <= tolerance);
                        const auto metadataPosition = metadata.TransformToVisual(layout).TransformPoint({ 0, 0 });
                        VERIFY_IS_TRUE(metadataPosition.X + metadata.ActualWidth() <= position.X - badge.Margin().Left + tolerance);
                    }
                }
            }
        });
    }

    void TabTests::KeepRunningDirectPaneCloseTerminates()
    {
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        const winrt::guid id{ L"{13f7aa41-8837-473e-92a3-f1e682ab1004}" };
        const auto connection = winrt::make_self<TestConnection>(id, State::Connected);
        const auto page = _commonSetup(*connection);
        TestOnUIThread([&]() {
            page->OnPaneAgentSessionChanged(_keepRunningHook(id, "agent.session.start"));
            page->SetTabKeepRunning(winrt::guid{ page->_GetFocusedTabImpl()->StableId() }, true);
            page->_HandleClosePaneRequested(page->_GetFocusedTabImpl()->GetRootPane());
        });
        VERIFY_IS_TRUE(connection->WaitForClose());
        TestOnUIThread([&]() { VERIFY_IS_FALSE(page->_manager.HasKeptSessions()); });
    }

    void TabTests::KeepRunningCliExitRetainsDetachedShell()
    {
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        const winrt::guid id{ L"{13f7aa41-8837-473e-92a3-f1e682ab1005}" };
        const auto connection = winrt::make_self<TestConnection>(id, State::Connected);
        const auto page = _commonSetup(*connection);
        TestOnUIThread([&]() {
            page->_settings.GlobalSettings().ConfirmOnClose(ConfirmOnClose::Never);
            const auto tab = page->_GetFocusedTabImpl();
            const winrt::guid groupId{ tab->StableId() };
            page->_manager.OnPaneAgentSessionChanged(_keepRunningHook(id, "agent.session.start"));
            page->OnPaneAgentSessionChanged(_keepRunningHook(id, "agent.session.start"));
            page->SetTabKeepRunning(groupId, true);
            page->_HandleCloseTabRequested(*tab, true);
            page->_manager.OnPaneAgentSessionChanged(_keepRunningHook(id, "agent.session.end"));
            VERIFY_IS_TRUE(page->_manager.HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, connection->CloseCount());
            VERIFY_IS_TRUE(page->RestoreKeptGroup(groupId));
            VERIFY_IS_TRUE(page->CanKeepTabRunning(groupId));
            VERIFY_IS_TRUE(page->IsTabKeepRunning(groupId));
            page->_GetFocusedTabImpl()->Close();
        });
        VERIFY_IS_TRUE(connection->WaitForClose());
    }

    void TabTests::KeepRunningDetachedPaneCloseDiscardsGroup()
    {
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        const winrt::guid id{ L"{13f7aa41-8837-473e-92a3-f1e682ab1040}" };
        const auto connection = winrt::make_self<TestConnection>(id, State::Connected);
        const auto page = _commonSetup(*connection);
        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            const winrt::guid groupId{ tab->StableId() };
            tab->KeepRunning(true);
            page->_KeepTabRunning(tab);
            VERIFY_ARE_EQUAL(0u, page->_tabs.Size());
            VERIFY_IS_TRUE(page->_manager.KeptGroups().HasKey(groupId));
            std::vector<ConnectionStateEventRecord> events;
            const auto token = page->_manager.DetachedSessionEvent([&](auto&&, const auto& json) {
                _recordConnectionStateEvent(json, events);
            });
            const auto revoke = wil::scope_exit([&]() noexcept { page->_manager.DetachedSessionEvent(token); });
            page->_HandleClosePaneRequested(tab->GetRootPane());
            VERIFY_IS_FALSE(page->_manager.HasKeptSessions());
            VERIFY_IS_FALSE(page->_manager.KeptGroups().HasKey(groupId));
            VERIFY_ARE_EQUAL(size_t{ 1 }, _statesForPane(events, _formatPaneId(id)).size());
            VERIFY_THROWS(page->RestoreKeptGroup(groupId), winrt::hresult_error);
        });
        VERIFY_IS_TRUE(connection->WaitForClose());
    }

    void TabTests::KeepRunningPageProjectionDoesNotRewriteManagerBinding()
    {
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        const winrt::guid id{ L"{13f7aa41-8837-473e-92a3-f1e682ab1041}" };
        const auto connection = winrt::make_self<TestConnection>(id, State::Connected);
        const auto page = _commonSetup(*connection);
        TestOnUIThread([&]() {
            const auto contentId = page->_GetFocusedTabImpl()->GetActiveTerminalControl().ContentId();
            const auto started = _keepRunningHook(id, "agent.session.start");
            page->_manager.OnPaneAgentSessionChanged(started);
            page->_manager.OnPaneAgentSessionChanged(_keepRunningHook(id, "agent.session.end"));
            VERIFY_IS_TRUE(page->_manager.AgentSessionEvent(contentId).empty());

            page->OnPaneAgentSessionChanged(started);
            VERIFY_IS_TRUE(page->_paneAgentSessions.contains(id));
            VERIFY_IS_TRUE(page->_manager.AgentSessionEvent(contentId).empty());

            const auto replacement = _keepRunningHook(id, "agent.session.start", "replacement-session");
            page->_manager.OnPaneAgentSessionChanged(replacement);
            page->OnPaneAgentSessionChanged(started);
            page->OnPaneAgentSessionChanged(_keepRunningHook(id, "agent.session.end"));
            VERIFY_ARE_EQUAL(replacement, page->_manager.AgentSessionEvent(contentId));
        });
    }

    void TabTests::KeepRunningReattachClaimAndRollback()
    {
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        const winrt::guid id{ L"{13f7aa41-8837-473e-92a3-f1e682ab1006}" };
        const auto connection = winrt::make_self<TestConnection>(id, State::Connected);
        const auto page = _commonSetup(*connection);
        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            const winrt::guid groupId{ tab->StableId() };
            page->OnPaneAgentSessionChanged(_keepRunningHook(id, "agent.session.start"));
            page->SetTabKeepRunning(groupId, true);
            page->_KeepTabRunning(tab);
            const auto firstTab = page->_manager.BeginReattachKeptGroup(groupId);
            VERIFY_IS_TRUE(firstTab == *tab);
            const auto contentId = tab->GetRootPane()->GetTerminalControl().ContentId();
            VERIFY_THROWS(page->_AttachControlToContent(contentId, NewTerminalArgs{}), winrt::hresult_error);
            VERIFY_IS_TRUE(page->_manager.HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, page->_manager.KeptGroups().Size());
            VERIFY_THROWS(page->_manager.BeginReattachKeptGroup(groupId), winrt::hresult_error);
            VERIFY_THROWS(page->_manager.DiscardKeptGroup(groupId), winrt::hresult_error);
            page->_manager.CompleteKeptGroupReattach(groupId, false);
            const auto retry = page->_manager.BeginReattachKeptGroup(groupId);
            VERIFY_IS_TRUE(retry == *tab);
            page->_manager.CompleteKeptGroupReattach(groupId, false);
            VERIFY_IS_TRUE(page->RestoreKeptGroup(groupId));
            VERIFY_ARE_EQUAL(0u, connection->CloseCount());
            page->_GetFocusedTabImpl()->Close();
        });
    }

    void TabTests::KeepRunningCloseAllPreservesAttachedAndRestoringTabs()
    {
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        const auto first = winrt::make_self<TestConnection>(winrt::guid{ L"{13f7aa41-8837-473e-92a3-f1e682ab1050}" }, State::Connected);
        const auto second = winrt::make_self<TestConnection>(winrt::guid{ L"{13f7aa41-8837-473e-92a3-f1e682ab1051}" }, State::Connected);
        const auto restoring = winrt::make_self<TestConnection>(winrt::guid{ L"{13f7aa41-8837-473e-92a3-f1e682ab1052}" }, State::Connected);
        const auto attached = winrt::make_self<TestConnection>(winrt::guid{ L"{13f7aa41-8837-473e-92a3-f1e682ab1053}" }, State::Connected);
        const auto page = _commonSetup(*first);
        TestOnUIThread([&]() {
            const auto firstTab = page->_GetFocusedTabImpl();
            firstTab->KeepRunning(true);
            page->_KeepTabRunning(firstTab);
            for (const auto& connection : { second, restoring, attached })
            {
                page->_CreateNewTabFromPane(page->_MakePane(nullptr, nullptr, *connection));
                const auto tab = page->_GetFocusedTabImpl();
                tab->SuppressAgentPrewarm();
                tab->KeepRunning(true);
                if (connection != attached)
                {
                    page->_KeepTabRunning(tab);
                }
            }
            const auto manager = page->_manager;
            const auto restoringGroup = manager.KeptGroupForPane(restoring->SessionId());
            manager.BeginReattachKeptGroup(restoringGroup);
            VERIFY_ARE_EQUAL(2u, manager.KeptGroups().Size());
            manager.DiscardAllKeptGroups();
            VERIFY_IS_TRUE(manager.HasKeptSessions());
            VERIFY_ARE_EQUAL(0u, manager.KeptGroups().Size());
            VERIFY_ARE_EQUAL(1u, page->_tabs.Size());
            VERIFY_ARE_EQUAL(0u, restoring->CloseCount());
            VERIFY_ARE_EQUAL(0u, attached->CloseCount());

            manager.CompleteKeptGroupReattach(restoringGroup, false);
            VERIFY_IS_TRUE(page->RestoreKeptGroup(restoringGroup));
            manager.DiscardAllKeptGroups();
            VERIFY_IS_FALSE(manager.HasKeptSessions());
            VERIFY_ARE_EQUAL(2u, page->_tabs.Size());
            VERIFY_ARE_EQUAL(0u, restoring->CloseCount());
            VERIFY_ARE_EQUAL(0u, attached->CloseCount());
            for (const auto& tab : page->_tabs)
            {
                tab.Shutdown();
            }
        });
        VERIFY_IS_TRUE(first->WaitForClose());
        VERIFY_IS_TRUE(second->WaitForClose());
        VERIFY_ARE_EQUAL(1u, first->CloseCount());
        VERIFY_ARE_EQUAL(1u, second->CloseCount());
    }

    void TabTests::KeepRunningCloseAllRechecksGroupsAfterNotifications()
    {
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        const auto first = winrt::make_self<TestConnection>(winrt::guid{ L"{13f7aa41-8837-473e-92a3-f1e682ab1054}" }, State::Connected);
        const auto second = winrt::make_self<TestConnection>(winrt::guid{ L"{13f7aa41-8837-473e-92a3-f1e682ab1055}" }, State::Connected);
        const auto page = _commonSetup(*first);
        winrt::guid restoredSession{};
        TestOnUIThread([&]() {
            const auto firstTab = page->_GetFocusedTabImpl();
            firstTab->KeepRunning(true);
            page->_KeepTabRunning(firstTab);
            page->_CreateNewTabFromPane(page->_MakePane(nullptr, nullptr, *second));
            const auto secondTab = page->_GetFocusedTabImpl();
            secondTab->SuppressAgentPrewarm();
            secondTab->KeepRunning(true);
            page->_KeepTabRunning(secondTab);
            const auto manager = page->_manager;
            bool restored = false;
            const auto token = manager.KeptSessionsChanged([&](auto&&, auto&&) {
                if (!restored && manager.KeptGroups().Size() == 1)
                {
                    restored = true;
                    const auto groupId = manager.KeptGroups().First().Current().Key();
                    VERIFY_IS_TRUE(page->RestoreKeptGroup(groupId));
                    restoredSession = page->_GetFocusedTabImpl()->GetActiveTerminalControl().Connection().SessionId();
                }
            });
            const auto revoke = wil::scope_exit([&]() noexcept { manager.KeptSessionsChanged(token); });
            manager.DiscardAllKeptGroups();
            VERIFY_IS_TRUE(restored);
            VERIFY_IS_FALSE(manager.HasKeptSessions());
            VERIFY_ARE_EQUAL(1u, page->_tabs.Size());
            const auto restoredConnection = restoredSession == first->SessionId() ? first : second;
            VERIFY_ARE_EQUAL(0u, restoredConnection->CloseCount());
            manager.DiscardAllKeptGroups();
            VERIFY_ARE_EQUAL(0u, restoredConnection->CloseCount());
            page->_GetFocusedTabImpl()->Close();
        });
        VERIFY_IS_TRUE(first->WaitForClose());
        VERIFY_IS_TRUE(second->WaitForClose());
        VERIFY_ARE_EQUAL(1u, first->CloseCount());
        VERIFY_ARE_EQUAL(1u, second->CloseCount());
    }

    void TabTests::KeepRunningFailedRestorePreservesGroup()
    {
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        const winrt::guid firstId{ L"{13f7aa41-8837-473e-92a3-f1e682ab1010}" };
        const winrt::guid secondId{ L"{13f7aa41-8837-473e-92a3-f1e682ab1011}" };
        const auto first = winrt::make_self<TestConnection>(firstId, State::Connected);
        const auto second = winrt::make_self<TestConnection>(secondId, State::Connected);
        const auto page = _commonSetup(*first);
        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            const winrt::guid groupId{ tab->StableId() };
            const auto firstPane = tab->GetRootPane();
            const auto firstControl = firstPane->GetTerminalControl();
            const auto other = page->_MakeTerminalPane(NewTerminalArgs{}, nullptr, *second);
            const auto otherControl = other->GetTerminalControl();
            VERIFY_IS_TRUE(page->_SplitPane(tab, SplitDirection::Right, 0.5f, other));
            page->SetTabKeepRunning(groupId, true);
            page->_KeepTabRunning(tab);
            using Stage = winrt::TerminalApp::implementation::TerminalPage::ContentTransferStage;
            page->_contentTransferTestHook = [](Stage stage, uint64_t, uint32_t) {
                THROW_HR_IF(E_ABORT, stage == Stage::BeforeSplitInsertion);
            };
            VERIFY_THROWS(page->RestoreKeptGroup(groupId), winrt::hresult_error);
            page->_contentTransferTestHook = {};
            VERIFY_ARE_EQUAL(0u, page->_tabs.Size());
            VERIFY_ARE_EQUAL(0u, first->CloseCount());
            VERIFY_ARE_EQUAL(0u, second->CloseCount());
            VERIFY_IS_TRUE(page->_manager.KeptGroups().HasKey(groupId));
            VERIFY_IS_TRUE(page->_manager.BeginReattachKeptGroup(groupId) == *tab);
            VERIFY_ARE_EQUAL(2, tab->GetLeafPaneCount());
            VERIFY_IS_TRUE(firstControl.TransferState() == winrt::Microsoft::Terminal::Control::ContentTransferState::Owned);
            VERIFY_IS_TRUE(otherControl.TransferState() == winrt::Microsoft::Terminal::Control::ContentTransferState::Owned);
            page->_manager.CompleteKeptGroupReattach(groupId, false);
            VERIFY_IS_TRUE(page->RestoreKeptGroup(groupId));
            VERIFY_IS_FALSE(page->_manager.HasKeptSessions());
            page->_GetFocusedTabImpl()->Close();
        });
        VERIFY_IS_TRUE(first->WaitForClose());
        VERIFY_IS_TRUE(second->WaitForClose());
    }

    void TabTests::KeepRunningConnectionExitPreservesTabLayout()
    {
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        const winrt::guid firstId{ L"{13f7aa41-8837-473e-92a3-f1e682ab1007}" };
        const winrt::guid secondId{ L"{13f7aa41-8837-473e-92a3-f1e682ab1008}" };
        const auto first = winrt::make_self<TestConnection>(firstId, State::Connected);
        const auto second = winrt::make_self<TestConnection>(secondId, State::Connected);
        const auto page = _commonSetup(*first);
        std::vector<ConnectionStateEventRecord> events;
        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            const winrt::guid groupId{ tab->StableId() };
            const auto other = page->_MakeTerminalPane(NewTerminalArgs{}, nullptr, *second);
            VERIFY_IS_TRUE(page->_SplitPane(tab, SplitDirection::Right, 0.5f, other));
            page->SetTabKeepRunning(groupId, true);
            const auto eventToken = page->_manager.DetachedSessionEvent([&](auto&&, const auto& json) { _recordConnectionStateEvent(json, events); });
            const auto revoke = wil::scope_exit([&]() noexcept { page->_manager.DetachedSessionEvent(eventToken); });
            page->_KeepTabRunning(tab);
            first->TransitionTo(State::Failed);
            VERIFY_IS_TRUE(page->_manager.HasKeptSessions());
            VERIFY_IS_TRUE(page->_manager.BeginReattachKeptGroup(groupId) == *tab);
            VERIFY_ARE_EQUAL(2, tab->GetLeafPaneCount());
            VERIFY_IS_NOT_NULL(tab->GetRootPane()->FindPaneBySessionId(firstId));
            VERIFY_IS_NOT_NULL(tab->GetRootPane()->FindPaneBySessionId(secondId));
            page->_manager.CompleteKeptGroupReattach(groupId, false);
            VERIFY_ARE_EQUAL(0u, second->CloseCount());
            page->_manager.DiscardKeptGroup(groupId);
            VERIFY_IS_FALSE(page->_manager.HasKeptSessions());
            VERIFY_ARE_EQUAL(size_t{ 1 }, _statesForPane(events, _formatPaneId(secondId)).size());
        });
        VERIFY_IS_TRUE(first->WaitForClose());
        VERIFY_IS_TRUE(second->WaitForClose());
    }

    void TabTests::KeepRunningWindowCloseIsIdempotent()
    {
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        const winrt::guid id{ L"{13f7aa41-8837-473e-92a3-f1e682ab1009}" };
        const auto connection = winrt::make_self<TestConnection>(id, State::Connected);
        const auto closed = winrt::make_self<TestConnection>(winrt::guid{ L"{13f7aa41-8837-473e-92a3-f1e682ab1012}" }, State::Connected);
        const auto page = _commonSetup(*connection);
        TestOnUIThread([&]() {
            page->_settings.GlobalSettings().ConfirmOnClose(ConfirmOnClose::Never);
            const auto other = page->_MakeTerminalPane(NewTerminalArgs{}, nullptr, *closed);
            const winrt::guid groupId{ page->_GetFocusedTabImpl()->StableId() };
            page->_CreateNewTabFromPane(other);
            page->OnPaneAgentSessionChanged(_keepRunningHook(id, "agent.session.start"));
            page->SetTabKeepRunning(groupId, true);
            uint32_t closeRequests = 0;
            const auto eventToken = page->CloseWindowRequested([&](auto&&, auto&&) { ++closeRequests; });
            const auto revoke = wil::scope_exit([&]() noexcept { page->CloseWindowRequested(eventToken); });
            page->CloseWindow();
            page->CloseWindow();
            VERIFY_ARE_EQUAL(1u, closeRequests);
            VERIFY_ARE_EQUAL(1u, page->_manager.KeptGroups().Size());
            VERIFY_ARE_EQUAL(0u, connection->CloseCount());
            page->ShutdownPanes();
            VERIFY_ARE_EQUAL(0u, connection->CloseCount());
            VERIFY_IS_TRUE(closed->WaitForClose());
            page->_manager.DiscardKeptGroup(groupId);
            VERIFY_IS_FALSE(page->_manager.HasKeptSessions());
        });
    }

    void TabTests::KeepRunningStartupWaitsForHostRegistration()
    {
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        using Startup = winrt::TerminalApp::implementation::StartupState;
        const winrt::guid id{ L"{13f7aa41-8837-473e-92a3-f1e682ab1013}" };
        const auto connection = winrt::make_self<TestConnection>(id, State::Connected);
        const auto page = _commonSetup(*connection);
        ::details::Event initialized;
        winrt::event_token token{};
        uint64_t contentId{};
        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            const winrt::guid groupId{ tab->StableId() };
            contentId = tab->GetRootPane()->GetTerminalControl().ContentId();
            tab->SuppressAgentPrewarm();
            page->OnPaneAgentSessionChanged(_keepRunningHook(id, "agent.session.start"));
            page->SetTabKeepRunning(groupId, true);
            page->_KeepTabRunning(tab);
            page->_startupState = Startup::NotInitialized;
            page->_transferReceiverReady = false;
            page->SetStartupKeptGroups({ groupId });
            page->Width(0);
            page->Height(0);
            page->_tabContent.Width(0);
            page->_tabContent.Height(0);
            page->UpdateLayout();
            page->_OnFirstLayout(nullptr, nullptr);
            VERIFY_IS_TRUE(page->_startupState == Startup::NotInitialized);
            VERIFY_ARE_EQUAL(0u, page->_tabs.Size());
            VERIFY_IS_TRUE(page->_manager.HasKeptSessions());
            page->Width(900);
            page->Height(600);
            page->_tabContent.Width(900);
            page->_tabContent.Height(600);
            page->UpdateLayout();
            page->_OnFirstLayout(nullptr, nullptr);
            VERIFY_IS_TRUE(page->_startupState == Startup::InStartup);
            VERIFY_IS_FALSE(page->_restoringStartupKeptGroups);
            token = page->Initialized([&](auto&&, auto&&) { initialized.Set(); });
            page->ContentTransferReceiverReady();
            page->ContentTransferReceiverReady();
            VERIFY_ARE_EQUAL(0u, page->_tabs.Size());
            VERIFY_IS_TRUE(page->_restoringStartupKeptGroups);
        });
        const auto revoke = wil::scope_exit([&]() {
            TestOnUIThread([&]() { page->Initialized(token); });
        });
        VERIFY_ARE_EQUAL(static_cast<DWORD>(WAIT_OBJECT_0), WaitForSingleObject(initialized.m_handle, 10000));
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(1u, page->_tabs.Size());
            VERIFY_ARE_EQUAL(contentId, page->_GetFocusedTabImpl()->GetRootPane()->GetTerminalControl().ContentId());
            VERIFY_IS_FALSE(page->_manager.HasKeptSessions());
            VERIFY_IS_FALSE(page->_restoringStartupKeptGroups);
            VERIFY_ARE_EQUAL(0u, connection->CloseCount());
            page->_GetFocusedTabImpl()->Close();
        });
    }

    void TabTests::KeepRunningStartupRestoresBatchAfterLayout()
    {
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        using Stage = winrt::TerminalApp::implementation::TerminalPage::ContentTransferStage;
        for (const auto rejectFirst : { false, true })
        {
            auto fixture = _createContentTransferFixture(false, true, true, false, 100);
            ::details::Event initialized;
            winrt::event_token initializedToken{};
            winrt::event_token closeToken{};
            uint32_t closeRequests{};
            const auto cleanup = wil::scope_exit([&]() {
                TestOnUIThread([&]() {
                    fixture->destination->Initialized(initializedToken);
                    fixture->destination->CloseWindowRequested(closeToken);
                    for (const auto& group : fixture->source->_manager.KeptGroups())
                    {
                        fixture->source->_manager.DiscardKeptGroup(group.Key());
                    }
                    _closeContentTransferFixture(*fixture, false);
                    fixture.reset();
                });
            });
            const auto second = winrt::make_self<TestConnection>(
                winrt::guid{ L"{13f7aa41-8837-473e-92a3-f1e682ab1030}" }, State::Connected);
            winrt::guid firstGroup{};
            winrt::guid secondGroup{};
            uint64_t secondContentId{};
            TestOnUIThread([&]() {
                const auto first = fixture->original.tab;
                firstGroup = winrt::guid{ first->StableId() };
                const auto secondPane = fixture->source->_MakePane(nullptr, nullptr, *second);
                secondContentId = secondPane->GetTerminalControl().ContentId();
                fixture->source->_CreateNewTabFromPane(secondPane);
                const auto secondTab = fixture->source->_GetFocusedTabImpl();
                secondGroup = winrt::guid{ secondTab->StableId() };
                secondTab->SuppressAgentPrewarm();
                first->KeepRunning(true);
                secondTab->KeepRunning(true);
                fixture->source->_KeepTabRunning(first);
                fixture->source->_KeepTabRunning(secondTab);
                const auto bounds = fixture->source->_manager.KeptGroupBounds(firstGroup);
                VERIFY_IS_TRUE(bounds.Width > 0);
                VERIFY_IS_TRUE(bounds.Height > 0);
                const auto loaded = winrt::make<winrt::TerminalApp::implementation::SettingsLoadEventArgs>(
                    false, S_OK, winrt::hstring{}, nullptr, fixture->source->_settings);
                const auto window = winrt::make_self<winrt::TerminalApp::implementation::TerminalWindow>(loaded, fixture->source->_manager);
                const auto position = window->GetInitialPosition(123, 234);
                const auto groups = winrt::single_threaded_vector<winrt::guid>({ firstGroup, secondGroup });
                window->SetStartupKeptGroups(groups.GetView(), bounds);
                VERIFY_ARE_EQUAL(bounds.Width, window->GetLaunchDimensions(96).Width);
                VERIFY_ARE_EQUAL(bounds.Height, window->GetLaunchDimensions(96).Height);
                VERIFY_ARE_EQUAL(bounds.Width * 1.5f, window->GetLaunchDimensions(144).Width);
                VERIFY_ARE_EQUAL(bounds.Height * 1.5f, window->GetLaunchDimensions(144).Height);
                const auto restoredPosition = window->GetInitialPosition(123, 234);
                VERIFY_ARE_EQUAL(position.X, restoredPosition.X);
                VERIFY_ARE_EQUAL(position.Y, restoredPosition.Y);
                fixture->source->ShutdownPanes();
                const auto destination = fixture->destination;
                destination->SetStartupKeptGroups({ firstGroup, secondGroup });
                initializedToken = destination->Initialized([&](auto&&, auto&&) { initialized.Set(); });
                closeToken = destination->CloseWindowRequested([&](auto&&, auto&&) { ++closeRequests; });
                if (rejectFirst)
                {
                    destination->_contentTransferTestHook = [](Stage stage, uint64_t, uint32_t) {
                        THROW_HR_IF(E_ABORT, stage == Stage::BeforeSplitInsertion);
                    };
                }
                destination->_OnFirstLayout(nullptr, nullptr);
                VERIFY_ARE_EQUAL(0u, destination->_tabs.Size());
                destination->ContentTransferReceiverReady();
                destination->ContentTransferReceiverReady();
                VERIFY_ARE_EQUAL(0u, destination->_tabs.Size());
                VERIFY_ARE_EQUAL(2u, fixture->source->_manager.KeptGroups().Size());
            });
            VERIFY_ARE_EQUAL(static_cast<DWORD>(WAIT_OBJECT_0), WaitForSingleObject(initialized.m_handle, 10000));
            TestOnUIThread([&]() {
                const auto destination = fixture->destination;
                VERIFY_ARE_EQUAL(rejectFirst ? 1u : 2u, destination->_tabs.Size());
                VERIFY_ARE_EQUAL(0u, closeRequests);
                VERIFY_IS_FALSE(destination->_restoringStartupKeptGroups);
                VERIFY_IS_TRUE(destination->_startupKeptGroups.empty());
                const auto secondTab = destination->_FindTabByStableId(winrt::to_hstring(secondGroup));
                VERIFY_IS_NOT_NULL(secondTab);
                VERIFY_ARE_EQUAL(secondContentId, secondTab->GetActiveTerminalControl().ContentId());
                VERIFY_IS_TRUE(secondTab->GetActiveTerminalControl().Connection() == *second);
                VERIFY_ARE_EQUAL(0u, second->CloseCount());
                VERIFY_ARE_EQUAL(rejectFirst, fixture->source->_manager.KeptGroups().HasKey(firstGroup));
                for (const auto& leaf : fixture->original.leaves)
                {
                    VERIFY_ARE_EQUAL(0u, leaf.closed->load());
                }
                if (!rejectFirst)
                {
                    const auto first = destination->_FindTabByStableId(fixture->original.stableId);
                    VERIFY_IS_NOT_NULL(first);
                    VERIFY_ARE_EQUAL(3, first->GetLeafPaneCount());
                    VERIFY_IS_TRUE(first->HasStashedAgentPane());
                }
                destination->ContentTransferReceiverReady();
                destination->_OnFirstLayout(nullptr, nullptr);
                VERIFY_ARE_EQUAL(rejectFirst ? 1u : 2u, destination->_tabs.Size());
            });
        }
    }

    void TabTests::KeepRunningPreservesAssistantAndLayout()
    {
        for (const auto hidden : { false, true })
        {
            auto fixture = _createContentTransferFixture(false, hidden, false, false, 100);
            const auto cleanup = wil::scope_exit([&]() {
                TestOnUIThread([&]() {
                    for (const auto& group : fixture->source->_manager.KeptGroups())
                    {
                        fixture->source->_manager.DiscardKeptGroup(group.Key());
                    }
                    fixture.reset();
                });
            });
            TestOnUIThread([&]() {
                const auto source = fixture->source;
                const auto destination = fixture->destination;
                const auto tab = fixture->original.tab;
                const winrt::guid id{ tab->StableId() };
                tab->ToggleSplitOrientation();
                tab->SetTabText(L"Kept tab");
                tab->SetRuntimeTabColor(winrt::Windows::UI::Colors::Orange());
                if (!hidden)
                {
                    tab->ToggleZoom();
                }
                else
                {
                    tab->HidePane();
                    VERIFY_IS_TRUE(tab->HasHiddenPane());
                }
                const auto activeSession = tab->GetActivePane()->GetTerminalControl().Connection().SessionId();
                const auto layoutOf = [](const auto& target) {
                    auto actions = target->BuildStartupActions(BuildStartupKind::Content);
                    // Transfer IDs are wrapper-local, not part of the split layout.
                    for (const auto& action : actions)
                    {
                        if (const auto newTab = action.Args().template try_as<NewTabArgs>())
                        {
                            action.Args(NewTabArgs{ NewTerminalArgs{} });
                        }
                        else if (const auto split = action.Args().template try_as<SplitPaneArgs>())
                        {
                            action.Args(SplitPaneArgs{ SplitType::Manual, split.SplitDirection(), split.SplitSize(), NewTerminalArgs{} });
                        }
                    }
                    return ActionAndArgs::Serialize(winrt::single_threaded_vector(std::move(actions)));
                };
                const auto layout = layoutOf(tab);
                source->SetTabKeepRunning(id, true);
                source->_HandleCloseTabRequested(*tab, true);
                VERIFY_ARE_EQUAL(0u, source->_tabs.Size());
                VERIFY_IS_TRUE(source->_previouslyClosedPanesAndTabs.empty());
                VERIFY_IS_TRUE(tab->FindAgentPaneContent() == fixture->agent);
                VERIFY_IS_TRUE(winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(fixture->agent)->HasLifetime());

                source->ShutdownPanes();
                Json::Value status;
                status["params"]["tab_id"] = winrt::to_string(tab->StableId());
                status["params"]["name"] = "Background agent";
                status["params"]["model"] = "background-model";
                status["params"]["state"] = "connected";
                source->OnAgentStatusChanged(winrt::to_hstring(Json::writeString(Json::StreamWriterBuilder{}, status)));
                for (const auto& leaf : fixture->original.leaves)
                {
                    VERIFY_ARE_EQUAL(0u, leaf.closed->load());
                }
                VERIFY_IS_TRUE(destination->RestoreKeptGroup(id));
                const auto restored = destination->_GetFocusedTabImpl();
                VERIFY_ARE_EQUAL(tab->StableId(), restored->StableId());
                VERIFY_ARE_EQUAL(winrt::hstring{ L"Kept tab" }, restored->GetTabText());
                VERIFY_IS_TRUE(restored->GetTabColor() == winrt::Windows::UI::Colors::Orange());
                VERIFY_ARE_EQUAL(!hidden, restored->IsZoomed());
                VERIFY_ARE_EQUAL(hidden, restored->HasStashedAgentPane());
                VERIFY_ARE_EQUAL(hidden, restored->HasHiddenPane());
                VERIFY_IS_TRUE(restored->KeepRunning());
                VERIFY_ARE_EQUAL(activeSession, restored->GetActivePane()->GetTerminalControl().Connection().SessionId());
                VERIFY_ARE_EQUAL(3, restored->GetLeafPaneCount());
                const auto agent = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(restored->FindAgentPaneContent());
                VERIFY_IS_TRUE(agent->HasLifetime());
                VERIFY_ARE_EQUAL(winrt::hstring{ L"Background agent" }, agent->GetAgentName());
                VERIFY_ARE_EQUAL(winrt::hstring{ L"background-model" }, agent->GetAgentModel());
                VERIFY_ARE_EQUAL(restored->StableId(), agent->TransferSourceTabId());
                destination->OnAgentStatusChanged(winrt::to_hstring(Json::writeString(Json::StreamWriterBuilder{}, status)));
                VERIFY_IS_TRUE(agent->TransferSourceTabId().empty());
                _verifyTransferredAgentRestoreState(*fixture, destination, restored);
                VERIFY_ARE_EQUAL(layout, layoutOf(restored));
                for (const auto& leaf : fixture->original.leaves)
                {
                    const auto pane = restored->GetRootPane()->FindPaneBySessionId(leaf.connection.SessionId());
                    VERIFY_IS_NOT_NULL(pane);
                    VERIFY_ARE_EQUAL(leaf.contentId, pane->GetTerminalControl().ContentId());
                    VERIFY_IS_TRUE(pane->GetTerminalControl().Connection() == leaf.connection);
                    VERIFY_IS_TRUE(destination->_manager.TryLookupCore(leaf.contentId) == leaf.core);
                    VERIFY_ARE_EQUAL(0u, leaf.closed->load());
                }
                VERIFY_IS_FALSE(source->_manager.HasKeptSessions());
                restored->Close();
            });
        }
    }

    void TabTests::KeepRunningHeadlessProtocolRetainsPaneRouting()
    {
        using namespace winrt::Windows::Foundation;
        using namespace winrt::Microsoft::Terminal::Protocol;
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        const winrt::guid paneId{ L"{13f7aa41-8837-473e-92a3-f1e682ab1014}" };
        const auto connection = winrt::make_self<TestConnection>(paneId, State::Connected);
        const auto page = _commonSetup(*connection);
        winrt::guid tabId;
        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            tabId = winrt::guid{ tab->StableId() };
            page->SetTabKeepRunning(tabId, true);
            page->_KeepTabRunning(tab);
            page->ShutdownPanes();
            VERIFY_ARE_EQUAL(0u, page->_tabs.Size());
            VERIFY_ARE_EQUAL(1u, page->_manager.KeptPages().Size());
            VERIFY_ARE_EQUAL(0u, connection->CloseCount());
        });
        const auto cleanup = wil::scope_exit([&]() {
            TestOnUIThread([&]() { page->_manager.DiscardKeptGroup(tabId); });
        });
        IAsyncOperation<bool> input{ nullptr };
        TestOnUIThread([&]() { input = page->SendProtocolInput(paneId, L"headless output"); });
        VERIFY_IS_TRUE(input.get());

        IAsyncOperation<PaneContext> context{ nullptr };
        IAsyncOperation<ProcessStatus> process{ nullptr };
        IAsyncOperation<Collections::IVector<PaneInfo>> panes{ nullptr };
        TestOnUIThread([&]() {
            context = page->GetProtocolPaneContext(paneId, true, 100, 1000);
            process = page->GetProtocolProcessStatus(paneId);
            panes = page->GetProtocolPanes(UINT32_MAX);
        });
        const auto captured = context.get();
        VERIFY_ARE_EQUAL(paneId, captured.Pane.SessionId);
        VERIFY_IS_TRUE(std::wstring_view{ captured.Content }.find(L"headless output") != std::wstring_view::npos);
        VERIFY_ARE_EQUAL(winrt::hstring{ L"running" }, process.get().State);
        VERIFY_ARE_EQUAL(1u, panes.get().Size());
        VERIFY_ARE_EQUAL(paneId, panes.get().GetAt(0).SessionId);
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

    void TabTests::KeepRunningFocusReattachesOriginalTab()
    {
        auto fixture = _createContentTransferFixture(false, true, false, false, 100);
        const auto cleanup = wil::scope_exit([&]() {
            TestOnUIThread([&]() {
                for (const auto& group : fixture->source->_manager.KeptGroups())
                {
                    fixture->source->_manager.DiscardKeptGroup(group.Key());
                }
                _closeContentTransferFixture(*fixture, false);
                fixture.reset();
            });
        });
        winrt::guid paneId{};
        winrt::guid groupId{};
        winrt::Windows::Foundation::IAsyncOperation<bool> focus{ nullptr };
        TestOnUIThread([&]() {
            const auto tab = fixture->original.tab;
            groupId = winrt::guid{ tab->StableId() };
            paneId = fixture->original.leaves.front().connection.SessionId();
            tab->KeepRunning(true);
            tab->ToggleZoom();
            fixture->source->_KeepTabRunning(tab);
            fixture->source->ShutdownPanes();
            VERIFY_ARE_EQUAL(groupId, fixture->source->_manager.KeptGroupForPane(paneId));
            fixture->destination->_tabStrip.HistoryActive(true);
            winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(fixture->destination->_tabStrip)->HistorySearchTextBox().Text(L"kept");
            focus = fixture->destination->FocusProtocolPane(paneId);
        });
        VERIFY_IS_TRUE(focus.get());
        TestOnUIThread([&]() {
            const auto restored = fixture->destination->_GetFocusedTabImpl();
            VERIFY_ARE_EQUAL(groupId, winrt::guid{ restored->StableId() });
            VERIFY_ARE_EQUAL(paneId, restored->GetActivePane()->GetTerminalControl().Connection().SessionId());
            VERIFY_ARE_EQUAL(fixture->destinationTabs.size() + 1, static_cast<size_t>(fixture->destination->_tabs.Size()));
            VERIFY_IS_TRUE(fixture->destination->_tabStrip.HistoryActive());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"kept" },
                             winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(fixture->destination->_tabStrip)->HistorySearchTextBox().Text());
            VERIFY_IS_FALSE(fixture->destination->_preserveSidebarHistory);
            VERIFY_IS_FALSE(fixture->source->_manager.HasKeptSessions());
            VERIFY_IS_TRUE(restored->KeepRunning());
            VERIFY_IS_TRUE(restored->HasStashedAgentPane());
            VERIFY_IS_TRUE(winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(restored->FindAgentPaneContent())->HasLifetime());
            for (const auto& leaf : fixture->original.leaves)
            {
                const auto pane = restored->GetRootPane()->FindPaneBySessionId(leaf.connection.SessionId());
                VERIFY_IS_NOT_NULL(pane);
                VERIFY_ARE_EQUAL(leaf.contentId, pane->GetTerminalControl().ContentId());
                VERIFY_IS_TRUE(pane->GetTerminalControl().Connection() == leaf.connection);
                VERIFY_ARE_EQUAL(0u, leaf.closed->load());
            }
            focus = fixture->destination->FocusProtocolPane(paneId);
        });
        VERIFY_IS_TRUE(focus.get());
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(fixture->destinationTabs.size() + 1, static_cast<size_t>(fixture->destination->_tabs.Size()));
            for (const auto& connection : fixture->connections)
            {
                VERIFY_ARE_EQUAL(0u, connection->CloseCount());
            }
        });
    }

    void TabTests::KeepRunningFocusPreservesFailedRestore()
    {
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        using Stage = winrt::TerminalApp::implementation::TerminalPage::ContentTransferStage;
        const winrt::guid firstId{ L"{13f7aa41-8837-473e-92a3-f1e682ab1020}" };
        const winrt::guid secondId{ L"{13f7aa41-8837-473e-92a3-f1e682ab1021}" };
        const auto first = winrt::make_self<TestConnection>(firstId, State::Connected);
        const auto second = winrt::make_self<TestConnection>(secondId, State::Connected);
        const auto page = _commonSetup(*first);
        winrt::guid groupId{};
        winrt::Windows::Foundation::IAsyncOperation<bool> focus{ nullptr };
        const auto cleanup = wil::scope_exit([&]() {
            TestOnUIThread([&]() {
                page->_contentTransferTestHook = {};
                for (const auto& group : page->_manager.KeptGroups())
                {
                    page->_manager.DiscardKeptGroup(group.Key());
                }
                while (page->_tabs.Size())
                {
                    page->_RemoveTab(page->_tabs.GetAt(0));
                }
            });
        });
        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            groupId = winrt::guid{ tab->StableId() };
            const auto split = page->_MakeTerminalPane(NewTerminalArgs{}, nullptr, *second);
            VERIFY_IS_TRUE(page->_SplitPane(tab, SplitDirection::Right, 0.35f, split));
            page->_CreateNewTabFromPane(page->_MakePane(nullptr, nullptr, nullptr));
            page->SetTabKeepRunning(groupId, true);
            page->_KeepTabRunning(tab);
            VERIFY_ARE_EQUAL(winrt::guid{}, page->_manager.KeptGroupForPane(winrt::guid{}));
            focus = page->FocusProtocolPane(winrt::guid{ L"{13f7aa41-8837-473e-92a3-f1e682ab1022}" });
        });
        VERIFY_IS_FALSE(focus.get());
        TestOnUIThread([&]() {
            VERIFY_IS_TRUE(page->_manager.KeptGroups().HasKey(groupId));
            page->_manager.BeginReattachKeptGroup(groupId);
            focus = page->FocusProtocolPane(firstId);
        });
        VERIFY_THROWS(focus.get(), winrt::hresult_error);
        TestOnUIThread([&]() {
            page->_manager.CompleteKeptGroupReattach(groupId, false);
            page->_contentTransferTestHook = [](Stage stage, uint64_t, uint32_t) {
                THROW_HR_IF(E_ABORT, stage == Stage::BeforeSplitInsertion);
            };
            focus = page->FocusProtocolPane(firstId);
        });
        VERIFY_THROWS(focus.get(), winrt::hresult_error);
        TestOnUIThread([&]() {
            page->_contentTransferTestHook = {};
            VERIFY_ARE_EQUAL(1u, page->_tabs.Size());
            VERIFY_IS_TRUE(page->_manager.KeptGroups().HasKey(groupId));
            VERIFY_ARE_EQUAL(0u, first->CloseCount());
            VERIFY_ARE_EQUAL(0u, second->CloseCount());
            focus = page->FocusProtocolPane(firstId);
        });
        VERIFY_IS_TRUE(focus.get());
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(2u, page->_tabs.Size());
            VERIFY_IS_FALSE(page->_manager.HasKeptSessions());
            VERIFY_IS_TRUE(page->_GetFocusedTabImpl()->GetActivePane()->GetTerminalControl().Connection() == *first);
        });
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

            const auto event = [&](const std::string_view name, const std::string_view sessionId = "agent-session-resumed") {
                Json::Value evt;
                evt["params"]["pane_id"] = paneId;
                evt["params"]["event"] = std::string{ name };
                evt["params"]["agent_session_id"] = std::string{ sessionId };
                evt["params"]["agent"] = "copilot";
                Json::StreamWriterBuilder writer;
                writer["indentation"] = "";
                page->OnPaneAgentSessionChanged(winrt::to_hstring(Json::writeString(writer, evt)));
            };

            event("agent.session.start");
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_paneAgentSessions.count(paneSessionId)));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_activeCliAgentPanes.count(paneSessionId)));

            // A late end naming a different agent session must not clear the
            // binding or active marker a newer session just installed.
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
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_activeCliAgentPanes.count(paneSessionId)));

            // The agent that ran in this pane exited, so there is nothing left
            // to resume and the pane restores as a plain shell.
            event("agent.session.end");
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_paneAgentSessions.count(paneSessionId)));
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_activeCliAgentPanes.count(paneSessionId)));

            // A new lifecycle may begin before its session id is known. Retire
            // the previous binding, ignore its delayed end, then accept the
            // current lifecycle's id when it first appears on the end event.
            event("agent.session.start", "agent-session-previous");
            event("agent.session.start", "");
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_paneAgentSessions.count(paneSessionId)));
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_activeCliAgentPanes.count(paneSessionId)));
            event("agent.session.end", "agent-session-previous");
            VERIFY_ARE_EQUAL(1u, static_cast<unsigned int>(page->_activeCliAgentPanes.count(paneSessionId)));
            event("agent.session.end", "agent-session-current");
            VERIFY_ARE_EQUAL(0u, static_cast<unsigned int>(page->_activeCliAgentPanes.count(paneSessionId)));
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

    void TabTests::VerticalRailVisibilityRestoresWidth()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            VERIFY_IS_TRUE(page->_isVerticalLayout);

            page->_verticalRailWidth = 333.0;
            page->_SetVerticalRailVisibility(false);

            VERIFY_ARE_EQUAL(Visibility::Collapsed, page->_tabView.Visibility());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, page->_tabStrip.Visibility());
            VERIFY_ARE_EQUAL(0.0, page->VerticalRailColumn().Width().Value);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, page->_verticalRailSplitter.Visibility());
            VERIFY_IS_FALSE(page->_verticalRailSplitter.IsHitTestVisible());

            page->_SetVerticalRailVisibility(true);

            VERIFY_ARE_EQUAL(Visibility::Collapsed, page->_tabView.Visibility());
            VERIFY_ARE_EQUAL(Visibility::Visible, page->_tabStrip.Visibility());
            VERIFY_ARE_EQUAL(333.0, page->VerticalRailColumn().Width().Value);
            VERIFY_ARE_EQUAL(Visibility::Visible, page->_verticalRailSplitter.Visibility());
            VERIFY_IS_TRUE(page->_verticalRailSplitter.IsHitTestVisible());

        });
    }

    void TabTests::VerticalRailCollapseRestoresWidth()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            page->_verticalRailWidth = 333.0;
            page->_SetVerticalRailVisibility(true);
            const auto firstTabItem = page->_tabs.GetAt(0).TabViewItem();
            VERIFY_IS_NOT_NULL(firstTabItem.ContextFlyout());

            page->_OnVerticalRailCollapseRequested(nullptr, nullptr);

            VERIFY_IS_TRUE(page->_isVerticalRailCollapsed);
            VERIFY_IS_TRUE(page->_tabStrip.IsRailCollapsed());
            VERIFY_ARE_EQUAL(Visibility::Visible, page->_tabStrip.Visibility());
            const auto tabStrip = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            VERIFY_ARE_EQUAL(Visibility::Visible, tabStrip->CompactNewTabToolbar().Visibility());
            VERIFY_ARE_EQUAL(Visibility::Visible, tabStrip->SearchTabsButton().Visibility());
            VERIFY_ARE_EQUAL(Visibility::Visible, tabStrip->ItemsList().Visibility());
            VERIFY_IS_TRUE(tabStrip->CompactNewTabButton().IsHitTestVisible());
            VERIFY_IS_TRUE(tabStrip->CompactNewTabMenuButton().IsHitTestVisible());
            VERIFY_IS_TRUE(tabStrip->SearchTabsButton().IsHitTestVisible());
            VERIFY_IS_TRUE(tabStrip->SearchTabsButton().IsEnabled());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, tabStrip->FilterTabsButton().Visibility());
            VERIFY_IS_FALSE(tabStrip->FilterTabsButton().IsHitTestVisible());
            VERIFY_IS_FALSE(tabStrip->FilterStatusBar().IsHitTestVisible());
            VERIFY_IS_FALSE(tabStrip->ItemsList().AllowDrop());
            VERIFY_IS_FALSE(tabStrip->ItemsList().CanDragItems());
            VERIFY_IS_FALSE(tabStrip->ItemsList().CanReorderItems());
            VERIFY_IS_NULL(firstTabItem.ContextFlyout());
            VERIFY_ARE_EQUAL(40.0, page->VerticalRailColumn().Width().Value);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, page->_verticalRailSplitter.Visibility());
            VERIFY_IS_FALSE(page->_verticalRailSplitter.IsHitTestVisible());

            tabStrip->SearchTabsButton().IsChecked(true);
            tabStrip->OnSearchToggleClick(nullptr, nullptr);

            VERIFY_IS_FALSE(page->_isVerticalRailCollapsed);
            VERIFY_IS_FALSE(page->_tabStrip.IsRailCollapsed());
            VERIFY_IS_TRUE(page->_tabSearchActive);
            VERIFY_IS_TRUE(page->_tabStrip.SearchActive());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, tabStrip->CompactNewTabToolbar().Visibility());
            VERIFY_IS_TRUE(tabStrip->SearchTabsButton().IsHitTestVisible());
            VERIFY_ARE_EQUAL(Visibility::Visible, tabStrip->FilterTabsButton().Visibility());
            VERIFY_IS_TRUE(tabStrip->FilterTabsButton().IsHitTestVisible());
            VERIFY_IS_TRUE(tabStrip->FilterStatusBar().IsHitTestVisible());
            VERIFY_IS_TRUE(tabStrip->ItemsList().AllowDrop());
            VERIFY_ARE_EQUAL(page->CanDragDrop(), tabStrip->ItemsList().CanDragItems());
            VERIFY_ARE_EQUAL(page->CanDragDrop(), tabStrip->ItemsList().CanReorderItems());
            VERIFY_IS_NOT_NULL(firstTabItem.ContextFlyout());
            VERIFY_ARE_EQUAL(333.0, page->VerticalRailColumn().Width().Value);
            VERIFY_ARE_EQUAL(Visibility::Visible, page->_verticalRailSplitter.Visibility());
            VERIFY_IS_TRUE(page->_verticalRailSplitter.IsHitTestVisible());

            page->_tabFilterMode = winrt::TerminalApp::TabStripFilterMode::AgentsOnly;
            page->_ApplyTabListProjection();
            VERIFY_IS_FALSE(tabStrip->ItemsList().CanDragItems());
            VERIFY_IS_FALSE(tabStrip->ItemsList().CanReorderItems());

            page->_OnVerticalRailCollapseRequested(nullptr, nullptr);
            page->_OnVerticalRailCollapseRequested(nullptr, nullptr);
            VERIFY_IS_FALSE(page->_isVerticalRailCollapsed);
            VERIFY_IS_FALSE(tabStrip->ItemsList().CanDragItems());
            VERIFY_IS_FALSE(tabStrip->ItemsList().CanReorderItems());
        });
    }

    void TabTests::FreTabModeSelectionDoesNotMutateSettings()
    {
        TestOnUIThread([]() {
            winrt::TerminalApp::FreOverlay fre;
            for (const auto configured : { std::optional<TabLayout>{}, std::optional{ TabLayout::Horizontal }, std::optional{ TabLayout::Vertical } })
            {
                CascadiaSettings settings{ LR"({"profiles":[{"name":"cmd","commandline":"cmd.exe"}]})", {} };
                const auto globals = settings.GlobalSettings();
                if (configured)
                {
                    globals.TabLayout(*configured);
                }

                fre.Initialize(settings);
                const auto picker = fre.FindName(L"TabModeComboBox").try_as<ComboBox>();
                VERIFY_IS_NOT_NULL(picker);
                VERIFY_ARE_EQUAL(2u, picker.Items().Size());
                VERIFY_ARE_EQUAL(configured == TabLayout::Horizontal ? 1 : 0, picker.SelectedIndex());
                VERIFY_IS_FALSE(Automation::AutomationProperties::GetName(picker).empty());
                VERIFY_IS_FALSE(Automation::AutomationProperties::GetHelpText(picker).empty());

                picker.SelectedIndex(1 - picker.SelectedIndex());
                VERIFY_ARE_EQUAL(configured.has_value(), globals.HasTabLayout());
                VERIFY_ARE_EQUAL(configured.value_or(TabLayout::Horizontal), globals.TabLayout());
            }
        });
    }

    void TabTests::FreIllustrationsFollowThemeWithoutChangingChrome()
    {
        TestOnUIThread([]() {
            winrt::TerminalApp::FreOverlay fre;
            for (const auto theme : { ElementTheme::Light, ElementTheme::Dark, ElementTheme::Default, ElementTheme::Light })
            {
                fre.RequestedTheme(theme);
                const auto actualTheme = fre.ActualTheme();
                VERIFY_ARE_NOT_EQUAL(ElementTheme::Default, actualTheme);
                if (theme != ElementTheme::Default)
                {
                    VERIFY_ARE_EQUAL(theme, actualTheme);
                }
                VERIFY_ARE_EQUAL(ElementTheme::Dark, fre.FindName(L"RootGrid").as<Grid>().RequestedTheme());
                for (const auto name : { L"SidebarImage", L"AutofixImage" })
                {
                    const auto image = fre.FindName(name).as<Image>();
                    VERIFY_ARE_EQUAL(actualTheme, image.RequestedTheme());
                    const auto source = image.Source().as<Media::Imaging::BitmapImage>().UriSource().AbsoluteUri();
                    const winrt::hstring expectedSource{ std::wstring_view{ name } == L"SidebarImage" ? L"ms-appx:///FREAssets/sidebar.png" : L"ms-appx:///FREAssets/Error-detection.png" };
                    VERIFY_ARE_EQUAL(expectedSource, source);
                }
            }
        });
    }

    void TabTests::EmptyTabLayoutChangeCompletesBeforeStartup()
    {
        _createContentManager();
        TestOnUIThread([&]() {
            const auto props = winrt::make_self<winrt::TerminalApp::implementation::WindowProperties>();
            winrt::TerminalApp::TerminalPage projectedPage{ *props, *_contentManager };
            const auto page = winrt::get_self<winrt::TerminalApp::implementation::TerminalPage>(projectedPage);
            page->_settings = CascadiaSettings{ LR"({"profiles":[{"name":"cmd","commandline":"cmd.exe"}]})", {} };
            page->_terminalSettingsCache = std::make_shared<winrt::TerminalApp::implementation::TerminalSettingsCache>(page->_settings);
            page->Create();

            VERIFY_ARE_EQUAL(0u, page->_tabs.Size());
            VERIFY_ARE_EQUAL(Visibility::Visible, page->SessionToggleButton().Visibility());
            VERIFY_IS_TRUE(page->_ApplyTabLayout(TabLayout::Vertical));
            VERIFY_IS_TRUE(page->_isVerticalLayout);
            VERIFY_IS_FALSE(page->_changingTabLayout);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, page->SessionToggleButton().Visibility());
            VERIFY_IS_TRUE(page->_ApplyTabLayout(TabLayout::Horizontal));
            VERIFY_IS_FALSE(page->_isVerticalLayout);
            VERIFY_IS_FALSE(page->_changingTabLayout);
            VERIFY_ARE_EQUAL(Visibility::Visible, page->SessionToggleButton().Visibility());
        });
    }

    void TabTests::LiveTabLayoutRoundTripPreservesState()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            page->_settings.GlobalSettings().UseAcrylicInTabRow(true);
            page->WindowActivated(true);
            VERIFY_IS_NOT_NULL(page->_tabStrip.Background().try_as<Media::AcrylicBrush>());

            const auto infoBar = page->FindName(L"TabLayoutRestartInfoBar").as<winrt::Microsoft::UI::Xaml::Controls::InfoBar>();
            VERIFY_IS_FALSE(infoBar.IsOpen());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, page->SessionToggleButton().Visibility());

            const auto selectedItem = page->_selectedTabItem();
            VERIFY_IS_NOT_NULL(selectedItem);
            const auto selectedTabItem = selectedItem.as<winrt::MUX::Controls::TabViewItem>();
            const auto focusedTab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(focusedTab);
            VERIFY_IS_TRUE(page->_SplitPane(
                focusedTab,
                SplitDirection::Right,
                0.5f,
                page->_MakePane(nullptr, page->_GetFocusedTab(), nullptr)));
            page->_RefreshTabStripPaneItems(focusedTab);
            VERIFY_ARE_EQUAL(2, focusedTab->GetLeafPaneCount());
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            const auto verticalHeader = stripImpl->HeaderForTab(selectedTabItem);
            VERIFY_IS_NOT_NULL(verticalHeader);
            VERIFY_IS_NOT_NULL(verticalHeader.as<FrameworkElement>().Parent());
            page->_tabRow.ShowElevationShield(true);
            const auto tabRowImpl = winrt::get_self<winrt::TerminalApp::implementation::TabRowControl>(page->_tabRow);
            VERIFY_ARE_EQUAL(Visibility::Visible, tabRowImpl->ElevationShieldIcon().Visibility());
            const auto horizontalNewTabButton = tabRowImpl->NewTabButton();
            const auto verticalNewTabButton = tabRowImpl->VerticalNewTabButton();
            VERIFY_IS_TRUE(winrt::get_abi(horizontalNewTabButton) != winrt::get_abi(verticalNewTabButton));
            const auto horizontalNewTabParent = horizontalNewTabButton.Parent();
            const auto verticalNewTabParent = verticalNewTabButton.Parent();
            VERIFY_IS_NOT_NULL(horizontalNewTabParent);
            VERIFY_IS_NOT_NULL(verticalNewTabParent);

            page->_verticalRailWidth = 333.0;
            page->_SetVerticalRailVisibility(true);
            page->_OnVerticalRailCollapseRequested(nullptr, nullptr);
            VERIFY_IS_TRUE(page->_isVerticalRailCollapsed);

            page->_settings.GlobalSettings().TabLayout(TabLayout::Horizontal);
            page->SetSettings(page->_settings, false);
            page->_CompleteTabLayoutChange(page->_tabLayoutGeneration);
            VERIFY_IS_FALSE(infoBar.IsOpen());
            VERIFY_IS_FALSE(page->_isVerticalLayout);
            VERIFY_ARE_EQUAL(Visibility::Visible, page->SessionToggleButton().Visibility());
            VERIFY_ARE_EQUAL(page->_tabs.Size(), page->_tabView.TabItems().Size());
            VERIFY_ARE_EQUAL(0u, page->_tabStrip.TabItems().Size());
            VERIFY_IS_TRUE(page->_tabView.SelectedItem() == selectedItem);
            VERIFY_IS_TRUE(selectedTabItem.Header() == verticalHeader);
            VERIFY_ARE_EQUAL(Visibility::Visible, tabRowImpl->ElevationShieldIcon().Visibility());
            VERIFY_IS_TRUE(horizontalNewTabButton.Parent() == horizontalNewTabParent);
            VERIFY_IS_TRUE(verticalNewTabButton.Parent() == verticalNewTabParent);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, page->_tabStrip.Visibility());
            VERIFY_IS_TRUE(page->_tabRow.Background() == page->TitlebarBrush());
            VERIFY_IS_NOT_NULL(page->_tabRow.Background().try_as<Media::AcrylicBrush>());
            VERIFY_ARE_EQUAL(0.0, page->VerticalRailColumn().Width().Value);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, page->_verticalRailSplitter.Visibility());

            const auto tabItem = page->_tabs.GetAt(0).TabViewItem();
            VERIFY_IS_TRUE(std::isnan(tabItem.Width()));
            VERIFY_ARE_EQUAL(Visibility::Visible, tabItem.Header().as<UIElement>().Visibility());

            page->_settings.GlobalSettings().TabLayout(TabLayout::Vertical);
            page->SetSettings(page->_settings, false);
            page->_CompleteTabLayoutChange(page->_tabLayoutGeneration);
            VERIFY_IS_FALSE(infoBar.IsOpen());
            VERIFY_IS_TRUE(page->_isVerticalLayout);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, page->SessionToggleButton().Visibility());
            VERIFY_ARE_EQUAL(0u, page->_tabView.TabItems().Size());
            VERIFY_ARE_EQUAL(page->_tabs.Size(), page->_tabStrip.TabItems().Size());
            VERIFY_IS_TRUE(page->_tabStrip.SelectedItem() == selectedItem);
            VERIFY_IS_TRUE(stripImpl->HeaderForTab(selectedTabItem) == verticalHeader);
            VERIFY_IS_NOT_NULL(verticalHeader.as<FrameworkElement>().Parent());
            VERIFY_ARE_EQUAL(Visibility::Visible, tabRowImpl->ElevationShieldIcon().Visibility());
            VERIFY_IS_TRUE(horizontalNewTabButton.Parent() == horizontalNewTabParent);
            VERIFY_IS_TRUE(verticalNewTabButton.Parent() == verticalNewTabParent);
            VERIFY_ARE_EQUAL(Visibility::Visible, page->_tabStrip.Visibility());
            VERIFY_IS_TRUE(page->_tabStrip.Background() == page->TitlebarBrush());
            VERIFY_IS_NOT_NULL(page->_tabStrip.Background().try_as<Media::AcrylicBrush>());
            VERIFY_IS_TRUE(page->_isVerticalRailCollapsed);
            VERIFY_ARE_EQUAL(40.0, page->VerticalRailColumn().Width().Value);
            VERIFY_ARE_EQUAL(333.0, page->_verticalRailWidth);
        });
    }

    void TabTests::VerticalTabChromeBackgroundTracksTheme()
    {
        const CascadiaSettings settings{ LR"({
            "defaultProfile": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
            "showTabsInTitlebar": false,
            "tabLayout": "vertical",
            "theme": "chrome",
            "themes": [{
                "name": "chrome",
                "window": { "applicationTheme": "dark" },
                "tabRow": { "background": "#123456", "unfocusedBackground": "#654321" }
            }],
            "profiles": [{
                "name": "profile0",
                "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
                "closeOnExit": "never"
            }]
        })",
                                         {} };
        winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage> page;
        _initializeTerminalPage(page, settings);

        TestOnUIThread([&]() {
            const auto globals = page->_settings.GlobalSettings();
            for (const auto hasTitlebarHost : { false, true })
            {
                page->_hasTitlebarHost = hasTitlebarHost;
                for (const auto useAcrylic : { false, true })
                {
                    globals.UseAcrylicInTabRow(useAcrylic);
                    for (const auto unfocusedAcrylic : { false, true })
                    {
                        globals.EnableUnfocusedAcrylic(unfocusedAcrylic);
                        for (const auto activated : { false, true })
                        {
                            page->WindowActivated(activated);
                            const auto brush = page->TitlebarBrush();
                            VERIFY_IS_TRUE(page->_tabStrip.Background() == brush);
                            const auto expectedColor = activated ?
                                                           winrt::Windows::UI::ColorHelper::FromArgb(255, 0x12, 0x34, 0x56) :
                                                           winrt::Windows::UI::ColorHelper::FromArgb(255, 0x65, 0x43, 0x21);
                            if (useAcrylic && (activated || unfocusedAcrylic))
                            {
                                const auto acrylic = brush.try_as<Media::AcrylicBrush>();
                                VERIFY_IS_NOT_NULL(acrylic);
                                VERIFY_ARE_EQUAL(Media::AcrylicBackgroundSource::HostBackdrop, acrylic.BackgroundSource());
                                VERIFY_ARE_EQUAL(0.5, acrylic.TintOpacity());
                                VERIFY_ARE_EQUAL(expectedColor, acrylic.TintColor());
                                VERIFY_ARE_EQUAL(expectedColor, acrylic.FallbackColor());
                                page->_updateThemeColors();
                                VERIFY_IS_TRUE(page->TitlebarBrush() == brush);
                            }
                            else
                            {
                                VERIFY_ARE_EQUAL(expectedColor, brush.as<Media::SolidColorBrush>().Color());
                            }

                            if (hasTitlebarHost)
                            {
                                VERIFY_ARE_EQUAL(uint8_t{ 0 }, page->_tabRow.Background().as<Media::SolidColorBrush>().Color().A);
                            }
                            else
                            {
                                VERIFY_IS_TRUE(page->_tabRow.Background() == brush);
                            }
                        }
                    }
                }
            }
            page->_hasTitlebarHost = false;
        });
    }

    void TabTests::NewTabButtonSharesChromeBackdrop()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            const auto tabRow = winrt::get_self<winrt::TerminalApp::implementation::TabRowControl>(page->_tabRow);
            const auto originalLayout = tabRow->IsVerticalLayout();
            tabRow->IsVerticalLayout(true);
            const auto originalButton = page->_newTabButton;
            const auto originalBrush = page->TitlebarBrush();
            const auto restore = wil::scope_exit([&]() {
                page->_newTabButton = originalButton;
                page->TitlebarBrush(originalBrush);
                tabRow->IsVerticalLayout(originalLayout);
            });
            const auto highContrast = winrt::Windows::UI::ViewManagement::AccessibilitySettings{}.HighContrast();

            for (const auto button : { tabRow->NewTabButton(), tabRow->VerticalNewTabButton() })
            {
                page->_newTabButton = button;
                button.ApplyTemplate();
                const auto root = Media::VisualTreeHelper::GetChild(button, 0).as<Grid>();
                const auto primary = root.FindName(L"PrimaryBackgroundGrid").as<Grid>();
                const auto secondary = root.FindName(L"SecondaryBackgroundGrid").as<Grid>();
                const auto divider = root.FindName(L"DividerBackgroundGrid").as<Grid>();
                VERIFY_ARE_EQUAL(1.0, divider.Width());
                for (const auto color : { winrt::Windows::UI::Colors::Black(), winrt::Windows::UI::Colors::White(), winrt::Windows::UI::Colors::Gray() })
                {
                    // Disabling Acrylic or losing focus must not restore an opaque button fill.
                    for (const auto useAcrylic : { false, true, false })
                    {
                        if (useAcrylic)
                        {
                            Media::AcrylicBrush acrylic;
                            acrylic.BackgroundSource(Media::AcrylicBackgroundSource::HostBackdrop);
                            acrylic.TintColor(color);
                            acrylic.FallbackColor(color);
                            page->TitlebarBrush(acrylic);
                        }
                        else
                        {
                            page->TitlebarBrush(Media::SolidColorBrush{ color });
                        }
                        page->_SetNewTabButtonColor(color, color);
                        const auto transparent = !highContrast;
                        const auto resources = button.Resources();
                        const auto normal = resources.Lookup(winrt::box_value(L"SplitButtonBackground")).as<Media::SolidColorBrush>().Color();
                        const auto hover = resources.Lookup(winrt::box_value(L"SplitButtonBackgroundPointerOver")).as<Media::SolidColorBrush>().Color();
                        const auto pressed = resources.Lookup(winrt::box_value(L"SplitButtonBackgroundPressed")).as<Media::SolidColorBrush>().Color();
                        VERIFY_ARE_EQUAL(transparent ? uint8_t{ 0 } : uint8_t{ 255 }, normal.A);
                        VERIFY_ARE_EQUAL(transparent ? uint8_t{ 13 } : uint8_t{ 255 }, hover.A);
                        VERIFY_ARE_EQUAL(transparent ? uint8_t{ 26 } : uint8_t{ 255 }, pressed.A);
                        if (!transparent)
                        {
                            VERIFY_ARE_EQUAL(color, normal);
                        }
                        VERIFY_ARE_EQUAL(normal, button.Background().as<Media::SolidColorBrush>().Color());
                        const auto verifyState = [&](const wchar_t* state, const auto& primaryColor, const auto& secondaryColor) {
                            VERIFY_IS_TRUE(VisualStateManager::GoToState(button, state, false));
                            VERIFY_ARE_EQUAL(primaryColor, primary.Background().as<Media::SolidColorBrush>().Color());
                            VERIFY_ARE_EQUAL(secondaryColor, secondary.Background().as<Media::SolidColorBrush>().Color());
                            VERIFY_ARE_EQUAL(Visibility::Visible, divider.Visibility());
                            VERIFY_ARE_EQUAL(16.0, divider.Height());
                            VERIFY_ARE_EQUAL(VerticalAlignment::Center, divider.VerticalAlignment());
                            VERIFY_IS_TRUE(divider.Background().as<Media::SolidColorBrush>().Color().A > 0);
                        };
                        verifyState(L"PrimaryPointerOver", hover, normal);
                        verifyState(L"PrimaryPressed", pressed, normal);
                        verifyState(L"SecondaryPointerOver", normal, hover);
                        verifyState(L"SecondaryPressed", normal, pressed);
                        verifyState(L"FlyoutOpen", pressed, pressed);
                        verifyState(L"Normal", normal, normal);
                    }
                }
            }
        });
    }

    void TabTests::VerticalTabStripBindsBackground()
    {
        TestOnUIThread([&]() {
            const auto window = Window::Current();
            const auto previousContent = window.Content();
            const auto cleanup = wil::scope_exit([&]() { window.Content(previousContent); });
            winrt::TerminalApp::TabStrip strip;
            Window::Current().Content(strip);
            Window::Current().Activate();
            strip.UpdateLayout();
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            const auto root = strip.Content().as<Grid>();

            for (const auto source : { Media::AcrylicBackgroundSource::HostBackdrop, Media::AcrylicBackgroundSource::Backdrop })
            {
                Media::AcrylicBrush acrylic;
                acrylic.BackgroundSource(source);
                acrylic.TintColor(winrt::Windows::UI::Colors::Black());
                acrylic.FallbackColor(winrt::Windows::UI::Colors::Black());
                acrylic.TintOpacity(0.5);
                const Media::SolidColorBrush solid{ winrt::Windows::UI::Colors::Black() };
                for (const Media::Brush brush : { Media::Brush{ solid }, Media::Brush{ acrylic }, Media::Brush{ solid } })
                {
                    strip.Background(brush);
                    VERIFY_IS_TRUE(strip.Background() == brush);
                    VERIFY_IS_TRUE(root.Background() == brush);

                    for (const Control button : { stripImpl->SearchTabsButton().as<Control>(),
                                                  stripImpl->FilterTabsButton().as<Control>() })
                    {
                        button.ApplyTemplate();
                        for (const auto state : { L"PointerOver", L"Pressed", L"Normal", L"PointerOver", L"Normal" })
                        {
                            VERIFY_IS_TRUE(VisualStateManager::GoToState(button, state, false));
                            VERIFY_IS_TRUE(strip.Background() == brush);
                            VERIFY_IS_TRUE(root.Background() == brush);
                        }
                    }
                }
            }
        });
    }

    void TabTests::VerticalTabHistorySharesBackdrop()
    {
        winrt::TerminalApp::TabStrip strip{ nullptr };
        UIElement previousContent{ nullptr };
        TestOnUIThread([&]() { previousContent = Window::Current().Content(); });
        const auto cleanup = wil::scope_exit([&]() {
            TestOnUIThread([&]() { Window::Current().Content(previousContent); });
        });
        TestOnUIThread([&]() {
            strip = winrt::TerminalApp::TabStrip{};
            Window::Current().Content(strip);
            Window::Current().Activate();
            strip.UpdateLayout();
        });

        TestOnUIThread([&]() {
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            Media::AcrylicBrush acrylic;
            acrylic.BackgroundSource(Media::AcrylicBackgroundSource::HostBackdrop);
            strip.Background(acrylic);
            VERIFY_IS_TRUE(strip.Background() == acrylic);
            VERIFY_IS_TRUE(strip.Content().as<Grid>().Background() == acrylic);
            VERIFY_IS_NULL(stripImpl->FilterStatusBar().Background());
            VERIFY_ARE_EQUAL(uint8_t{ 0 }, stripImpl->HistoryPanel().Background().as<Media::SolidColorBrush>().Color().A);

            winrt::MUX::Controls::TabViewItem tab;
            strip.TabItems().Append(tab);
            strip.SelectedItem(tab);

            strip.HistoryActive(true);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->ItemsList().Visibility());
            VERIFY_ARE_EQUAL(Visibility::Visible, stripImpl->HistoryPanel().Visibility());
            VERIFY_IS_TRUE(strip.Background() == acrylic);

            strip.HistoryActive(false);
            VERIFY_ARE_EQUAL(Visibility::Visible, stripImpl->ItemsList().Visibility());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->HistoryPanel().Visibility());
            VERIFY_IS_TRUE(strip.Background() == acrylic);
        });
    }

    void TabTests::VerticalTabStripPreservesClosePolicy()
    {
        TestOnUIThread([&]() {
            winrt::TerminalApp::TabStrip strip;
            winrt::MUX::Controls::TabViewItem tab;
            tab.IsClosable(false);
            strip.TabItems().Append(tab);
            VERIFY_IS_FALSE(tab.IsClosable());
        });
    }

    void TabTests::VerticalTabSearchMatchesCommittedTitle()
    {
        const auto rootConnection = winrt::make_self<TestConnection>(
            winrt::guid{ L"{cbb39c84-08be-4d32-bb38-4e2394e7ab62}" },
            winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
        auto page = _commonSetup(*rootConnection, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);

            tab->Title(L"PowerShell Ångström");
            page->_tabSearchActive = true;
            page->_tabSearchQuery = L"shell";
            VERIFY_IS_TRUE(page->_MatchesTabSearch(*tab));
            page->_tabSearchQuery = L"ångSTRÖM";
            VERIFY_IS_TRUE(page->_MatchesTabSearch(*tab));

            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            const auto display = stripImpl->DisplayItemForTab(tab->TabViewItem());
            VERIFY_IS_NOT_NULL(display);
            VERIFY_IS_FALSE(display.IsGroup());
            page->UpdateLayout();
            const auto container = page->_tabStrip.ContainerFromIndex(0).as<ListViewItem>();

            page->_tabStrip.SearchActive(true);
            const auto search = [&](const winrt::hstring& query) {
                stripImpl->SearchTextBox().Text(query);
                page->UpdateLayout();
                VERIFY_IS_TRUE(page->_tabSearchActive);
                VERIFY_ARE_EQUAL(query, page->_tabSearchQuery);
            };
            const auto applyRichTabUpdate = [&](const std::shared_ptr<Pane>& pane,
                                                const std::wstring_view text,
                                                const uint64_t updateSequence) {
                const auto control = pane->GetTerminalControl();
                VERIFY_IS_NOT_NULL(control);
                page->_AttachOrUpdateRichTabControl(control);
                const auto key = reinterpret_cast<uintptr_t>(winrt::get_abi(control));
                const auto attachment = page->_richTabAttachments.find(key);
                VERIFY_IS_TRUE(attachment != page->_richTabAttachments.end());
                if (attachment == page->_richTabAttachments.end())
                {
                    return;
                }

                ::Microsoft::Terminal::RichTab::Provider::Presentation presentation;
                presentation.text = text;
                ::Microsoft::Terminal::RichTab::Provider::BrokerUpdate update;
                update.sessionId = attachment->second.sessionId;
                update.sessionIncarnation = std::numeric_limits<uint64_t>::max();
                update.updateSequence = updateSequence;
                update.presentation = std::move(presentation);
                page->_ApplyRichTabUpdate(key, attachment->second.reservation, update);
                page->UpdateLayout();
            };

            search(L"änderUNGEN");
            VERIFY_ARE_EQUAL(Visibility::Collapsed, container.Visibility());
            applyRichTabUpdate(tab->GetRootPane(), L"main\n2 Änderungen", 1);
            VERIFY_ARE_EQUAL(Visibility::Visible, container.Visibility());
            VERIFY_IS_TRUE(display.Header().as<winrt::TerminalApp::TabHeaderControl>().IsMetadataVisible());

            const auto unicodePaneConnection = winrt::make_self<TestConnection>(
                winrt::guid{ L"{ed7ea490-998e-4aac-aecd-a74051a9faee}" },
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            const auto unicodePane = page->_MakePane(nullptr, page->_GetFocusedTab(), *unicodePaneConnection);
            VERIFY_IS_NOT_NULL(unicodePane);
            VERIFY_IS_TRUE(page->_SplitPane(tab, SplitDirection::Right, 0.5f, unicodePane, false));
            page->UpdateLayout();
            VERIFY_IS_TRUE(display.IsGroup());
            VERIFY_IS_TRUE(display.IsExpanded());
            VERIFY_IS_FALSE(display.Header().as<winrt::TerminalApp::TabHeaderControl>().IsMetadataVisible());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, container.Visibility());

            search(L"münchen");
            VERIFY_ARE_EQUAL(Visibility::Collapsed, container.Visibility());
            const std::u16string unicodePaneTitle{ u"\x1b]0;MÜNCHEN \U0001F680\x07" };
            unicodePaneConnection->TerminalOutput.raise(
                winrt::array_view<const char16_t>{ unicodePaneTitle.data(), unicodePaneTitle.data() + unicodePaneTitle.size() });
            VERIFY_ARE_EQUAL(winrt::hstring{ L"PowerShell Ångström" }, tab->Title());
            page->UpdateLayout();
            VERIFY_ARE_EQUAL(Visibility::Visible, container.Visibility());
            const auto projectedUnicodePane = display.PaneItems().GetAt(1);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"MÜNCHEN \U0001F680" }, projectedUnicodePane.Title());
            const auto toggle = container.ContentTemplateRoot().as<FrameworkElement>().FindName(L"TabGroupToggleButton").as<Button>();

            search(L"\U0001F680");
            VERIFY_ARE_EQUAL(Visibility::Visible, container.Visibility());

            search(L"visible pane metadata");
            VERIFY_ARE_EQUAL(Visibility::Collapsed, container.Visibility());
            applyRichTabUpdate(unicodePane, L"feature\nvisible pane metadata", 1);
            VERIFY_ARE_EQUAL(Visibility::Visible, container.Visibility());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"feature\nvisible pane metadata" }, projectedUnicodePane.MetadataText());
            VERIFY_IS_TRUE(unicodePane->Id().has_value());
            VERIFY_IS_TRUE(tab->FocusPane(unicodePane->Id().value()));
            page->UpdateLayout();
            VERIFY_IS_FALSE(display.Header().as<winrt::TerminalApp::TabHeaderControl>().IsMetadataVisible());

            uint32_t selectionChanges = 0;
            const auto selectionToken = page->_tabStrip.SelectionChanged([&](auto&&, auto&&) {
                ++selectionChanges;
            });
            const auto revokeSelection = wil::scope_exit([&]() {
                page->_tabStrip.SelectionChanged(selectionToken);
            });

            stripImpl->OnGroupToggleClick(toggle, RoutedEventArgs{});
            VERIFY_IS_FALSE(display.IsExpanded());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, display.ChildrenVisibility());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, container.Visibility());
            VERIFY_IS_FALSE(page->_MatchesTabSearch(*tab));
            page->_tabSearchQuery = L"münchen";
            VERIFY_IS_FALSE(page->_MatchesTabSearch(*tab));
            VERIFY_IS_TRUE(page->_tabStrip.SelectedItem() == tab->TabViewItem());
            VERIFY_ARE_EQUAL(0, page->_tabStrip.SelectedIndex());
            VERIFY_ARE_EQUAL(0u, selectionChanges);

            page->_tabSearchQuery = L"visible pane metadata";
            stripImpl->OnGroupToggleClick(toggle, RoutedEventArgs{});
            VERIFY_IS_TRUE(display.IsExpanded());
            VERIFY_ARE_EQUAL(Visibility::Visible, display.ChildrenVisibility());
            VERIFY_ARE_EQUAL(Visibility::Visible, container.Visibility());
            VERIFY_IS_TRUE(page->_MatchesTabSearch(*tab));
            page->_tabSearchQuery = L"münchen";
            VERIFY_IS_TRUE(page->_MatchesTabSearch(*tab));
            VERIFY_IS_TRUE(page->_tabStrip.SelectedItem() == tab->TabViewItem());
            VERIFY_ARE_EQUAL(0, page->_tabStrip.SelectedIndex());
            VERIFY_ARE_EQUAL(0u, selectionChanges);

            const auto agentConnection = winrt::make_self<TestConnection>(
                winrt::guid{ L"{6a480c9d-7cef-4e90-a18a-68c66c7e0888}" },
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            const auto agentPane = page->_WrapInAgentPaneContent(
                page->_MakePane(nullptr, page->_GetFocusedTab(), *agentConnection));
            VERIFY_IS_NOT_NULL(agentPane);
            agentPane->IsAgentPane(true);
            VERIFY_IS_TRUE(page->_SplitPane(tab, SplitDirection::Right, 0.5f, agentPane, false));

            search(L"Hidden Agent Search Title");
            VERIFY_ARE_EQUAL(Visibility::Collapsed, container.Visibility());
            const std::u16string agentTitle{ u"\x1b]0;Hidden Agent Search Title\x07" };
            agentConnection->TerminalOutput.raise(
                winrt::array_view<const char16_t>{ agentTitle.data(), agentTitle.data() + agentTitle.size() });
            VERIFY_ARE_EQUAL(winrt::hstring{ L"PowerShell Ångström" }, tab->Title());
            page->UpdateLayout();
            VERIFY_ARE_EQUAL(Visibility::Collapsed, container.Visibility());
            VERIFY_IS_FALSE(page->_MatchesTabSearch(*tab));

            search(L"POWER");
            VERIFY_ARE_EQUAL(Visibility::Visible, container.Visibility());
        });
    }

    void TabTests::VerticalTabSearchTracksActivePaneMetadata()
    {
        const auto rootConnection = winrt::make_self<TestConnection>(
            winrt::guid{ L"{b7c8ba10-99df-41e5-a637-087f325cd186}" },
            winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
        auto page = _commonSetup(*rootConnection, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);
            const auto rootPane = tab->GetActivePane();
            VERIFY_IS_NOT_NULL(rootPane);
            VERIFY_IS_TRUE(rootPane->Id().has_value());

            const auto agentConnection = winrt::make_self<TestConnection>(
                winrt::guid{ L"{a4ac8309-d880-40dc-8822-c22469369419}" },
                winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
            const auto agentPane = page->_WrapInAgentPaneContent(
                page->_MakePane(nullptr, page->_GetFocusedTab(), *agentConnection));
            VERIFY_IS_NOT_NULL(agentPane);
            agentPane->IsAgentPane(true);
            VERIFY_IS_TRUE(page->_SplitPane(tab, SplitDirection::Right, 0.5f, agentPane));
            VERIFY_IS_TRUE(agentPane->Id().has_value());
            VERIFY_IS_TRUE(tab->GetActivePane() == agentPane);

            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            const auto display = stripImpl->DisplayItemForTab(tab->TabViewItem());
            VERIFY_IS_NOT_NULL(display);
            VERIFY_IS_FALSE(display.IsGroup());
            page->UpdateLayout();
            const auto container = page->_tabStrip.ContainerFromIndex(0).as<ListViewItem>();

            const auto applyRichTabUpdate = [&](const std::shared_ptr<Pane>& pane,
                                                const std::wstring_view text,
                                                const uint64_t updateSequence) {
                const auto control = pane->GetTerminalControl();
                VERIFY_IS_NOT_NULL(control);
                page->_AttachOrUpdateRichTabControl(control);
                const auto key = reinterpret_cast<uintptr_t>(winrt::get_abi(control));
                const auto attachment = page->_richTabAttachments.find(key);
                VERIFY_IS_TRUE(attachment != page->_richTabAttachments.end());
                if (attachment == page->_richTabAttachments.end())
                {
                    return;
                }

                ::Microsoft::Terminal::RichTab::Provider::Presentation presentation;
                presentation.text = text;
                ::Microsoft::Terminal::RichTab::Provider::BrokerUpdate update;
                update.sessionId = attachment->second.sessionId;
                update.sessionIncarnation = std::numeric_limits<uint64_t>::max();
                update.updateSequence = updateSequence;
                update.presentation = std::move(presentation);
                page->_ApplyRichTabUpdate(key, attachment->second.reservation, update);
                page->UpdateLayout();
            };
            const auto search = [&](const winrt::hstring& query) {
                page->_tabStrip.SearchActive(true);
                stripImpl->SearchTextBox().Text(query);
                page->UpdateLayout();
                VERIFY_ARE_EQUAL(query, page->_tabSearchQuery);
            };

            search(L"Committed Search Title");
            VERIFY_ARE_EQUAL(Visibility::Collapsed, container.Visibility());
            tab->SetTabText(L"Committed Search Title");
            page->UpdateLayout();
            VERIFY_ARE_EQUAL(Visibility::Visible, container.Visibility());
            VERIFY_ARE_EQUAL(
                winrt::hstring{ L"Committed Search Title" },
                display.Header().as<winrt::TerminalApp::TabHeaderControl>().SearchText());

            applyRichTabUpdate(agentPane, L"new-agent-metadata", 1);
            VERIFY_IS_TRUE(tab->FocusPane(rootPane->Id().value()));
            applyRichTabUpdate(rootPane, L"old-shell-metadata", 1);
            VERIFY_IS_FALSE(display.IsGroup());

            search(L"old-shell-metadata");
            VERIFY_ARE_EQUAL(Visibility::Visible, container.Visibility());
            VERIFY_IS_TRUE(tab->FocusPane(agentPane->Id().value()));
            page->UpdateLayout();
            VERIFY_ARE_EQUAL(Visibility::Collapsed, container.Visibility());

            search(L"new-agent-metadata");
            VERIFY_ARE_EQUAL(Visibility::Visible, container.Visibility());
            VERIFY_IS_TRUE(tab->FocusPane(rootPane->Id().value()));
            page->UpdateLayout();
            VERIFY_ARE_EQUAL(Visibility::Collapsed, container.Visibility());

            search(L"old-shell-metadata");
            VERIFY_ARE_EQUAL(Visibility::Visible, container.Visibility());
            const auto controlLessPane = std::make_shared<Pane>(page->_makeSettingsContent());
            controlLessPane->IsAgentPane(true);
            VERIFY_IS_TRUE(page->_SplitPane(tab, SplitDirection::Right, 0.5f, controlLessPane));
            VERIFY_IS_NULL(controlLessPane->GetTerminalControl());
            VERIFY_IS_TRUE(tab->GetActivePane() == controlLessPane);
            VERIFY_IS_FALSE(display.IsGroup());
            page->UpdateLayout();
            VERIFY_ARE_EQUAL(Visibility::Collapsed, container.Visibility());
        });
    }

    void TabTests::VerticalTabSearchUiState()
    {
        TestOnUIThread([&]() {
            winrt::TerminalApp::TabStrip strip;
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);

            strip.SearchActive(true);
            strip.SearchQuery(L"power");
            VERIFY_ARE_EQUAL(Visibility::Visible, stripImpl->SearchPanel().Visibility());
            VERIFY_ARE_EQUAL(40.0, stripImpl->SearchPanel().Height());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"power" }, stripImpl->SearchTextBox().Text());
            VERIFY_IS_TRUE(stripImpl->FilterTabsButton().IsTabStop());
            VERIFY_IS_TRUE(stripImpl->TabHistoryButton().IsTabStop());

            strip.IsRailCollapsed(true);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->SearchPanel().Visibility());
            VERIFY_IS_TRUE(stripImpl->SearchTabsButton().IsEnabled());
            VERIFY_IS_TRUE(stripImpl->SearchTabsButton().IsHitTestVisible());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->FilterTabsButton().Visibility());
            VERIFY_IS_FALSE(stripImpl->FilterTabsButton().IsEnabled());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->TabHistoryButton().Visibility());
            VERIFY_IS_FALSE(stripImpl->TabHistoryButton().IsEnabled());

            strip.IsRailCollapsed(false);
            VERIFY_ARE_EQUAL(Visibility::Visible, stripImpl->SearchPanel().Visibility());
            VERIFY_IS_TRUE(stripImpl->SearchTabsButton().IsEnabled());
            VERIFY_ARE_EQUAL(Visibility::Visible, stripImpl->FilterTabsButton().Visibility());
            VERIFY_IS_TRUE(stripImpl->FilterTabsButton().IsEnabled());
            VERIFY_ARE_EQUAL(Visibility::Visible, stripImpl->TabHistoryButton().Visibility());
            VERIFY_IS_TRUE(stripImpl->TabHistoryButton().IsEnabled());
            stripImpl->ProjectionControlsEnabled(false);
            VERIFY_IS_FALSE(stripImpl->SearchTabsButton().IsEnabled());
            VERIFY_IS_FALSE(stripImpl->FilterTabsButton().IsEnabled());
            VERIFY_IS_FALSE(stripImpl->TabHistoryButton().IsEnabled());
            stripImpl->ProjectionControlsEnabled(true);
            VERIFY_IS_TRUE(stripImpl->SearchTabsButton().IsEnabled());
            VERIFY_IS_TRUE(stripImpl->FilterTabsButton().IsEnabled());
            VERIFY_IS_TRUE(stripImpl->TabHistoryButton().IsEnabled());
            strip.SearchQuery(L"");
            strip.SearchQuery(L"");
            strip.SearchActive(false);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->SearchPanel().Visibility());
            VERIFY_ARE_EQUAL(0.0, stripImpl->SearchPanel().Height());
        });
    }

    void TabTests::VerticalTabHistoryButtonOpensView()
    {
        HistoryTestView view;
        TestOnUIThread([&]() {
            const auto strip = view.strip;
            strip.HistoryActive(false);
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            const auto title = stripImpl->HistoryHeader().Text();
            VERIFY_IS_FALSE(title.empty());
            VERIFY_ARE_EQUAL(title, Automation::AutomationProperties::GetName(stripImpl->TabHistoryButton()));
            VERIFY_ARE_EQUAL(title, winrt::unbox_value<winrt::hstring>(ToolTipService::GetToolTip(stripImpl->TabHistoryButton())));
            VERIFY_IS_FALSE(Automation::AutomationProperties::GetName(stripImpl->HistoryCloseButton()).empty());
            bool historyRequested = false;
            bool historyClosed = false;
            const auto requested = strip.HistoryRequested([&](auto&&, auto&&) {
                historyRequested = true;
            });
            const auto closed = strip.HistoryClosed([&](auto&&, auto&&) {
                historyClosed = true;
                strip.HistoryActive(false);
            });
            const auto revoke = wil::scope_exit([&]() {
                strip.HistoryRequested(requested);
                strip.HistoryClosed(closed);
            });

            strip.IsRailCollapsed(true);
            stripImpl->OnHistoryClick(nullptr, {});
            VERIFY_IS_FALSE(historyRequested);
            VERIFY_IS_FALSE(strip.HistoryActive());
            strip.IsRailCollapsed(false);
            stripImpl->ProjectionControlsEnabled(false);
            stripImpl->OnHistoryClick(nullptr, {});
            VERIFY_IS_FALSE(historyRequested);
            VERIFY_IS_FALSE(strip.HistoryActive());
            stripImpl->ProjectionControlsEnabled(true);

            strip.SearchActive(true);
            strip.SearchQuery(L"power");
            stripImpl->OnHistoryClick(nullptr, {});

            VERIFY_IS_TRUE(historyRequested);
            VERIFY_IS_TRUE(strip.HistoryActive());
            VERIFY_ARE_EQUAL(winrt::TerminalApp::TabStripFilterMode::AllTabs, strip.FilterMode());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->TabsToolbar().Visibility());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->SearchPanel().Visibility());
            VERIFY_ARE_EQUAL(2, Grid::GetRow(stripImpl->HistoryPanel()));
            VERIFY_ARE_EQUAL(4, Grid::GetRowSpan(stripImpl->HistoryPanel()));
            VERIFY_ARE_EQUAL(1, Grid::GetRow(stripImpl->HistorySearchTextBox()));
            VERIFY_ARE_EQUAL(2, Grid::GetRow(Media::VisualTreeHelper::GetParent(stripImpl->HistoryList()).as<FrameworkElement>()));
            VERIFY_IS_TRUE(stripImpl->HistoryCloseButton().IsTabStop());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->ItemsList().Visibility());
            VERIFY_ARE_EQUAL(Visibility::Visible, stripImpl->HistoryPanel().Visibility());

            strip.TabsVisible(false);
            strip.TabsVisible(true);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->ItemsList().Visibility());
            VERIFY_ARE_EQUAL(Visibility::Visible, stripImpl->HistoryPanel().Visibility());

            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->TabsToolbar().Visibility());
            stripImpl->HistorySearchTextBox().Text(L"agent query");
            stripImpl->OnHistoryCloseClick(nullptr, {});
            VERIFY_IS_TRUE(historyClosed);
            VERIFY_IS_FALSE(strip.HistoryActive());
            VERIFY_ARE_EQUAL(Visibility::Visible, stripImpl->TabsToolbar().Visibility());
            VERIFY_ARE_EQUAL(Visibility::Visible, stripImpl->SearchPanel().Visibility());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"power" }, strip.SearchQuery());
            VERIFY_IS_TRUE(stripImpl->HistorySearchTextBox().Text().empty());
            VERIFY_ARE_EQUAL(Visibility::Visible, stripImpl->ItemsList().Visibility());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->HistoryPanel().Visibility());
        });
    }

    void TabTests::VerticalTabHistoryCloseStopsRefresh()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            page->_tabStrip.HistoryActive(true);
            page->_StartSidebarHistoryRefreshTimer();
            VERIFY_IS_TRUE(page->_historyRefreshTimer.IsEnabled());

            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            stripImpl->OnHistoryCloseClick(nullptr, {});

            VERIFY_IS_FALSE(page->_historyRefreshTimer.IsEnabled());
            VERIFY_IS_FALSE(page->_tabStrip.HistoryActive());
            VERIFY_ARE_EQUAL(Visibility::Visible, stripImpl->ItemsList().Visibility());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->HistoryPanel().Visibility());
        });
    }

    void TabTests::VerticalTabFilterContainsOnlyMetadata()
    {
        TestOnUIThread([&]() {
            winrt::TerminalApp::TabStrip strip;
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            const auto items = stripImpl->FilterTabsButton().Flyout().as<MenuFlyout>().Items();
            VERIFY_ARE_EQUAL(6u, items.Size());
            VERIFY_IS_TRUE(items.GetAt(0) == stripImpl->RichTabMetadataSectionItem());
            VERIFY_IS_TRUE(items.GetAt(1) == stripImpl->RichTabAgentStatusVisibleItem());
            VERIFY_IS_TRUE(items.GetAt(2) == stripImpl->RichTabWorkingDirectoryVisibleItem());
            VERIFY_IS_TRUE(items.GetAt(3) == stripImpl->RichTabRepositoryVisibleItem());
            VERIFY_IS_TRUE(items.GetAt(4) == stripImpl->RichTabBranchVisibleItem());
            VERIFY_IS_TRUE(items.GetAt(5) == stripImpl->RichTabChangesVisibleItem());

            bool fieldsChanged = false;
            bool historyRequested = false;
            strip.VisibleFieldsChanged([&](auto&&, auto&&) { fieldsChanged = true; });
            strip.HistoryRequested([&](auto&&, auto&&) { historyRequested = true; });
            stripImpl->RichTabWorkingDirectoryVisibleItem().IsChecked(false);
            stripImpl->OnRichTabWorkingDirectoryVisibleClick(nullptr, {});
            stripImpl->RichTabRepositoryVisibleItem().IsChecked(true);
            stripImpl->OnRichTabRepositoryVisibleClick(nullptr, {});
            VERIFY_IS_TRUE(fieldsChanged);
            VERIFY_IS_TRUE(strip.RichTabRepositoryVisible());
            VERIFY_IS_FALSE(strip.RichTabWorkingDirectoryVisible());
            VERIFY_IS_FALSE(historyRequested);
            VERIFY_IS_FALSE(strip.HistoryActive());
            VERIFY_ARE_EQUAL(winrt::TerminalApp::TabStripFilterMode::AllTabs, strip.FilterMode());

            stripImpl->RichTabMetadataControlsVisible(false);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->FilterTabsButton().Visibility());
            VERIFY_IS_TRUE(stripImpl->TabHistoryButton().IsEnabled());
            strip.IsRailCollapsed(true);
            strip.IsRailCollapsed(false);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->FilterTabsButton().Visibility());
            strip.IsRailCollapsed(true);
            stripImpl->RichTabMetadataControlsVisible(true);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->FilterTabsButton().Visibility());
            strip.IsRailCollapsed(false);
            VERIFY_ARE_EQUAL(Visibility::Visible, stripImpl->FilterTabsButton().Visibility());
        });
    }

    void TabTests::LiteralSearchHighlighting()
    {
        TestOnUIThread([&]() {
            winrt::TerminalApp::HighlightedTextControl control;
            control.Text(L"PowerShell");
            control.SearchText(L"shell");
            control.ApplyTemplate();

            const auto textBlock = Media::VisualTreeHelper::GetChild(control, 0).as<TextBlock>();
            VERIFY_ARE_EQUAL(2u, textBlock.Inlines().Size());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Power" }, textBlock.Inlines().GetAt(0).as<Documents::Run>().Text());
            const auto highlighted = textBlock.Inlines().GetAt(1).as<Documents::Run>();
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Shell" }, highlighted.Text());
            VERIFY_ARE_EQUAL(FontWeights::Bold().Weight, highlighted.FontWeight().Weight);

            control.Text(L"Launch \U0001F680 now");
            control.SearchText(L"\U0001F680");
            VERIFY_ARE_EQUAL(3u, textBlock.Inlines().Size());
            const auto rocket = textBlock.Inlines().GetAt(1).as<Documents::Run>();
            VERIFY_ARE_EQUAL(winrt::hstring{ L"\U0001F680" }, rocket.Text());
            VERIFY_ARE_EQUAL(2u, rocket.Text().size());
            VERIFY_ARE_EQUAL(FontWeights::Bold().Weight, rocket.FontWeight().Weight);

            control.SearchText(L"");
            VERIFY_ARE_EQUAL(1u, textBlock.Inlines().Size());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Launch \U0001F680 now" }, textBlock.Inlines().GetAt(0).as<Documents::Run>().Text());
        });
    }

    void TabTests::BottomBarSessionsButtonFollowsLayout()
    {
        for (const auto vertical : { false, true })
        {
            auto page = _commonSetup(nullptr, nullptr, std::nullopt, vertical);
            TestOnUIThread([&]() {
                const auto button = page->SessionToggleButton();
                VERIFY_IS_NOT_NULL(button);
                VERIFY_ARE_EQUAL(vertical ? Visibility::Collapsed : Visibility::Visible, button.Visibility());
                VERIFY_ARE_EQUAL(3, Grid::GetColumn(button));
                VERIFY_ARE_EQUAL(4u, page->BottomBar().ColumnDefinitions().Size());
                Command sessionsCommand;
                sessionsCommand.ActionAndArgs(ActionAndArgs{ ShortcutAction::OpenAgentSessions, nullptr });
                const auto label = sessionsCommand.Name();
                VERIFY_IS_FALSE(label.empty());
                VERIFY_ARE_EQUAL(label, page->SessionToggleLabel().Text());
                VERIFY_ARE_EQUAL(label, Automation::AutomationProperties::GetName(button));
                VERIFY_IS_NOT_NULL(page->AgentToggleButton());
                if (vertical)
                {
                    page->_OnVerticalRailCollapseRequested(nullptr, nullptr);
                    VERIFY_IS_TRUE(page->_isVerticalRailCollapsed);
                    VERIFY_ARE_EQUAL(Visibility::Collapsed, button.Visibility());
                    page->_SetVerticalRailVisibility(false);
                    VERIFY_ARE_EQUAL(Visibility::Collapsed, button.Visibility());
                }
            });
        }
    }

    void TabTests::BottomBarSessionsButtonDispatchesExistingAction()
    {
        auto page = _commonSetup();
        TestOnUIThread([&]() {
            auto dispatch = winrt::make_self<winrt::TerminalApp::implementation::ShortcutActionDispatch>();
            uint32_t invocations = 0;
            dispatch->OpenAgentSessions([&](auto&&, const ActionEventArgs& args) {
                ++invocations;
                args.Handled(true);
            });
            const auto previousDispatch = std::exchange(page->_actionDispatch, dispatch);
            const auto restoreDispatch = wil::scope_exit([&]() {
                page->_actionDispatch = previousDispatch;
            });

            page->_SessionToggleButtonOnClick(nullptr, {});

            VERIFY_ARE_EQUAL(1u, invocations);
        });
    }

    void TabTests::BottomBarSessionsButtonTracksVisibleView()
    {
        auto page = _commonSetup();
        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            auto agentPane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            agentPane->IsAgentPane(true);
            VERIFY_IS_TRUE(page->_SplitPane(tab, SplitDirection::Right, 0.5f, agentPane));
            const auto content = tab->FindAgentPaneContent();
            VERIFY_IS_NOT_NULL(content);
            const auto alpha = [](const Button& button) {
                return button.Background().as<Media::SolidColorBrush>().Color().A;
            };

            content.SetSessionsView(true);
            page->_UpdateBottomBarState();
            VERIFY_ARE_EQUAL(uint8_t{ 30 }, alpha(page->SessionToggleButton()));
            VERIFY_ARE_EQUAL(uint8_t{ 0 }, alpha(page->AgentToggleButton()));

            content.SetSessionsView(false);
            page->_UpdateBottomBarState();
            VERIFY_ARE_EQUAL(uint8_t{ 0 }, alpha(page->SessionToggleButton()));
            VERIFY_ARE_EQUAL(uint8_t{ 30 }, alpha(page->AgentToggleButton()));

            content.SetSessionsView(true);
            tab->StashAgentPane();
            page->_UpdateBottomBarState();
            VERIFY_ARE_EQUAL(uint8_t{ 0 }, alpha(page->SessionToggleButton()));
            VERIFY_ARE_EQUAL(uint8_t{ 0 }, alpha(page->AgentToggleButton()));
            VERIFY_IS_TRUE(tab->FindAgentPaneContent() == content);
        });
    }

    void TabTests::VerticalTabHistoryStatusText()
    {
        TestOnUIThread([&]() {
            const auto resources = winrt::Windows::ApplicationModel::Resources::Core::ResourceManager::Current()
                                       .MainResourceMap()
                                       .GetSubtree(L"TerminalApp/Resources");
            const std::pair<std::string_view, winrt::hstring> cases[]{
                { "Idle", L"VerticalTabsHistoryStatusIdle" },
                { "Working", L"VerticalTabsHistoryStatusWorking" },
                { "Attention", L"VerticalTabsHistoryStatusAttention" },
                { "Error", L"VerticalTabsHistoryStatusError" },
                { "Ended", L"VerticalTabsHistoryStatusHistorical" },
                { "Historical", L"VerticalTabsHistoryStatusHistorical" },
                { "", L"VerticalTabsHistoryStatusUnknown" },
                { "FutureStatus", L"VerticalTabsHistoryStatusUnknown" },
            };
            for (const auto& [status, resource] : cases)
            {
                const auto text = winrt::TerminalApp::implementation::TerminalPage::_SidebarHistoryStatusText(status);
                VERIFY_IS_FALSE(text.empty());
                VERIFY_ARE_EQUAL(resources.GetValue(resource).ValueAsString(), text);
            }
        });
    }

    void TabTests::SessionRegistryStatusDeltaUpdatesCaches()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            page->_tabStrip.RichTabAgentStatusVisible(false);
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            auto item = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
            item.SessionId(L"session-a");
            item.PaneSessionId(L"00000000-0000-0000-0000-000000000001");
            item.Title(L"Waiting session");
            item.AgentId(L"copilot");
            item.ProviderDisplayName(L"Copilot");
            item.AgentSource(L"host");
            item.Status(L"Idle");
            const winrt::hstring metadata{ L"Copilot \u00b7 just now \u00b7 " };
            item.Subtitle(metadata);
            item.StatusText(winrt::TerminalApp::implementation::TerminalPage::_SidebarHistoryStatusText("Idle"));
            item.IsLive(true);
            stripImpl->CommitHistorySnapshot({ item });
            const auto idleStyle = item.StatusTextStyle();

            page->_richTabAgentStatusRequestGeneration = 41;
            page->_richTabAgentStatusSnapshotLoaded = false;
            page->_richTabAgentStatusRefreshInFlight = true;
            page->_richTabAgentStatusRefreshPending = false;
            VERIFY_IS_TRUE(page->_ApplyAgentSessionStatusDelta(
                "session-a",
                "00000000-0000-0000-0000-000000000001",
                "Attention"));
            VERIFY_ARE_EQUAL(uint64_t{ 42 }, page->_richTabAgentStatusRequestGeneration);
            VERIFY_IS_FALSE(page->_richTabAgentStatusSnapshotLoaded);
            VERIFY_IS_TRUE(page->_richTabAgentStatusRefreshPending);
            VERIFY_ARE_EQUAL(
                std::string{ "Attention" },
                page->_richTabAgentStatusBySessionId.at("session-a"));
            VERIFY_ARE_EQUAL(
                std::string{ "Attention" },
                page->_richTabAgentStatusByPaneId.at(
                    winrt::guid{ L"00000000-0000-0000-0000-000000000001" }));
            VERIFY_ARE_EQUAL(1u, page->_tabStrip.HistoryItems().Size());
            const auto updated = page->_tabStrip.HistoryItems().GetAt(0);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Attention" }, updated.Status());
            VERIFY_ARE_EQUAL(metadata, updated.Subtitle());
            VERIFY_ARE_EQUAL(winrt::TerminalApp::implementation::TerminalPage::_SidebarHistoryStatusText("Attention"), updated.StatusText());
            VERIFY_IS_TRUE(updated.StatusTextStyle() != idleStyle);
            VERIFY_IS_TRUE(
                updated.StatusTextStyle() ==
                page->_tabStrip.Resources().Lookup(winrt::box_value(L"HistoryAttentionTextStyle")).as<Style>());
            VERIFY_IS_TRUE(updated.IsLive());
            VERIFY_IS_FALSE(updated.IsHistorical());

            VERIFY_IS_FALSE(page->_ApplyAgentSessionStatusDelta("session-a", "", "FutureStatus"));
        });
    }

    void TabTests::VerticalTabHistoryRelativeAge()
    {
        TestOnUIThread([&]() {
            const auto resources = winrt::Windows::ApplicationModel::Resources::Core::ResourceManager::Current()
                                       .MainResourceMap()
                                       .GetSubtree(L"TerminalApp/Resources");
            constexpr uint64_t nowMs = 100ULL * 86400 * 1000;
            struct AgeCase
            {
                uint64_t elapsedMs;
                winrt::hstring resource;
                uint64_t count;
            };
            const AgeCase cases[]{
                { 0, L"VerticalTabsHistoryAgeJustNow", 0 },
                { 59'999, L"VerticalTabsHistoryAgeJustNow", 0 },
                { 60'000, L"VerticalTabsHistoryAgeMinute", 0 },
                { 119'999, L"VerticalTabsHistoryAgeMinute", 0 },
                { 120'000, L"VerticalTabsHistoryAgeMinutes", 2 },
                { 3'599'999, L"VerticalTabsHistoryAgeMinutes", 59 },
                { 3'600'000, L"VerticalTabsHistoryAgeHour", 0 },
                { 7'199'999, L"VerticalTabsHistoryAgeHour", 0 },
                { 7'200'000, L"VerticalTabsHistoryAgeHours", 2 },
                { 86'399'999, L"VerticalTabsHistoryAgeHours", 23 },
                { 86'400'000, L"VerticalTabsHistoryAgeDay", 0 },
                { 172'799'999, L"VerticalTabsHistoryAgeDay", 0 },
                { 172'800'000, L"VerticalTabsHistoryAgeDays", 2 },
                { 7ULL * 86'400'000 - 1, L"VerticalTabsHistoryAgeDays", 6 },
                { nowMs, L"VerticalTabsHistoryAgeUnknown", 0 },
            };
            for (const auto& test : cases)
            {
                auto expected = resources.GetValue(test.resource).ValueAsString();
                if (test.count)
                {
                    expected = fmt::format(fmt::runtime(std::wstring_view{ expected }), test.count);
                }
                VERIFY_ARE_EQUAL(expected, winrt::TerminalApp::implementation::TerminalPage::_SidebarHistoryAgeText(nowMs - test.elapsedMs, nowMs));
            }
            const auto justNow = resources.GetValue(L"VerticalTabsHistoryAgeJustNow").ValueAsString();
            VERIFY_ARE_EQUAL(justNow, winrt::TerminalApp::implementation::TerminalPage::_SidebarHistoryAgeText(nowMs + 1, nowMs));
            VERIFY_ARE_EQUAL(justNow, winrt::TerminalApp::implementation::TerminalPage::_SidebarHistoryAgeText(UINT64_MAX, nowMs));
            VERIFY_ARE_EQUAL(resources.GetValue(L"VerticalTabsHistoryAgeUnknown").ValueAsString(),
                             winrt::TerminalApp::implementation::TerminalPage::_SidebarHistoryAgeText(std::nullopt, nowMs));

            const auto context = winrt::Windows::ApplicationModel::Resources::Core::ResourceContext::GetForViewIndependentUse();
            const auto language = context.QualifierValues().TryLookup(L"language");
            const auto locale = language ? *language : winrt::hstring{};
            const auto expectedDate = [&](WORD year, WORD month, WORD day) {
                SYSTEMTIME time{};
                time.wYear = year;
                time.wMonth = month;
                time.wDay = day;
                wchar_t buffer[256]{};
                VERIFY_IS_TRUE(GetDateFormatEx(locale.empty() ? LOCALE_NAME_USER_DEFAULT : locale.c_str(),
                                               DATE_LONGDATE,
                                               &time,
                                               nullptr,
                                               buffer,
                                               ARRAYSIZE(buffer),
                                               nullptr) > 0);
                return winrt::hstring{ buffer };
            };
            constexpr uint64_t calendarNowMs = 1'790'596'800'000; // 2026-09-28 12:00 UTC
            constexpr uint64_t weekMs = 7ULL * 86'400'000;
            const auto oldDate = expectedDate(2026, 9, 21);
            VERIFY_ARE_EQUAL(oldDate, winrt::TerminalApp::implementation::TerminalPage::_SidebarHistoryAgeText(calendarNowMs - weekMs, calendarNowMs));
            VERIFY_ARE_EQUAL(oldDate, winrt::TerminalApp::implementation::TerminalPage::_SidebarHistoryAgeText(calendarNowMs - weekMs - 1, calendarNowMs));
            VERIFY_ARE_EQUAL(expectedDate(1970, 1, 1),
                             winrt::TerminalApp::implementation::TerminalPage::_SidebarHistoryAgeText(1, calendarNowMs));
            const auto midnightMs = calendarNowMs - 12ULL * 3'600'000 - weekMs;
            VERIFY_ARE_EQUAL(oldDate, winrt::TerminalApp::implementation::TerminalPage::_SidebarHistoryAgeText(midnightMs, calendarNowMs));
            VERIFY_ARE_EQUAL(expectedDate(2026, 9, 20),
                             winrt::TerminalApp::implementation::TerminalPage::_SidebarHistoryAgeText(midnightMs - 1, calendarNowMs));
            VERIFY_ARE_EQUAL(resources.GetValue(L"VerticalTabsHistoryAgeUnknown").ValueAsString(),
                             winrt::TerminalApp::implementation::TerminalPage::_SidebarHistoryAgeText(UINT64_MAX - weekMs, UINT64_MAX));
        });
    }

    void TabTests::VerticalTabHistoryMetadataLayout()
    {
        TestOnUIThread([&]() {
            winrt::TerminalApp::TabStrip strip;
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            const auto row = stripImpl->HistoryList().ItemTemplate().LoadContent().as<Grid>();
            const auto icon = row.Children().GetAt(1).as<ContentControl>();
            VERIFY_ARE_EQUAL(0, Grid::GetColumn(icon));
            VERIFY_ARE_EQUAL(2, Grid::GetRowSpan(icon));
            VERIFY_ARE_EQUAL(16.0, icon.Width());
            VERIFY_ARE_EQUAL(16.0, icon.Height());
            VERIFY_ARE_EQUAL(12.0, icon.Margin().Right);
            VERIFY_ARE_EQUAL(VerticalAlignment::Center, icon.VerticalAlignment());
            VERIFY_IS_FALSE(icon.IsTabStop());
            VERIFY_IS_FALSE(icon.IsHitTestVisible());
            VERIFY_ARE_EQUAL(1, Grid::GetColumn(row.Children().GetAt(2).as<FrameworkElement>()));
            const auto metadata = row.Children().GetAt(3).as<Grid>();
            VERIFY_ARE_EQUAL(1, Grid::GetRow(metadata));
            VERIFY_ARE_EQUAL(1, Grid::GetColumn(metadata));
            VERIFY_ARE_EQUAL(HorizontalAlignment::Left, metadata.HorizontalAlignment());
            VERIFY_ARE_EQUAL(GridUnitType::Star, metadata.ColumnDefinitions().GetAt(0).Width().GridUnitType);
            VERIFY_ARE_EQUAL(GridUnitType::Auto, metadata.ColumnDefinitions().GetAt(1).Width().GridUnitType);
            const auto subtitle = metadata.Children().GetAt(0).as<winrt::TerminalApp::HighlightedTextControl>();
            const auto status = metadata.Children().GetAt(1).as<winrt::TerminalApp::HighlightedTextControl>();
            VERIFY_ARE_EQUAL(1, Grid::GetColumn(status));
            VERIFY_ARE_EQUAL(4.0, status.Margin().Left);
            VERIFY_ARE_EQUAL(0.0, status.Margin().Right);

            status.TextBlockStyle(strip.Resources().Lookup(winrt::box_value(L"HistoryActiveTextStyle")).as<Style>());
            subtitle.ApplyTemplate();
            status.ApplyTemplate();
            const auto subtitleText = Media::VisualTreeHelper::GetChild(subtitle, 0).as<TextBlock>();
            const auto statusText = Media::VisualTreeHelper::GetChild(status, 0).as<TextBlock>();
            const auto rowOverhead = row.Padding().Left + icon.Width() + icon.Margin().Right + row.Padding().Right;
            constexpr double tolerance = 1.0;
            for (const auto subtitleValue : { L"Copilot · 2m ago", L"Copilot \u00b7 Ubuntu-24.04 \u00b7 2m ago", L"Localized provider with a very long display name · several minutes ago" })
            {
                for (const auto statusValue : { L"· Idle", L"· Waiting for confirmation" })
                {
                    subtitle.Text(subtitleValue);
                    status.Text(statusValue);
                    subtitleText.Measure({ 10000, 80 });
                    statusText.Measure({ 10000, 80 });
                    const auto subtitleWidth = subtitleText.DesiredSize().Width;
                    const auto statusWidth = statusText.DesiredSize().Width;
                    VERIFY_IS_TRUE(subtitleWidth > 0);
                    VERIFY_IS_TRUE(statusWidth > 0);
                    const auto fixedWidth = rowOverhead + status.Margin().Left + statusWidth;
                    const auto wideWidth = static_cast<float>(fixedWidth + subtitleWidth + 120);
                    const auto narrowWidth = static_cast<float>(fixedWidth + subtitleWidth / 2);

                    // Re-expanding also catches stale trimming or column widths after a resize.
                    for (const auto width : { wideWidth, narrowWidth, wideWidth })
                    {
                        row.Width(width);
                        row.Measure({ width, 80 });
                        row.Arrange({ 0, 0, width, 80 });
                        row.UpdateLayout();
                        const auto subtitlePosition = subtitleText.TransformToVisual(row).TransformPoint({ 0, 0 });
                        const auto statusPosition = statusText.TransformToVisual(row).TransformPoint({ 0, 0 });
                        VERIFY_IS_TRUE(std::abs(subtitlePosition.X - (rowOverhead - row.Padding().Right)) <= tolerance);
                        VERIFY_ARE_EQUAL(Visibility::Visible, status.Visibility());
                        VERIFY_IS_TRUE(statusText.ActualWidth() >= statusWidth - tolerance);
                        VERIFY_IS_FALSE(statusText.IsTextTrimmed());
                        VERIFY_IS_TRUE(statusPosition.X >= subtitlePosition.X);
                        VERIFY_IS_TRUE(statusPosition.X + statusText.ActualWidth() <= width - row.Padding().Right + tolerance);
                        const auto gap = statusPosition.X - (subtitlePosition.X + subtitleText.ActualWidth());
                        VERIFY_IS_TRUE(gap >= status.Margin().Left - tolerance);
                        VERIFY_IS_TRUE(gap <= status.Margin().Left + tolerance);
                        VERIFY_IS_TRUE(std::abs(statusPosition.Y - subtitlePosition.Y) <= tolerance);
                        if (width == narrowWidth)
                        {
                            VERIFY_IS_TRUE(subtitleText.IsTextTrimmed());
                            VERIFY_IS_TRUE(subtitleText.ActualWidth() < subtitleWidth - tolerance);
                        }
                        else
                        {
                            VERIFY_IS_FALSE(subtitleText.IsTextTrimmed());
                            VERIFY_IS_TRUE(std::abs(subtitleText.ActualWidth() - subtitleWidth) <= tolerance);
                            VERIFY_IS_TRUE(width - row.Padding().Right - (statusPosition.X + statusText.ActualWidth()) >= 100);
                        }
                    }
                }
            }
        });
    }

    void TabTests::VerticalTabHistoryAgentIcons()
    {
        TestOnUIThread([&]() {
            winrt::TerminalApp::TabStrip strip;
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            for (const auto provider : { L"copilot", L"claude", L"codex", L"gemini", L"opencode", L"custom:agent", L"" })
            {
                auto item = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
                item.AgentId(provider);
                stripImpl->CommitHistorySnapshot({ item });
                const auto iconId = std::wstring_view{ provider }.empty() || std::wstring_view{ provider }.starts_with(L"custom:") ?
                                        winrt::hstring{ L"generic" } :
                                        winrt::hstring{ provider };
                const auto expected = strip.Resources().Lookup(winrt::box_value(L"AgentIcon." + iconId)).as<DataTemplate>();
                VERIFY_IS_TRUE(item.IconTemplate() == expected);
                const auto art = item.IconTemplate().LoadContent().as<Viewbox>();
                VERIFY_IS_TRUE(static_cast<bool>(art.Child()));
                for (const auto color : { winrt::Windows::UI::Colors::Black(), winrt::Windows::UI::Colors::White() })
                {
                    Media::SolidColorBrush foreground{ color };
                    art.DataContext(foreground);
                    if (const auto path = art.Child().try_as<Shapes::Path>())
                    {
                        VERIFY_IS_TRUE(path.Fill() == foreground);
                    }
                    else if (const auto layers = art.Child().try_as<Grid>())
                    {
                        VERIFY_ARE_EQUAL(2u, layers.Children().Size());
                        for (const auto& layer : layers.Children())
                        {
                            VERIFY_IS_TRUE(layer.as<Shapes::Path>().Fill() == foreground);
                        }
                    }
                    else
                    {
                        VERIFY_IS_TRUE(art.Child().as<SymbolIcon>().Foreground() == foreground);
                    }
                }
            }
        });
    }

    void TabTests::VerticalTabHistoryEndedPresentation()
    {
        HistoryTestView view;
        const auto strip = view.strip;
        const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
        std::vector<winrt::TerminalApp::TabStripHistoryItem> items;
        TestOnUIThread([&]() {
            for (const auto status : { "Ended", "Historical" })
            {
                auto item = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
                item.SessionId(winrt::to_hstring(status));
                item.AgentId(L"copilot");
                item.Status(winrt::to_hstring(status));
                item.StatusText(winrt::TerminalApp::implementation::TerminalPage::_SidebarHistoryStatusText(status));
                item.IsLive(false);
                items.emplace_back(item);
            }
            stripImpl->CommitHistorySnapshot(items);
            VERIFY_ARE_EQUAL(items[0].StatusText(), items[1].StatusText());
        });
        view.Search(items[0].StatusText());
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(2u, strip.HistoryItems().Size());
        });
        view.Search(L"ended");
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Ended" }, strip.HistoryItems().GetAt(0).Status());
            VERIFY_IS_FALSE(strip.HistoryItems().GetAt(0).IsLive());
            for (const auto& item : items)
            {
                item.IsHistorical(true);
            }
            stripImpl->CommitHistorySnapshot(items);
        });
        view.Search(L"history");
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(2u, strip.HistoryItems().Size());
        });
    }

    void TabTests::VerticalTabHistoryUnfinishedFirst()
    {
        HistoryTestView view;
        const auto strip = view.strip;
        const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
        winrt::Windows::Foundation::Collections::IObservableVector<winrt::TerminalApp::TabStripHistoryItem> visibleItems{ nullptr };
        winrt::TerminalApp::TabStripHistoryItem ended{ nullptr }, idle{ nullptr }, historical{ nullptr }, working{ nullptr },
            attention{ nullptr }, error{ nullptr }, unknown{ nullptr }, oldest{ nullptr };
        const auto verifyOrder = [&](const std::initializer_list<const wchar_t*> expected) {
            VERIFY_ARE_EQUAL(expected.size(), static_cast<size_t>(visibleItems.Size()));
            uint32_t index = 0;
            for (const auto id : expected)
            {
                VERIFY_ARE_EQUAL(winrt::hstring{ id }, visibleItems.GetAt(index++).SessionId());
            }
        };
        TestOnUIThread([&]() {
            visibleItems = strip.HistoryItems();
            const auto makeItem = [](const wchar_t* id, const wchar_t* status) {
                auto item = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
                item.SessionId(id);
                item.Title(winrt::hstring{ L"session " } + id);
                item.Status(status);
                item.IsLive(std::wstring_view{ status } == L"Idle" ||
                            std::wstring_view{ status } == L"Working" ||
                            std::wstring_view{ status } == L"Attention" ||
                            std::wstring_view{ status } == L"Error");
                return item;
            };
            ended = makeItem(L"ended-newest", L"Ended");
            idle = makeItem(L"idle", L"Idle");
            historical = makeItem(L"historical-newer", L"Historical");
            working = makeItem(L"working", L"Working");
            attention = makeItem(L"attention", L"Attention");
            error = makeItem(L"error", L"Error");
            unknown = makeItem(L"unknown", L"");
            oldest = makeItem(L"historical-oldest", L"Historical");

            stripImpl->CommitHistorySnapshot({ ended, idle, historical, working, attention, error, unknown, oldest });
            verifyOrder({ L"idle", L"working", L"attention", L"error", L"unknown", L"ended-newest", L"historical-newer", L"historical-oldest" });
        });
        view.Search(L"session");
        TestOnUIThread([&]() {
            verifyOrder({ L"idle", L"working", L"attention", L"error", L"unknown", L"ended-newest", L"historical-newer", L"historical-oldest" });
        });
        view.Search(L"historical");
        TestOnUIThread([&]() {
            verifyOrder({ L"historical-newer", L"historical-oldest" });
        });
        view.Search(L"");
        TestOnUIThread([&]() {
            working.Status(L"Ended");
            working.IsLive(false);
            stripImpl->CommitHistorySnapshot({ working, ended, idle, historical, attention, error, unknown, oldest });
            verifyOrder({ L"idle", L"attention", L"error", L"unknown", L"working", L"ended-newest", L"historical-newer", L"historical-oldest" });

            oldest.Status(L"Idle");
            oldest.IsLive(true);
            stripImpl->CommitHistorySnapshot({ oldest, working, ended, idle, historical, attention, error, unknown });
            verifyOrder({ L"historical-oldest", L"idle", L"attention", L"error", L"unknown", L"working", L"ended-newest", L"historical-newer" });
            VERIFY_ARE_EQUAL(winrt::get_abi(visibleItems), winrt::get_abi(strip.HistoryItems()));
        });
    }

    void TabTests::VerticalTabHistoryStatusStyles()
    {
        TestOnUIThread([&]() {
            const auto previousContent = Window::Current().Content();
            const auto restore = wil::scope_exit([&]() {
                Window::Current().Content(previousContent);
            });
            winrt::TerminalApp::TabStrip strip;
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            const auto attentionStyle = strip.Resources().Lookup(winrt::box_value(L"HistoryAttentionTextStyle")).as<Style>();
            const auto activeStyle = strip.Resources().Lookup(winrt::box_value(L"HistoryActiveTextStyle")).as<Style>();
            const auto errorStyle = strip.Resources().Lookup(winrt::box_value(L"HistoryErrorTextStyle")).as<Style>();
            const auto subtitleStyle = strip.Resources().Lookup(winrt::box_value(L"HistorySubtitleTextStyle")).as<Style>();
            for (const auto status : { L"Attention", L"Working", L"Idle", L"Error", L"Ended", L"Historical", L"" })
            {
                auto item = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
                item.Status(status);
                stripImpl->CommitHistorySnapshot({ item });
                const auto state = std::wstring_view{ status };
                const auto expected = state == L"Working"   ? activeStyle :
                                      state == L"Attention" ? attentionStyle :
                                      state == L"Error"     ? errorStyle :
                                                              subtitleStyle;
                VERIFY_IS_TRUE(item.StatusTextStyle() == expected);
            }

            for (const auto& style : { activeStyle, attentionStyle, errorStyle })
            {
                winrt::TerminalApp::HighlightedTextControl statusText;
                statusText.Text(L"Status text");
                statusText.SearchText(L"text");
                statusText.TextBlockStyle(style);
                ResourceDictionary highlightingResources;
                highlightingResources.Source(winrt::Windows::Foundation::Uri{ L"ms-resource:///Files/TerminalApp/HighlightedTextControlStyle.xaml" });
                statusText.Resources().MergedDictionaries().Append(highlightingResources);
                Window::Current().Content(statusText);
                Window::Current().Activate();
                statusText.ApplyTemplate();
                statusText.UpdateLayout();
                VERIFY_ARE_EQUAL(1, Media::VisualTreeHelper::GetChildrenCount(statusText));
                const auto textBlock = Media::VisualTreeHelper::GetChild(statusText, 0).as<TextBlock>();
                VERIFY_IS_NOT_NULL(textBlock.Foreground());
                const auto foreground = textBlock.Foreground().as<Media::SolidColorBrush>().Color();
                VERIFY_ARE_EQUAL(1.0, textBlock.Opacity());
                VERIFY_ARE_EQUAL(2u, textBlock.Inlines().Size());
                for (const auto& inlineText : textBlock.Inlines())
                {
                    const auto run = inlineText.as<Documents::Run>();
                    VERIFY_IS_NOT_NULL(run.Foreground());
                    VERIFY_ARE_EQUAL(foreground, run.Foreground().as<Media::SolidColorBrush>().Color());
                }
                VERIFY_ARE_EQUAL(FontWeights::Bold().Weight,
                                 textBlock.Inlines().GetAt(1).as<Documents::Run>().FontWeight().Weight);
            }
            VERIFY_IS_TRUE(activeStyle != attentionStyle && activeStyle != errorStyle && attentionStyle != errorStyle);
        });
    }

    void TabTests::VerticalTabHistoryProtocolActivationPreservesView()
    {
        const winrt::guid firstSession{ L"{db061472-5898-4eb6-a218-6d5d042838a5}" };
        auto connection = winrt::make_self<TestConnection>(
            firstSession, winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected);
        auto page = _commonSetup(*connection, nullptr, std::nullopt, true);
        winrt::Windows::Foundation::IAsyncOperation<winrt::Microsoft::Terminal::Protocol::TabCreationResult> create{ nullptr };
        uint32_t summonRequests = 0;
        const auto summonToken = page->SummonWindowRequested([&](auto&&, auto&&) { ++summonRequests; });
        const auto revokeSummon = wil::scope_exit([&]() { page->SummonWindowRequested(summonToken); });
        TestOnUIThread([&]() {
            page->_tabStrip.HistoryActive(true);
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            stripImpl->HistorySearchTextBox().Text(L"history query");
            create = page->CreateProtocolTab(NewTerminalArgs{ 1 }, true);
        });
        const auto created = create.get();
        winrt::Windows::Foundation::IAsyncOperation<bool> focus{ nullptr };
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(1u, created.TabId);
            VERIFY_ARE_EQUAL(2u, page->_tabs.Size());
            VERIFY_IS_TRUE(page->_tabStrip.HistoryActive());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"history query" },
                             winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip)->HistorySearchTextBox().Text());
            VERIFY_ARE_EQUAL(0u, page->_GetFocusedTabIndex().value_or(1));
            VERIFY_IS_TRUE(page->_selectedTabItem() == page->_tabs.GetAt(0).TabViewItem());
            VERIFY_ARE_EQUAL(0u, summonRequests);
            VERIFY_ARE_EQUAL(0.0, page->_tabs.GetAt(created.TabId).Content().Opacity());
            VERIFY_IS_FALSE(page->_tabs.GetAt(created.TabId).Content().IsHitTestVisible());
            VERIFY_IS_FALSE(page->_preserveSidebarHistory);
            VERIFY_IS_TRUE(created.SessionId != firstSession);
            focus = page->FocusProtocolPane(created.SessionId);
        });
        VERIFY_IS_TRUE(focus.get());
        TestOnUIThread([&]() {
            VERIFY_IS_TRUE(page->_tabStrip.HistoryActive());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"history query" },
                             winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip)->HistorySearchTextBox().Text());
            VERIFY_ARE_EQUAL(1u, page->_GetFocusedTabIndex().value_or(0));
            VERIFY_IS_TRUE(page->_selectedTabItem() == page->_tabs.GetAt(1).TabViewItem());
            VERIFY_ARE_EQUAL(created.SessionId, page->_GetActiveControl().Connection().SessionId());
            VERIFY_ARE_EQUAL(1u, summonRequests);
            VERIFY_ARE_EQUAL(1.0, page->_tabs.GetAt(created.TabId).Content().Opacity());
            VERIFY_IS_TRUE(page->_tabs.GetAt(created.TabId).Content().IsHitTestVisible());
            VERIFY_IS_FALSE(page->_preserveSidebarHistory);
        });
    }

    void TabTests::VerticalTabHistoryForegroundProtocolCreationExitsView()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);
        for (const auto activating : { false, true })
        {
            winrt::Windows::Foundation::IAsyncOperation<winrt::Microsoft::Terminal::Protocol::TabCreationResult> create{ nullptr };
            TestOnUIThread([&]() {
                page->_tabStrip.HistoryActive(true);
                page->_tabStrip.HistoryActivating(activating);
                winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip)->HistorySearchTextBox().Text(L"history query");
                create = page->CreateProtocolTab(NewTerminalArgs{ 1 }, false);
            });
            const auto created = create.get();
            TestOnUIThread([&]() {
                VERIFY_IS_FALSE(page->_tabStrip.HistoryActive());
                VERIFY_ARE_EQUAL(created.TabId, page->_GetFocusedTabIndex().value());
                VERIFY_ARE_EQUAL(created.SessionId, page->_GetActiveControl().Connection().SessionId());
                VERIFY_IS_FALSE(page->_preserveSidebarHistory);
            });
        }
    }

    void TabTests::VerticalTabHistoryActivationCompletionPreservesView()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);
        TestOnUIThread([&]() {
            page->_tabStrip.HistoryActive(true);
            page->_tabStrip.HistoryActivating(true);
            page->_historyActivationSerial = 9;
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            stripImpl->HistorySearchTextBox().Text(L"history query");
            VERIFY_IS_FALSE(page->_CompleteSidebarHistoryActivation(8, true, L""));
            VERIFY_IS_TRUE(page->_tabStrip.HistoryActivating());
            auto duplicate = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
            page->_ActivateSidebarHistoryItem(duplicate);
            VERIFY_ARE_EQUAL(9ULL, page->_historyActivationSerial);
            VERIFY_IS_TRUE(page->_CompleteSidebarHistoryActivation(9, true, L""));
            VERIFY_IS_TRUE(page->_tabStrip.HistoryActive());
            VERIFY_IS_FALSE(page->_tabStrip.HistoryActivating());
            VERIFY_IS_FALSE(page->_tabStrip.HistoryLoading());
            VERIFY_IS_TRUE(page->_tabStrip.HistoryError().empty());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"history query" }, stripImpl->HistorySearchTextBox().Text());

            page->_historyActivationSerial = 10;
            page->_tabStrip.HistoryActivating(true);
            VERIFY_IS_TRUE(page->_CompleteSidebarHistoryActivation(10, false, L"Cannot focus session"));
            VERIFY_IS_TRUE(page->_tabStrip.HistoryActive());
            VERIFY_IS_FALSE(page->_tabStrip.HistoryActivating());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Cannot focus session" }, page->_tabStrip.HistoryError());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"history query" }, stripImpl->HistorySearchTextBox().Text());
            page->_CloseSidebarHistory(false);
            VERIFY_IS_FALSE(page->_CompleteSidebarHistoryActivation(10, true, L""));
            VERIFY_IS_FALSE(page->_tabStrip.HistoryActive());
            VERIFY_IS_FALSE(page->_tabStrip.HistoryActivating());
        });
    }

    void TabTests::VerticalTabHistoryActivationKeepsRows()
    {
        TestOnUIThread([&]() {
            winrt::TerminalApp::TabStrip strip;
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            strip.HistoryActive(true);
            strip.HistoryLoading(true);
            VERIFY_IS_TRUE(stripImpl->HistoryLoadingIndicator().IsActive());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->HistoryList().Visibility());

            auto item = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
            item.SessionId(L"claude-session");
            item.AgentId(L"claude");
            item.Title(L"Claude session");
            item.Status(L"Historical");
            stripImpl->CommitHistorySnapshot({ item });
            strip.HistoryLoading(false);
            stripImpl->HistorySearchTextBox().Text(L"Claude");
            const auto items = strip.HistoryItems();

            strip.HistoryActivating(true);
            VERIFY_IS_FALSE(strip.HistoryLoading());
            VERIFY_ARE_EQUAL(Visibility::Visible, stripImpl->HistoryList().Visibility());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, stripImpl->HistoryLoadingIndicator().Visibility());
            VERIFY_IS_FALSE(stripImpl->HistoryLoadingIndicator().IsActive());
            VERIFY_IS_FALSE(stripImpl->HistoryList().IsItemClickEnabled());
            VERIFY_ARE_EQUAL(1u, items.Size());
            VERIFY_IS_TRUE(items.GetAt(0) == item);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Claude" }, stripImpl->HistorySearchTextBox().Text());

            stripImpl->CommitHistorySnapshot({ item });
            strip.HistoryLoading(false);
            VERIFY_IS_TRUE(strip.HistoryActivating());
            VERIFY_IS_FALSE(stripImpl->HistoryList().IsItemClickEnabled());
            VERIFY_ARE_EQUAL(winrt::get_abi(items), winrt::get_abi(strip.HistoryItems()));

            strip.HistoryActivating(false);
            VERIFY_IS_TRUE(stripImpl->HistoryList().IsItemClickEnabled());
            VERIFY_ARE_EQUAL(Visibility::Visible, stripImpl->HistoryList().Visibility());
            strip.HistoryActivating(true);
            strip.HistoryActive(false);
            strip.HistoryActive(true);
            VERIFY_IS_FALSE(strip.HistoryActivating());
            VERIFY_IS_TRUE(stripImpl->HistoryList().IsItemClickEnabled());
        });
    }

    void TabTests::VerticalTabHistoryStartupLoading()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);
        TestOnUIThread([&]() {
            const auto strip = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            page->_tabStrip.HistoryActive(true);
            const auto generation = page->_historyRequestGeneration;
            for (auto i = 0; i < 2; ++i)
            {
                page->_historyRefreshInFlight = true;
                page->_CompleteSidebarHistoryRefresh(generation, page->_ParseSidebarHistorySnapshot(R"({"sessions":[],"history_status":"loading"})"));
                VERIFY_IS_FALSE(page->_historyRefreshInFlight);
                VERIFY_IS_TRUE(page->_tabStrip.HistoryLoading());
                VERIFY_IS_TRUE(strip->HistoryLoadingIndicator().IsActive());
                VERIFY_ARE_EQUAL(Visibility::Collapsed, strip->HistoryMessage().Visibility());
                VERIFY_ARE_EQUAL(0u, page->_tabStrip.HistoryItems().Size());
            }

            page->_CompleteSidebarHistoryRefresh(generation, page->_ParseSidebarHistorySnapshot(R"({"sessions":[],"history_status":"ready"})"));
            VERIFY_IS_FALSE(page->_tabStrip.HistoryLoading());
            VERIFY_ARE_EQUAL(Visibility::Visible, strip->HistoryMessage().Visibility());
            const auto resources = winrt::Windows::ApplicationModel::Resources::Core::ResourceManager::Current()
                                       .MainResourceMap()
                                       .GetSubtree(L"TerminalApp/Resources");
            VERIFY_ARE_EQUAL(resources.GetValue(L"VerticalTabsHistoryEmpty").ValueAsString(), strip->HistoryMessage().Text());

            // Transport failures use the default Error outcome, never a successful empty snapshot.
            page->_CompleteSidebarHistoryRefresh(generation, {});
            VERIFY_IS_FALSE(page->_tabStrip.HistoryLoading());
            VERIFY_ARE_EQUAL(resources.GetValue(L"VerticalTabsHistoryLoadError").ValueAsString(), strip->HistoryMessage().Text());
            page->_CompleteSidebarHistoryRefresh(generation, page->_ParseSidebarHistorySnapshot(R"({"sessions":[],"history_status":"loading"})"));
            VERIFY_IS_TRUE(page->_tabStrip.HistoryLoading());
            VERIFY_IS_TRUE(page->_tabStrip.HistoryError().empty());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, strip->HistoryMessage().Visibility());
        });
    }

    void TabTests::VerticalTabHistoryLoadingAndErrorsKeepRows()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);
        TestOnUIThread([&]() {
            const auto strip = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            page->_tabStrip.HistoryActive(true);
            const auto generation = page->_historyRequestGeneration;
            const auto partial = R"({"history_status":"loading","sessions":[
                {"session_id":"available","provider_id":"copilot","title":"Available session",
                 "location":"Host","status":"Historical"}]})";
            page->_CompleteSidebarHistoryRefresh(generation, page->_ParseSidebarHistorySnapshot(partial));
            VERIFY_IS_FALSE(page->_tabStrip.HistoryLoading());
            VERIFY_ARE_EQUAL(1u, page->_tabStrip.HistoryItems().Size());
            VERIFY_ARE_EQUAL(Visibility::Visible, strip->HistoryList().Visibility());
            VERIFY_IS_TRUE(strip->HistoryList().IsItemClickEnabled());
            const auto item = page->_tabStrip.HistoryItems().GetAt(0);

            strip->HistorySearchTextBox().Text(L"no match");
            page->_CompleteSidebarHistoryRefresh(generation, page->_ParseSidebarHistorySnapshot(R"({"sessions":[],"history_status":"loading"})"));
            VERIFY_IS_FALSE(page->_tabStrip.HistoryLoading());
            VERIFY_IS_TRUE(strip->HasHistoryItems());
            strip->HistorySearchTextBox().Text(L"");

            for (const auto response : {
                     R"({"sessions":[],"history_status":"error"})",
                     R"({"sessions":[]})" })
            {
                page->_CompleteSidebarHistoryRefresh(generation, page->_ParseSidebarHistorySnapshot(response));
                VERIFY_IS_FALSE(page->_tabStrip.HistoryLoading());
                VERIFY_IS_FALSE(page->_tabStrip.HistoryError().empty());
                VERIFY_IS_TRUE(page->_tabStrip.HistoryItems().GetAt(0) == item);
                VERIFY_ARE_EQUAL(Visibility::Visible, strip->HistoryList().Visibility());
                VERIFY_ARE_EQUAL(Visibility::Visible, strip->HistoryMessage().Visibility());
                VERIFY_ARE_EQUAL(1, Grid::GetRow(strip->HistoryMessage()));
                VERIFY_IS_TRUE(strip->HistoryList().IsItemClickEnabled());
            }

            for (const auto metadata : {
                     R"("cli_source":42)",
                     R"("cli_source":{"Unknown":42})",
                     R"("location":42)",
                     R"("location":{"Wsl":{"distro":42}})",
                     R"("status":42)",
                     R"("origin":42)" })
            {
                const auto response = std::string{ R"({"history_status":"ready","sessions":[{"session_id":"replacement","provider_id":"copilot",)" } +
                                      metadata + "}]}";
                const auto parsed = page->_ParseSidebarHistorySnapshot(response);
                VERIFY_IS_TRUE(parsed.state == winrt::TerminalApp::implementation::TerminalPage::_SidebarHistorySnapshot::State::InvalidResponse);
                VERIFY_IS_TRUE(parsed.items.empty());
                page->_CompleteSidebarHistoryRefresh(generation, parsed);
                VERIFY_IS_FALSE(page->_tabStrip.HistoryLoading());
                VERIFY_IS_FALSE(page->_tabStrip.HistoryError().empty());
                VERIFY_ARE_EQUAL(1u, page->_tabStrip.HistoryItems().Size());
                VERIFY_IS_TRUE(page->_tabStrip.HistoryItems().GetAt(0) == item);
                VERIFY_ARE_EQUAL(Visibility::Visible, strip->HistoryList().Visibility());
                VERIFY_IS_TRUE(strip->HistoryList().IsItemClickEnabled());
            }

            page->_CompleteSidebarHistoryRefresh(generation, page->_ParseSidebarHistorySnapshot(partial));
            VERIFY_IS_TRUE(page->_tabStrip.HistoryError().empty());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, strip->HistoryMessage().Visibility());
            page->_CompleteSidebarHistoryRefresh(generation, page->_ParseSidebarHistorySnapshot(R"({"sessions":[],"history_status":"ready"})"));
            VERIFY_IS_FALSE(strip->HasHistoryItems());
            VERIFY_ARE_EQUAL(0u, page->_tabStrip.HistoryItems().Size());
            VERIFY_ARE_EQUAL(0, Grid::GetRow(strip->HistoryMessage()));
        });
    }

    void TabTests::VerticalTabHistoryTelemetryWaitsForReady()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);
        TestOnUIThread([&]() {
            const auto strip = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            page->_historyRefreshInFlight = true;
            strip->OnHistoryClick(nullptr, {});
            page->_historyRefreshPending = false;
            VERIFY_IS_TRUE(strip->_agentFilterTelemetryPending);
            const auto generation = page->_historyRequestGeneration;
            for (const auto state : { "loading", "error" })
            {
                const auto response = std::string{ R"({"history_status":")" } + state + R"(","sessions":[
                    {"session_id":"stale","provider_id":"copilot","title":"Old row","location":"Host","status":"Historical"}]})";
                page->_CompleteSidebarHistoryRefresh(generation, page->_ParseSidebarHistorySnapshot(response));
                VERIFY_ARE_EQUAL(1u, page->_tabStrip.HistoryItems().Size());
                VERIFY_IS_TRUE(strip->_agentFilterTelemetryPending);
            }

            strip->HistorySearchTextBox().Text(L"no match");
            page->_CompleteSidebarHistoryRefresh(generation, page->_ParseSidebarHistorySnapshot(
                                                                 R"({"history_status":"ready","sessions":[
                    {"session_id":"fresh","provider_id":"copilot","title":"New row","location":"Host","status":"Historical"}]})"));
            VERIFY_IS_TRUE(strip->HasHistoryItems());
            VERIFY_ARE_EQUAL(0u, page->_tabStrip.HistoryItems().Size());
            VERIFY_IS_FALSE(strip->_agentFilterTelemetryPending);
            page->_historyRefreshInFlight = true;
            strip->OnHistoryClick(nullptr, {});
            page->_historyRefreshPending = false;
            VERIFY_IS_FALSE(strip->_agentFilterTelemetryPending);
            page->_CompleteSidebarHistoryRefresh(generation, page->_ParseSidebarHistorySnapshot(R"({"sessions":[],"history_status":"ready"})"));
            VERIFY_IS_FALSE(strip->_agentFilterTelemetryPending);

            page->_tabStrip.HistoryActive(false);
            page->_historyRefreshInFlight = true;
            strip->OnHistoryClick(nullptr, {});
            VERIFY_IS_TRUE(strip->_agentFilterTelemetryPending);
            page->_CloseSidebarHistory(false);
            VERIFY_IS_FALSE(strip->_agentFilterTelemetryPending);
        });
    }

    void TabTests::VerticalTabHistorySnapshotRejectsMalformedResponse()
    {
        TestOnUIThread([&]() {
            using Page = winrt::TerminalApp::implementation::TerminalPage;
            for (const auto response : {
                     "",
                     "{}",
                     R"({"sessions":[]})",
                     R"({"sessions":{},"history_status":"ready"})",
                     R"({"sessions":[],"history_status":true})",
                     R"({"sessions":[],"history_status":"unknown"})",
                     R"({"sessions":[],"history_status":"ready"} {})",
                     R"({"sessions":[null],"history_status":"ready"})",
                     R"({"sessions":[{"title":[]}],"history_status":"ready"})" })
            {
                const auto parsed = Page::_ParseSidebarHistorySnapshot(response);
                VERIFY_IS_TRUE(parsed.state == Page::_SidebarHistorySnapshot::State::InvalidResponse);
                VERIFY_IS_TRUE(parsed.items.empty());
            }
            for (const auto metadata : {
                     R"("cli_source":42)",
                     R"("cli_source":[])",
                     R"("cli_source":{})",
                     R"("cli_source":{"Unknown":42})",
                     R"("location":42)",
                     R"("location":[])",
                     R"("location":{})",
                     R"("location":"Wsl")",
                     R"("location":{"Wsl":42})",
                     R"("location":{"Wsl":{}})",
                     R"("location":{"Wsl":{"distro":null}})",
                     R"("location":{"Wsl":{"distro":42}})",
                     R"("status":42)",
                     R"("status":{})",
                     R"("origin":42)",
                     R"("origin":[])" })
            {
                const auto response = std::string{ R"({"history_status":"ready","sessions":[
                    {"session_id":"valid","provider_id":"copilot","location":"Host"},
                    {"session_id":"invalid","provider_id":"copilot",)" } +
                                      metadata + "}]}";
                const auto parsed = Page::_ParseSidebarHistorySnapshot(response);
                VERIFY_IS_TRUE(parsed.state == Page::_SidebarHistorySnapshot::State::InvalidResponse);
                VERIFY_IS_TRUE(parsed.items.empty());
            }
            const auto custom = Page::_ParseSidebarHistorySnapshot(
                R"({"sessions":[{"session_id":"custom-session","provider_id":"custom:test",
                    "cli_source":{"Unknown":"custom:test"},"location":"Host"}],"history_status":"ready"})");
            VERIFY_IS_TRUE(custom.state == Page::_SidebarHistorySnapshot::State::Ready);
            VERIFY_ARE_EQUAL(size_t{ 1 }, custom.items.size());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:test" }, custom.items.front().AgentId());
            const auto optional = Page::_ParseSidebarHistorySnapshot(
                R"({"sessions":[{"session_id":"optional","provider_id":"copilot","location":"Host",
                    "cli_source":null,"status":null,"origin":null}],"history_status":"ready"})");
            VERIFY_IS_TRUE(optional.state == Page::_SidebarHistorySnapshot::State::Ready);
            VERIFY_ARE_EQUAL(size_t{ 1 }, optional.items.size());
            const auto wsl = Page::_ParseSidebarHistorySnapshot(
                R"({"sessions":[{"session_id":"wsl","cli_source":"Copilot",
                    "location":{"Wsl":{"distro":"Ubuntu"}}}],"history_status":"ready"})");
            VERIFY_IS_TRUE(wsl.state == Page::_SidebarHistorySnapshot::State::Ready);
            VERIFY_ARE_EQUAL(size_t{ 1 }, wsl.items.size());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Ubuntu" }, wsl.items.front().WslDistro());
            for (const auto location : { R"("Unknown")", "null" })
            {
                const auto response = std::string{ R"({"history_status":"ready","sessions":[
                    {"session_id":"unavailable","provider_id":"copilot","location":)" } +
                                      location + "}]}";
                const auto parsed = Page::_ParseSidebarHistorySnapshot(response);
                VERIFY_IS_TRUE(parsed.state == Page::_SidebarHistorySnapshot::State::Ready);
                VERIFY_IS_TRUE(parsed.items.empty());
            }
        });
    }

    void TabTests::VerticalTabHistoryTitlesUseFirstLine()
    {
        TestOnUIThread([&]() {
            using Page = winrt::TerminalApp::implementation::TerminalPage;
            winrt::TerminalApp::TabStrip strip;
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            const std::pair<std::wstring_view, std::wstring_view> cases[]{
                { L"Single-line title", L"Single-line title" },
                { L"# Working in Windows Terminal\r\n\r\nHidden instructions", L"# Working in Windows Terminal" },
                { L"First line\nHidden instructions", L"First line" },
                { L"First line\rHidden instructions", L"First line" },
                { L"First line\r\nSecond line\nThird line", L"First line" },
                { L"First line\n\rSecond line", L"First line" },
                { L"First line\r\n", L"First line" },
                { L"  First line  \nHidden instructions", L"  First line  " },
                { L"\u4f1a\u8bdd\u6807\u9898\nHidden instructions", L"\u4f1a\u8bdd\u6807\u9898" },
                { L"\r\nHidden instructions", L"repo" },
                { L"", L"repo" },
            };
            for (const auto& [input, expected] : cases)
            {
                Json::Value response;
                response["history_status"] = "ready";
                auto& row = response["sessions"][0];
                row["session_id"] = "multiline-title";
                row["provider_id"] = "copilot";
                row["location"] = "Host";
                row["status"] = "Historical";
                row["cwd"] = "C:\\repo";
                row["title"] = winrt::to_string(winrt::hstring{ input });
                auto snapshot = Page::_ParseSidebarHistorySnapshot(Json::writeString(Json::StreamWriterBuilder{}, response));
                VERIFY_IS_TRUE(snapshot.state == Page::_SidebarHistorySnapshot::State::Ready);
                VERIFY_ARE_EQUAL(size_t{ 1 }, snapshot.items.size());
                VERIFY_ARE_EQUAL(winrt::hstring{ expected }, snapshot.items.front().Title());
                VERIFY_ARE_EQUAL(winrt::hstring{ L"multiline-title" }, snapshot.items.front().SessionId());

                impl->HistorySearchTextBox().Text(L"");
                impl->CommitHistorySnapshot(std::move(snapshot.items));
                impl->HistorySearchTextBox().Text(L"Hidden instructions");
                VERIFY_ARE_EQUAL(0u, strip.HistoryItems().Size());
                impl->HistorySearchTextBox().Text(winrt::hstring{ expected });
                VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
            }

            TextBlock title;
            title.Style(strip.Resources().Lookup(winrt::box_value(L"HistoryTitleTextStyle")).as<Style>());
            VERIFY_ARE_EQUAL(1, title.MaxLines());
            VERIFY_ARE_EQUAL(TextWrapping::NoWrap, title.TextWrapping());
            title.Text(L"First line");
            title.Measure({ 240, 400 });
            const auto singleLineHeight = title.DesiredSize().Height;
            VERIFY_IS_TRUE(singleLineHeight > 0);
            title.Text(L"First line\r\nSecond line\rThird line\nFourth line");
            title.Measure({ 240, 400 });
            VERIFY_ARE_EQUAL(singleLineHeight, title.DesiredSize().Height);
        });
    }

    void TabTests::VerticalTabHistoryIgnoresStaleLoadingResult()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);
        TestOnUIThread([&]() {
            page->_tabStrip.HistoryActive(true);
            page->_historyRequestGeneration = 10;
            page->_CompleteSidebarHistoryRefresh(9, page->_ParseSidebarHistorySnapshot(R"({"sessions":[],"history_status":"loading"})"));
            VERIFY_IS_FALSE(page->_tabStrip.HistoryLoading());
            VERIFY_IS_TRUE(page->_tabStrip.HistoryError().empty());
            page->_CloseSidebarHistory(false);
            page->_CompleteSidebarHistoryRefresh(10, {});
            VERIFY_IS_FALSE(page->_tabStrip.HistoryActive());
            VERIFY_IS_FALSE(page->_tabStrip.HistoryLoading());
            VERIFY_IS_TRUE(page->_tabStrip.HistoryError().empty());
        });
    }

    void TabTests::VerticalTabHistoryRefreshPreservesCollection()
    {
        HistoryTestView view;
        const auto strip = view.strip;
        const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
        winrt::Windows::Foundation::Collections::IObservableVector<winrt::TerminalApp::TabStripHistoryItem> items{ nullptr };
        winrt::TerminalApp::TabStripHistoryItem changed{ nullptr };
        std::vector<CollectionChange> changes;
        winrt::event_token token{};
        const auto revoke = wil::scope_exit([&]() {
            LOG_IF_FAILED(RunOnUIThread([&]() {
                if (items)
                {
                    items.VectorChanged(token);
                }
            }));
        });
        const auto makeItem = [](const wchar_t* provider, const wchar_t* id, const wchar_t* status) {
            auto item = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
            item.AgentId(provider);
            item.SessionId(id);
            item.AgentSource(L"host");
            item.Title(id);
            item.Subtitle(L"1 minute ago");
            item.Status(status);
            item.IsHistorical(true);
            return item;
        };
        TestOnUIThread([&]() {
            auto first = makeItem(L"copilot", L"same-id", L"Historical");
            auto second = makeItem(L"claude", L"same-id", L"Historical");
            stripImpl->CommitHistorySnapshot({ first, second });
            items = strip.HistoryItems();
            token = items.VectorChanged([&](auto&&, const IVectorChangedEventArgs& args) {
                changes.emplace_back(args.CollectionChange());
            });
            stripImpl->CommitHistorySnapshot({ makeItem(L"copilot", L"same-id", L"Historical"),
                                               makeItem(L"claude", L"same-id", L"Historical") });
            VERIFY_IS_TRUE(changes.empty());
            VERIFY_IS_TRUE(items.GetAt(0) == first);
            VERIFY_IS_TRUE(items.GetAt(1) == second);

            changed = makeItem(L"claude", L"same-id", L"Historical");
            changed.Subtitle(L"2 minutes ago");
            changed.PaneSessionId(L"new-pane");
            stripImpl->CommitHistorySnapshot({ makeItem(L"copilot", L"same-id", L"Historical"), changed });
            VERIFY_ARE_EQUAL(1u, changes.size());
            VERIFY_ARE_EQUAL(CollectionChange::ItemChanged, changes[0]);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"2 minutes ago" }, items.GetAt(1).Subtitle());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"new-pane" }, items.GetAt(1).PaneSessionId());

            auto added = makeItem(L"codex", L"new-id", L"Idle");
            stripImpl->CommitHistorySnapshot({ first, changed, added });
            VERIFY_ARE_EQUAL(winrt::hstring{ L"new-id" }, items.GetAt(0).SessionId());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"copilot" }, items.GetAt(1).AgentId());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"claude" }, items.GetAt(2).AgentId());
            stripImpl->CommitHistorySnapshot({ changed, first });
            VERIFY_ARE_EQUAL(winrt::hstring{ L"claude" }, items.GetAt(0).AgentId());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"copilot" }, items.GetAt(1).AgentId());
            VERIFY_IS_TRUE(std::ranges::find(changes, CollectionChange::Reset) == changes.end());
            VERIFY_ARE_EQUAL(winrt::get_abi(items), winrt::get_abi(strip.HistoryItems()));

            changes.clear();
        });
        view.Search(L"claude");
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(1u, items.Size());
            VERIFY_ARE_EQUAL(CollectionChange::Reset, changes.back());
            changes.clear();
            stripImpl->CommitHistorySnapshot({ makeItem(L"copilot", L"same-id", L"Historical"), changed });
            VERIFY_IS_TRUE(changes.empty());
        });
        view.Search(L"");
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(2u, items.Size());
        });
    }

    void TabTests::VerticalTabHistoryRefreshPreservesScroll()
    {
        winrt::TerminalApp::TabStrip strip{ nullptr };
        Grid host{ nullptr };
        UIElement previousContent{ nullptr };
        ScrollViewer scroll{ nullptr };
        double offset = 0;
        const auto snapshot = [](const wchar_t* age, size_t count) {
            std::vector<winrt::TerminalApp::TabStripHistoryItem> items;
            for (size_t index = 0; index < count; ++index)
            {
                auto item = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
                item.SessionId(winrt::to_hstring(index));
                item.AgentId(L"copilot");
                item.Title(winrt::hstring{ L"Session " } + winrt::to_hstring(index));
                item.Subtitle(age);
                item.Status(L"Historical");
                item.StatusText(L"Historical");
                item.IsHistorical(true);
                items.emplace_back(item);
            }
            return items;
        };
        const auto cleanup = wil::scope_exit([&]() {
            TestOnUIThread([&]() {
                Window::Current().Content(previousContent);
                scroll = nullptr;
                strip = nullptr;
                host = nullptr;
            });
        });
        TestOnUIThread([&]() {
            strip = winrt::TerminalApp::TabStrip{};
            host = Grid{};
            host.Width(320);
            host.Height(400);
            strip.Width(320);
            strip.Height(400);
            strip.HistoryActive(true);
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            impl->CommitHistorySnapshot(snapshot(L"1 minute ago", 80));
            host.Children().Append(strip);
            previousContent = Window::Current().Content();
            Window::Current().Content(host);
            Window::Current().Activate();
            host.UpdateLayout();
        });
        _waitForContentTransferReviewUI([&]() {
            host.UpdateLayout();
            return winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip)->HistoryList().ItemsPanelRoot() != nullptr;
        });
        TestOnUIThread([&]() {
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            const auto list = impl->HistoryList();
            VERIFY_IS_NOT_NULL(list.ItemsPanelRoot());
            VERIFY_ARE_EQUAL(ItemsUpdatingScrollMode::KeepScrollOffset,
                             list.ItemsPanelRoot().as<ItemsStackPanel>().ItemsUpdatingScrollMode());
            std::function<ScrollViewer(const DependencyObject&)> findScroll;
            findScroll = [&](const DependencyObject& parent) -> ScrollViewer {
                if (const auto viewer = parent.try_as<ScrollViewer>())
                {
                    return viewer;
                }
                for (int32_t index = 0; index < Media::VisualTreeHelper::GetChildrenCount(parent); ++index)
                {
                    if (const auto viewer = findScroll(Media::VisualTreeHelper::GetChild(parent, index)))
                    {
                        return viewer;
                    }
                }
                return nullptr;
            };
            scroll = findScroll(list);
            VERIFY_IS_NOT_NULL(scroll);
            VERIFY_IS_TRUE(scroll.ScrollableHeight() > 800);
            scroll.ChangeView(nullptr, 800.0, nullptr, true);
            host.UpdateLayout();
        });
        TestOnUIThread([&]() {
            host.UpdateLayout();
            offset = scroll.VerticalOffset();
            VERIFY_IS_TRUE(offset > 0);
            winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip)->CommitHistorySnapshot(snapshot(L"2 minutes ago", 81));
            host.UpdateLayout();
        });
        TestOnUIThread([&]() {
            host.UpdateLayout();
            VERIFY_IS_TRUE(std::abs(scroll.VerticalOffset() - offset) <= 1.0);
            winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip)->CommitHistorySnapshot(snapshot(L"3 minutes ago", 79));
            host.UpdateLayout();
        });
        TestOnUIThread([&]() {
            host.UpdateLayout();
            VERIFY_IS_TRUE(std::abs(scroll.VerticalOffset() - offset) <= 1.0);
        });
    }

    void TabTests::VerticalTabHistoryCurrentSessionTracksPane()
    {
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;
        const winrt::guid firstId{ L"{15a970e1-676f-440e-b250-e93717c1edc1}" };
        const winrt::guid secondId{ L"{15a970e1-676f-440e-b250-e93717c1edc2}" };
        const auto first = winrt::make_self<TestConnection>(firstId, State::Connected);
        const auto second = winrt::make_self<TestConnection>(secondId, State::Connected);
        const auto page = _commonSetup(*first, nullptr, std::nullopt, true);
        TestOnUIThread([&]() {
            using Page = winrt::TerminalApp::implementation::TerminalPage;
            const auto tab = page->_GetFocusedTabImpl();
            const auto secondPane = page->_MakeTerminalPane(NewTerminalArgs{}, nullptr, *second);
            VERIFY_IS_TRUE(page->_SplitPane(tab, SplitDirection::Right, 0.5f, secondPane));
            const auto firstPane = tab->GetRootPane()->FindPaneBySessionId(firstId);
            VERIFY_IS_NOT_NULL(firstPane);
            VERIFY_SUCCEEDED(page->_OpenNewTab(NewTerminalArgs{}));
            const auto otherTab = page->_GetFocusedTabImpl();
            page->_selectedTabItem(tab->TabViewItem());
            VERIFY_IS_TRUE(tab->FocusPane(firstPane->Id().value()));

            const auto makeItem = [](const wchar_t* session, const winrt::guid& pane, const wchar_t* agent) {
                auto item = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
                item.SessionId(session);
                item.Title(session);
                item.PaneSessionId(winrt::to_hstring(pane));
                item.AgentId(agent);
                item.AgentSource(L"host");
                item.Status(L"Idle");
                item.IsLive(true);
                return item;
            };
            const auto firstItem = makeItem(L"first-session", firstId, L"copilot");
            const auto secondItem = makeItem(L"second-session", secondId, L"claude");
            const auto superseded = makeItem(L"older-session", firstId, L"copilot");
            const auto otherProvider = makeItem(L"first-session", firstId, L"claude");
            page->_paneAgentSessions.insert_or_assign(firstId, Page::_PaneAgentSession{ L"first-session", L"copilot", L"" });
            page->_paneAgentSessions.insert_or_assign(secondId, Page::_PaneAgentSession{ L"second-session", L"claude", L"" });
            page->_tabStrip.HistoryActive(true);
            const auto strip = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            strip->CommitHistorySnapshot({ superseded, otherProvider, firstItem, secondItem });
            VERIFY_IS_TRUE(firstItem.IsCurrent());
            VERIFY_IS_FALSE(secondItem.IsCurrent());
            VERIFY_IS_FALSE(superseded.IsCurrent());
            VERIFY_IS_FALSE(otherProvider.IsCurrent());

            strip->HistorySearchTextBox().Text(L"second-session");
            VERIFY_ARE_EQUAL(1u, page->_tabStrip.HistoryItems().Size());
            VERIFY_IS_FALSE(secondItem.IsCurrent());
            strip->HistorySearchTextBox().Text(L"");
            VERIFY_IS_TRUE(firstItem.IsCurrent());

            page->_historyActivationSerial = 17;
            VERIFY_IS_TRUE(page->_CompleteSidebarHistoryActivation(17, false, L"Activation failed"));
            VERIFY_IS_TRUE(firstItem.IsCurrent());
            VERIFY_IS_FALSE(secondItem.IsCurrent());

            VERIFY_IS_TRUE(tab->FocusPane(secondPane->Id().value()));
            VERIFY_IS_FALSE(firstItem.IsCurrent());
            VERIFY_IS_TRUE(secondItem.IsCurrent());
            tab->SetRuntimeTabColor(winrt::Windows::UI::Colors::LightSkyBlue());
            VERIFY_ARE_EQUAL(tab->TabViewItem().Background().as<Media::SolidColorBrush>().Color(),
                             secondItem.CurrentBackground().as<Media::SolidColorBrush>().Color());
            VERIFY_ARE_EQUAL(winrt::Windows::UI::Colors::Black(),
                             secondItem.CurrentForeground().as<Media::SolidColorBrush>().Color());
            tab->SetRuntimeTabColor(winrt::Windows::UI::Colors::DarkBlue());
            VERIFY_ARE_EQUAL(winrt::Windows::UI::Colors::White(),
                             secondItem.CurrentForeground().as<Media::SolidColorBrush>().Color());

            page->_selectedTabItem(otherTab->TabViewItem());
            VERIFY_IS_FALSE(firstItem.IsCurrent());
            VERIFY_IS_FALSE(secondItem.IsCurrent());
            page->_selectedTabItem(tab->TabViewItem());
            VERIFY_IS_TRUE(secondItem.IsCurrent());

            VERIFY_IS_TRUE(strip->ApplyHistoryStatusDelta(L"second-session", winrt::to_hstring(secondId), L"Ended", L"Historical"));
            VERIFY_IS_FALSE(secondItem.IsCurrent());
            VERIFY_IS_TRUE(strip->ApplyHistoryStatusDelta(L"second-session", winrt::to_hstring(secondId), L"Idle", L"Idle"));
            VERIFY_IS_TRUE(secondItem.IsCurrent());
        });
    }

    void TabTests::VerticalTabHistoryCurrentSessionColors()
    {
        const auto cleanup = wil::scope_exit([&]() {
            TestOnUIThread([&]() { Window::Current().Content(nullptr); });
        });
        TestOnUIThread([&]() {
            winrt::TerminalApp::TabStrip strip;
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            winrt::MUX::Controls::TabViewItem tab;
            const auto first = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
            const auto second = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
            first.Title(L"First session");
            second.Title(L"Second session");
            impl->CommitHistorySnapshot({ first, second });
            strip.Width(360);
            strip.Height(400);
            strip.HistoryActive(true);
            ContentControl host;
            host.Content(strip);
            Window::Current().Content(host);
            Window::Current().Activate();
            host.UpdateLayout();
            VERIFY_IS_TRUE(impl->HistoryList().ReadLocalValue(ItemsControl::ItemContainerStyleProperty()) == DependencyProperty::UnsetValue());
            uint32_t collectionChanges = 0;
            const auto changed = strip.HistoryItems().VectorChanged(winrt::auto_revoke, [&](auto&&, auto&&) { ++collectionChanges; });
            bool notified = false;
            const auto propertyChanged = first.PropertyChanged(winrt::auto_revoke, [&](auto&&, const auto& args) {
                notified |= args.PropertyName() == L"IsCurrent";
            });
            const auto currentStatus = winrt::Windows::ApplicationModel::Resources::Core::ResourceManager::Current()
                                           .MainResourceMap().GetSubtree(L"TerminalApp/Resources")
                                           .GetValue(L"VerticalTabsHistoryCurrentSession").ValueAsString();
            VERIFY_IS_FALSE(currentStatus.empty());

            for (const auto theme : { ElementTheme::Dark, ElementTheme::Light })
            {
                strip.RequestedTheme(theme);
                tab.Background(nullptr);
                impl->SetCurrentHistoryItem(nullptr, tab);
                host.UpdateLayout();
                const auto container = impl->HistoryList().ContainerFromIndex(0).as<ListViewItem>();
                const auto row = container.ContentTemplateRoot().as<Grid>();
                const auto title = row.FindName(L"HistoryTitleText").as<winrt::TerminalApp::HighlightedTextControl>();
                const auto selection = row.FindName(L"HistorySelectionBackground").as<Border>();
                const auto palette = row.FindName(L"HistorySelectionPalette").as<ContentControl>();
                const auto inheritedForeground = title.Foreground();
                VERIFY_IS_NOT_NULL(inheritedForeground);
                VERIFY_IS_NULL(row.GetBindingExpression(Panel::BackgroundProperty()));
                VERIFY_IS_TRUE(title.ReadLocalValue(Control::ForegroundProperty()) == DependencyProperty::UnsetValue());
                VERIFY_ARE_EQUAL(Visibility::Collapsed, selection.Visibility());
                VERIFY_IS_TRUE(Automation::AutomationProperties::GetItemStatus(container).empty());

                impl->SetCurrentHistoryItem(first, tab);
                host.UpdateLayout();
                VERIFY_IS_TRUE(first.IsCurrent());
                VERIFY_IS_TRUE(notified);
                VERIFY_IS_NULL(first.CurrentBackground());
                VERIFY_IS_NULL(first.CurrentForeground());
                VERIFY_IS_NULL(second.CurrentBackground());
                VERIFY_ARE_EQUAL(Visibility::Visible, selection.Visibility());
                VERIFY_IS_NOT_NULL(selection.Background());
                VERIFY_IS_TRUE(title.Foreground() == palette.Foreground());
                VERIFY_ARE_EQUAL(currentStatus, Automation::AutomationProperties::GetItemStatus(container));
                VERIFY_IS_TRUE(Automation::AutomationProperties::GetItemStatus(
                                   impl->HistoryList().ContainerFromIndex(1)).empty());

                auto tabBrush = Media::SolidColorBrush{ winrt::Windows::UI::Colors::DarkBlue() };
                tabBrush.Opacity(0.3);
                tab.Background(tabBrush);
                impl->SetCurrentHistoryItem(first, tab);
                host.UpdateLayout();
                VERIFY_ARE_EQUAL(tabBrush.Color(), palette.Content().as<Border>().Background().as<Media::SolidColorBrush>().Color());
                VERIFY_ARE_EQUAL(1.0, first.CurrentBackground().Opacity());
                VERIFY_ARE_EQUAL(winrt::Windows::UI::Colors::White(), title.Foreground().as<Media::SolidColorBrush>().Color());

                impl->SetCurrentHistoryItem(second, tab);
                host.UpdateLayout();
                VERIFY_IS_FALSE(first.IsCurrent());
                VERIFY_IS_TRUE(second.IsCurrent());
                VERIFY_IS_NULL(first.CurrentBackground());
                VERIFY_ARE_EQUAL(Visibility::Collapsed, selection.Visibility());
                VERIFY_IS_TRUE(title.ReadLocalValue(Control::ForegroundProperty()) == DependencyProperty::UnsetValue());
                VERIFY_IS_TRUE(title.Foreground() == inheritedForeground);
                VERIFY_IS_TRUE(Automation::AutomationProperties::GetItemStatus(container).empty());
                VERIFY_ARE_EQUAL(currentStatus, Automation::AutomationProperties::GetItemStatus(
                                                    impl->HistoryList().ContainerFromIndex(1)));
            }
            impl->SetCurrentHistoryItem(nullptr, tab);
            VERIFY_IS_FALSE(second.IsCurrent());
            VERIFY_ARE_EQUAL(0u, collectionChanges);

            tab.Background(nullptr);
            impl->SetCurrentHistoryItem(first, tab);
            const auto currentRow = impl->HistoryList().ContainerFromIndex(0).as<ListViewItem>().ContentTemplateRoot().as<Grid>();
            const auto currentTitle = currentRow.FindName(L"HistoryTitleText").as<winrt::TerminalApp::HighlightedTextControl>();
            const auto currentPalette = currentRow.FindName(L"HistorySelectionPalette").as<ContentControl>();
            currentPalette.Foreground(Media::SolidColorBrush{ winrt::Windows::UI::Colors::Magenta() });
            host.UpdateLayout();
            VERIFY_IS_TRUE(currentTitle.Foreground() == currentPalette.Foreground());
            const auto replacement = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
            replacement.Title(L"Replacement session");
            impl->CommitHistorySnapshot({ replacement });
            host.UpdateLayout();
            const auto recycledRow = impl->HistoryList().ContainerFromIndex(0).as<ListViewItem>().ContentTemplateRoot().as<Grid>();
            const auto recycledTitle = recycledRow.FindName(L"HistoryTitleText").as<winrt::TerminalApp::HighlightedTextControl>();
            VERIFY_IS_TRUE(recycledTitle.ReadLocalValue(Control::ForegroundProperty()) == DependencyProperty::UnsetValue());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, recycledRow.FindName(L"HistorySelectionBackground").as<Border>().Visibility());
            VERIFY_IS_TRUE(Automation::AutomationProperties::GetItemStatus(
                               impl->HistoryList().ContainerFromIndex(0)).empty());
        });
    }

    void TabTests::VerticalTabHistoryWslDistroMetadata()
    {
        TestOnUIThread([&]() {
            using Page = winrt::TerminalApp::implementation::TerminalPage;
            auto snapshot = Page::_ParseSidebarHistorySnapshot(R"({
                "history_status": "ready",
                "sessions": [
                    {"session_id":"host","provider_id":"copilot","location":"Host","status":"Idle"},
                    {"session_id":"ubuntu","provider_id":"copilot","location":{"Wsl":{"distro":"Ubuntu-24.04"}},"status":"Working"},
                    {"session_id":"debian","provider_id":"claude","location":{"Wsl":{"distro":"Debian"}},"status":"Historical"}
                ]
            })");
            VERIFY_IS_TRUE(snapshot.state == Page::_SidebarHistorySnapshot::State::Ready);
            VERIFY_ARE_EQUAL(size_t{ 3 }, snapshot.items.size());

            const auto age = Page::_SidebarHistoryAgeText(std::nullopt, 0);
            const auto hostMetadata = winrt::hstring{ L"Copilot \u00b7 " } + age + L" \u00b7 ";
            const auto ubuntuMetadata = winrt::hstring{ L"Copilot \u00b7 Ubuntu-24.04 \u00b7 " } + age + L" \u00b7 ";
            const auto debianMetadata = winrt::hstring{ L"Claude \u00b7 Debian \u00b7 " } + age + L" \u00b7 ";
            VERIFY_ARE_EQUAL(hostMetadata, snapshot.items[0].Subtitle());
            VERIFY_ARE_EQUAL(ubuntuMetadata, snapshot.items[1].Subtitle());
            VERIFY_ARE_EQUAL(debianMetadata, snapshot.items[2].Subtitle());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"wsl" }, snapshot.items[1].AgentSource());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Ubuntu-24.04" }, snapshot.items[1].WslDistro());

            winrt::TerminalApp::TabStrip strip;
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            stripImpl->CommitHistorySnapshot(std::move(snapshot.items));
            stripImpl->HistorySearchTextBox().Text(L"ubuntu-24.04");
            VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
            const auto item = strip.HistoryItems().GetAt(0);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"ubuntu" }, item.SessionId());

            const auto status = Page::_SidebarHistoryStatusText("Attention");
            VERIFY_IS_TRUE(stripImpl->ApplyHistoryStatusDelta(L"ubuntu", L"pane-ubuntu", L"Attention", status));
            VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
            VERIFY_ARE_EQUAL(ubuntuMetadata, item.Subtitle());
            VERIFY_ARE_EQUAL(status, item.StatusText());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Ubuntu-24.04" }, item.WslDistro());

            stripImpl->HistorySearchTextBox().Text(ubuntuMetadata + status);
            VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"ubuntu" }, strip.HistoryItems().GetAt(0).SessionId());
            stripImpl->HistorySearchTextBox().Text(L"debian");
            VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
            VERIFY_ARE_EQUAL(debianMetadata, strip.HistoryItems().GetAt(0).Subtitle());
        });
    }

    void TabTests::VerticalTabHistorySearchProjection()
    {
        HistoryTestView view;
        const auto strip = view.strip;
        const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
        winrt::TerminalApp::TabStripHistoryItem host{ nullptr }, wsl{ nullptr }, attention{ nullptr }, unknown{ nullptr }, updated{ nullptr };
        winrt::hstring attentionText, workingText;
        TestOnUIThread([&]() {
            const auto visibleItems = strip.HistoryItems();

            host = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
            host.Title(L"Fix build");
            host.AgentId(L"copilot");
            host.ProviderDisplayName(L"Copilot");
            host.AgentSource(L"host");
            host.Status(L"Idle");
            host.Subtitle(L"Copilot \u00b7 just now \u00b7 ");
            host.StatusText(L"Idle");
            host.IsLive(true);

            wsl = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
            wsl.Title(L"Deploy service");
            wsl.AgentId(L"claude");
            wsl.ProviderDisplayName(L"Claude");
            wsl.AgentSource(L"wsl");
            wsl.WslDistro(L"Ubuntu");
            wsl.Status(L"Historical");
            wsl.Subtitle(L"Claude \u00b7 Ubuntu \u00b7 1 hour ago \u00b7 ");
            wsl.StatusText(L"Historical");
            wsl.IsLive(false);
            wsl.IsHistorical(true);

            attentionText = winrt::TerminalApp::implementation::TerminalPage::_SidebarHistoryStatusText("Attention");
            attention = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
            attention.SessionId(L"attention-session");
            attention.Title(L"Review changes");
            attention.Subtitle(L"Copilot \u00b7 5 minutes ago \u00b7 ");
            attention.StatusText(attentionText);
            attention.Status(L"Attention");
            attention.IsLive(true);

            unknown = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
            unknown.Title(L"Unknown session");
            unknown.AgentId(L"copilot");
            unknown.ProviderDisplayName(L"Copilot");
            unknown.AgentSource(L"host");
            unknown.Status(L"FutureStatus");
            unknown.Subtitle(L"Copilot \u00b7 just now \u00b7 ");
            unknown.StatusText(L"Unknown");

            stripImpl->CommitHistorySnapshot({ host, wsl, attention, unknown });
            VERIFY_ARE_EQUAL(winrt::get_abi(visibleItems), winrt::get_abi(strip.HistoryItems()));
            VERIFY_ARE_EQUAL(4u, strip.HistoryItems().Size());

        });
        view.Search(L"ubuntu");
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Deploy service" }, strip.HistoryItems().GetAt(0).Title());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"ubuntu" }, strip.HistoryItems().GetAt(0).SearchQuery());

        });
        view.Search(L"idle");
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Fix build" }, strip.HistoryItems().GetAt(0).Title());

        });
        view.Search(attentionText);
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"attention-session" }, strip.HistoryItems().GetAt(0).SessionId());
            VERIFY_ARE_EQUAL(attentionText, strip.HistoryItems().GetAt(0).SearchQuery());

        });
        view.Search(winrt::hstring{ L"5 minutes ago \u00b7 " } + attentionText);
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
        });
        view.Search(L"5 minutes ago");
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Copilot \u00b7 5 minutes ago \u00b7 " },
                             strip.HistoryItems().GetAt(0).Subtitle());

        });
        view.Search(L"attention");
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
        });
        view.Search(L"live");
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(2u, strip.HistoryItems().Size());
        });
        view.Search(L"history");
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Deploy service" }, strip.HistoryItems().GetAt(0).Title());
        });
        view.Search(L"historical");
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
        });
        view.Search(L"unknown");
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Unknown session" }, strip.HistoryItems().GetAt(0).Title());

        });
        view.Search(attentionText);
        TestOnUIThread([&]() {
            updated = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
            updated.SessionId(attention.SessionId());
            updated.Title(attention.Title());
            updated.Status(L"Working");
            updated.IsLive(true);
            workingText = winrt::TerminalApp::implementation::TerminalPage::_SidebarHistoryStatusText("Working");
            updated.Subtitle(attention.Subtitle());
            updated.StatusText(workingText);
            stripImpl->CommitHistorySnapshot({ host, wsl, updated, unknown });
            VERIFY_ARE_EQUAL(0u, strip.HistoryItems().Size());
        });
        view.Search(workingText);
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
            VERIFY_ARE_EQUAL(updated.Subtitle(), strip.HistoryItems().GetAt(0).Subtitle());
            VERIFY_ARE_EQUAL(workingText, strip.HistoryItems().GetAt(0).StatusText());
            VERIFY_IS_FALSE(updated.StatusTextStyle() == attention.StatusTextStyle());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Working" }, strip.HistoryItems().GetAt(0).Status());
            VERIFY_IS_TRUE(strip.HistoryItems().GetAt(0).IsLive());

        });
        view.Search(L"missing");
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(0u, strip.HistoryItems().Size());

        });
        view.Search(L"");
        TestOnUIThread([&]() {
            VERIFY_ARE_EQUAL(4u, strip.HistoryItems().Size());
        });
    }

    void TabTests::VerticalTabHistoryPreservesLiveSearch()
    {
        TestOnUIThread([&]() {
            winrt::TerminalApp::TabStrip strip;

            strip.SearchActive(true);
            strip.SearchQuery(L"power");
            strip.HistoryActive(true);

            VERIFY_IS_TRUE(strip.SearchActive());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"power" }, strip.SearchQuery());

            strip.HistoryActive(false);
            VERIFY_IS_TRUE(strip.SearchActive());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"power" }, strip.SearchQuery());
        });
    }

    void TabTests::VerticalTabHistoryClosePreservesForegroundSelection()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            page->_tabStrip.HistoryActive(true);
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            stripImpl->HistorySearchTextBox().Focus(FocusState::Programmatic);

            NewTerminalArgs newTerminalArgs{ 1 };
            VERIFY_SUCCEEDED(page->_OpenNewTab(newTerminalArgs, false));
            VERIFY_ARE_EQUAL(2u, page->_tabs.Size());

            const auto foregroundItem = page->_tabs.GetAt(1).TabViewItem();
            VERIFY_IS_FALSE(page->_tabStrip.HistoryActive());
            VERIFY_IS_TRUE(page->_selectedTabItem() == foregroundItem);
            VERIFY_ARE_EQUAL(1u, page->_GetFocusedTabIndex().value_or(0));
        });

        TestOnUIThread([&]() {
            const auto foregroundItem = page->_tabs.GetAt(1).TabViewItem();
            VERIFY_IS_TRUE(page->_selectedTabItem() == foregroundItem);
            VERIFY_ARE_EQUAL(1u, page->_GetFocusedTabIndex().value_or(0));
        });
    }

    void TabTests::VerticalTabHistoryCloseCancelsRefresh()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);
        TestOnUIThread([&]() {
            const auto strip = page->_tabStrip;
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            const auto cancellation = std::make_shared<std::atomic<bool>>(false);
            page->_historyRefreshCancellation = cancellation;
            page->_historyRefreshInFlight = true;
            impl->OnHistoryClick(nullptr, {});
            VERIFY_IS_TRUE(strip.HistoryLoading());
            VERIFY_IS_FALSE(cancellation->load());
            const auto generation = page->_historyRequestGeneration;
            impl->OnHistoryCloseClick(nullptr, {});
            VERIFY_IS_TRUE(cancellation->load());
            VERIFY_IS_TRUE(page->_historyRefreshInFlight);
            VERIFY_IS_FALSE(strip.HistoryActive());
            VERIFY_IS_FALSE(strip.HistoryLoading());
            VERIFY_IS_FALSE(page->_historyRefreshPending);
            VERIFY_IS_TRUE(page->_historyRequestGeneration > generation);

            using State = winrt::TerminalApp::implementation::TerminalPage::_SidebarHistorySnapshot::State;
            page->_CompleteSidebarHistoryRefresh(generation, { State::Cancelled, {} });
            VERIFY_IS_FALSE(page->_historyRefreshInFlight);
            VERIFY_IS_TRUE(page->_historyRefreshCancellation == nullptr);
            VERIFY_IS_TRUE(strip.HistoryError().empty());
            VERIFY_ARE_EQUAL(int64_t{ 0 }, page->_historyRetryDelay.count());
            VERIFY_ARE_EQUAL(0u, strip.HistoryItems().Size());
        });
        TestOnUIThread([&]() {
            const auto cancellation = std::make_shared<std::atomic<bool>>(false);
            {
                auto closingPage = winrt::make_self<winrt::TerminalApp::implementation::TerminalPage>(*_windowProperties, *_contentManager);
                closingPage->_historyRefreshCancellation = cancellation;
            }
            VERIFY_IS_TRUE(cancellation->load());
        });
    }

    void TabTests::VerticalTabHistoryRefreshBackoff()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);
        TestOnUIThread([&]() {
            page->_tabStrip.HistoryActive(true);
            const auto generation = page->_historyRequestGeneration;
            using State = winrt::TerminalApp::implementation::TerminalPage::_SidebarHistorySnapshot::State;
            for (const auto seconds : { 5, 10, 20, 40, 60, 60 })
            {
                page->_historyRefreshInFlight = true;
                page->_RequestSidebarHistoryRefresh(false);
                page->_RequestSidebarHistoryRefresh(false);
                VERIFY_IS_TRUE(page->_historyRefreshPending);
                VERIFY_ARE_EQUAL(generation, page->_historyRequestGeneration);
                const auto before = std::chrono::steady_clock::now();
                page->_CompleteSidebarHistoryRefresh(generation, {});
                const auto after = std::chrono::steady_clock::now();
                VERIFY_ARE_EQUAL(static_cast<int64_t>(seconds), page->_historyRetryDelay.count());
                VERIFY_IS_TRUE(page->_historyNextRefresh >= before + std::chrono::seconds{ seconds });
                VERIFY_IS_TRUE(page->_historyNextRefresh <= after + std::chrono::seconds{ seconds });
                VERIFY_IS_FALSE(page->_historyRefreshPending);
                VERIFY_IS_FALSE(page->_historyRefreshInFlight);
                page->OnSessionRegistryChanged(L"");
                page->_RequestSidebarHistoryRefresh(false);
                VERIFY_IS_FALSE(page->_historyRefreshInFlight);
                VERIFY_ARE_EQUAL(generation, page->_historyRequestGeneration);
            }
            const auto nextRefresh = page->_historyNextRefresh;
            page->_CompleteSidebarHistoryRefresh(generation, { State::Loading, {} });
            VERIFY_ARE_EQUAL(int64_t{ 60 }, page->_historyRetryDelay.count());
            VERIFY_IS_TRUE(page->_historyNextRefresh == nextRefresh);
            page->_CompleteSidebarHistoryRefresh(generation, { State::Ready, {} });
            VERIFY_ARE_EQUAL(int64_t{ 0 }, page->_historyRetryDelay.count());
            VERIFY_IS_TRUE(page->_historyNextRefresh == std::chrono::steady_clock::time_point{});
            page->_CompleteSidebarHistoryRefresh(generation, { State::InvalidResponse, {} });
            VERIFY_ARE_EQUAL(int64_t{ 5 }, page->_historyRetryDelay.count());
            page->_CloseSidebarHistory(false);
            VERIFY_ARE_EQUAL(int64_t{ 0 }, page->_historyRetryDelay.count());
        });
    }

    void TabTests::VerticalTabHistoryRefreshDuringActivation()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);
        TestOnUIThread([&]() {
            const auto strip = page->_tabStrip;
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            using State = winrt::TerminalApp::implementation::TerminalPage::_SidebarHistorySnapshot::State;
            strip.HistoryActive(true);
            auto item = winrt::make<winrt::TerminalApp::implementation::TabStripHistoryItem>();
            item.SessionId(L"existing");
            item.Title(L"Session");
            impl->CommitHistorySnapshot({ item });
            page->_historyRefreshInFlight = true;
            const auto cancellation = std::make_shared<std::atomic<bool>>(false);
            page->_historyRefreshCancellation = cancellation;
            const auto oldGeneration = page->_historyRequestGeneration;
            page->_StopSidebarHistoryRefreshTimer();
            VERIFY_IS_TRUE(cancellation->load());
            strip.HistoryActivating(true);
            page->OnSessionRegistryChanged(L"");
            VERIFY_IS_TRUE(page->_historyRefreshPending);
            page->_CompleteSidebarHistoryRefresh(oldGeneration, { State::Cancelled, {} });
            VERIFY_IS_TRUE(strip.HistoryActivating());
            VERIFY_IS_FALSE(strip.HistoryLoading());
            VERIFY_IS_FALSE(page->_historyRefreshInFlight);
            VERIFY_ARE_EQUAL(Visibility::Visible, impl->HistoryList().Visibility());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, impl->HistoryMessage().Visibility());
            VERIFY_IS_FALSE(impl->HistoryList().IsItemClickEnabled());
            VERIFY_ARE_EQUAL(1u, strip.HistoryItems().Size());
            page->OnSessionRegistryChanged(L"");
            VERIFY_IS_FALSE(page->_historyRefreshInFlight);
            VERIFY_IS_TRUE(page->_historyRefreshPending);

            VERIFY_IS_TRUE(page->_CompleteSidebarHistoryActivation(page->_historyActivationSerial, false, L"Cannot focus session"));
            // Simulate completion of the coalesced refresh without launching WTA.
            page->_historyRefreshPending = false;
            page->_CompleteSidebarHistoryRefresh(page->_historyRequestGeneration, { State::Ready, { item } });
            VERIFY_IS_TRUE(strip.HistoryActive());
            VERIFY_IS_FALSE(strip.HistoryActivating());
            VERIFY_ARE_EQUAL(Visibility::Visible, impl->HistoryList().Visibility());
            VERIFY_IS_TRUE(impl->HistoryList().IsItemClickEnabled());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Cannot focus session" }, impl->HistoryMessage().Text());
            VERIFY_ARE_EQUAL(Visibility::Visible, impl->HistoryMessage().Visibility());
            page->_CompleteSidebarHistoryRefresh(page->_historyRequestGeneration, {});
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Cannot focus session" }, impl->HistoryMessage().Text());
            VERIFY_IS_TRUE(strip.HistoryActive());
            VERIFY_IS_TRUE(page->_CompleteSidebarHistoryActivation(page->_historyActivationSerial, true, L""));
            VERIFY_IS_FALSE(strip.HistoryError().empty());
            VERIFY_ARE_EQUAL(Visibility::Visible, impl->HistoryMessage().Visibility());
            page->_CompleteSidebarHistoryRefresh(page->_historyRequestGeneration, { State::Ready, { item } });
            VERIFY_ARE_EQUAL(Visibility::Collapsed, impl->HistoryMessage().Visibility());
            VERIFY_IS_TRUE(strip.HistoryActive());
            page->_CloseSidebarHistory(false);
        });
    }

    void TabTests::VerticalTabHistoryRefreshAfterReopen()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);
        TestOnUIThread([&]() {
            const auto strip = page->_tabStrip;
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            using State = winrt::TerminalApp::implementation::TerminalPage::_SidebarHistorySnapshot::State;
            strip.HistoryActive(true);
            const auto generation = page->_historyRequestGeneration;
            page->_CompleteSidebarHistoryRefresh(generation, {});
            page->_historyRefreshInFlight = true;
            const auto cancellation = std::make_shared<std::atomic<bool>>(false);
            page->_historyRefreshCancellation = cancellation;
            impl->OnHistoryCloseClick(nullptr, {});
            VERIFY_ARE_EQUAL(int64_t{ 0 }, page->_historyRetryDelay.count());
            VERIFY_ARE_EQUAL(Visibility::Visible, impl->ItemsList().Visibility());
            VERIFY_IS_TRUE(cancellation->load());
            VERIFY_IS_FALSE(strip.HistoryActive());
            VERIFY_ARE_EQUAL(int64_t{ 0 }, page->_historyRetryDelay.count());
            page->_historyRefreshInFlight = true;
            impl->OnHistoryClick(nullptr, {});
            VERIFY_IS_TRUE(strip.HistoryLoading());
            VERIFY_IS_TRUE(page->_historyRefreshPending);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, impl->HistoryMessage().Visibility());
            strip.HistoryActivating(true);
            page->_CompleteSidebarHistoryRefresh(generation, { State::Cancelled, {} });
            VERIFY_IS_TRUE(strip.HistoryLoading());
            VERIFY_IS_TRUE(strip.HistoryActivating());
            VERIFY_IS_FALSE(page->_historyRefreshInFlight);
            VERIFY_IS_TRUE(page->_historyRefreshPending);
            VERIFY_IS_TRUE(page->_historyRefreshCancellation == nullptr);
            VERIFY_IS_TRUE(strip.HistoryError().empty());
            page->_CloseSidebarHistory(false);
        });
    }

    void TabTests::VerticalTabStripCollapsedItemsPreserveSelection()
    {
        winrt::TerminalApp::TabStrip strip;
        Grid host;
        winrt::MUX::Controls::TabViewItem first;
        winrt::MUX::Controls::TabViewItem second;
        winrt::MUX::Controls::TabViewItem third;

        TestOnUIThread([&]() {
            host.Width(240);
            host.Height(160);
            strip.Width(240);
            strip.Height(160);

            first.Header(winrt::box_value(L"First"));
            second.Header(winrt::box_value(L"Second"));
            third.Header(winrt::box_value(L"Third"));

            strip.TabItems().Append(first);
            strip.TabItems().Append(second);
            strip.TabItems().Append(third);
            host.Children().Append(strip);
            Window::Current().Content(host);
            Window::Current().Activate();
            host.UpdateLayout();

            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            const auto firstContainer = strip.ContainerFromIndex(0).as<ListViewItem>();
            const auto secondContainer = strip.ContainerFromIndex(1).as<ListViewItem>();
            const auto thirdContainer = strip.ContainerFromIndex(2).as<ListViewItem>();
            const auto tabForContainer = [&](const ListViewItem& container) {
                return stripImpl->ItemsList().ItemFromContainer(container).as<winrt::TerminalApp::TabStripDisplayItem>().Tab();
            };
            VERIFY_IS_TRUE(strip.IsLoaded());
            VERIFY_IS_TRUE(strip.ActualWidth() > 0);
            VERIFY_IS_TRUE(strip.ActualHeight() > 0);
            VERIFY_IS_TRUE(tabForContainer(firstContainer) == first);
            VERIFY_IS_TRUE(tabForContainer(secondContainer) == second);
            VERIFY_IS_TRUE(tabForContainer(thirdContainer) == third);

            strip.SelectedItem(second);
            VERIFY_IS_TRUE(strip.SelectedItem() == second);
            VERIFY_IS_TRUE(stripImpl->ItemsList().SelectedItem().as<winrt::TerminalApp::TabStripDisplayItem>().Tab() == second);
            VERIFY_ARE_EQUAL(1, strip.SelectedIndex());

            uint32_t selectionChanges = 0;
            const auto selectionToken = strip.SelectionChanged([&](auto&&, auto&&) {
                ++selectionChanges;
            });
            const auto revokeSelection = wil::scope_exit([&]() {
                strip.SelectionChanged(selectionToken);
            });

            const auto firstTop = firstContainer.TransformToVisual(strip).TransformPoint({ 0, 0 }).Y;
            const auto secondTop = secondContainer.TransformToVisual(strip).TransformPoint({ 0, 0 }).Y;
            const auto thirdTop = thirdContainer.TransformToVisual(strip).TransformPoint({ 0, 0 }).Y;
            VERIFY_IS_TRUE(firstContainer.ActualHeight() > 0);
            VERIFY_IS_TRUE(secondTop > firstTop);
            VERIFY_IS_TRUE(thirdTop > secondTop);

            secondContainer.Visibility(Visibility::Collapsed);
            host.UpdateLayout();

            VERIFY_IS_TRUE(strip.SelectedItem() == second);
            VERIFY_IS_TRUE(stripImpl->ItemsList().SelectedItem().as<winrt::TerminalApp::TabStripDisplayItem>().Tab() == second);
            VERIFY_ARE_EQUAL(1, strip.SelectedIndex());
            VERIFY_IS_TRUE(stripImpl->ItemsList().Items().GetAt(strip.SelectedIndex()).as<winrt::TerminalApp::TabStripDisplayItem>().Tab() == second);
            VERIFY_ARE_EQUAL(0u, selectionChanges);
            const auto collapsedThirdTop = thirdContainer.TransformToVisual(strip).TransformPoint({ 0, 0 }).Y;
            VERIFY_IS_TRUE(collapsedThirdTop < thirdTop);
            VERIFY_IS_TRUE(collapsedThirdTop <= firstTop + firstContainer.ActualHeight() + 1.0);
        });

        // Run on a later dispatcher turn to prove layout processing does not
        // clear a selected item solely because its row is collapsed.
        TestOnUIThread([&]() {
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            host.UpdateLayout();
            VERIFY_IS_TRUE(strip.SelectedItem() == second);
            VERIFY_IS_TRUE(stripImpl->ItemsList().SelectedItem().as<winrt::TerminalApp::TabStripDisplayItem>().Tab() == second);

            // The existing tab commands can select a filtered-out tab. This
            // must remain a stable operation without an asynchronous repair.
            strip.SelectedItem(first);
            const auto firstContainer = strip.ContainerFromIndex(0).as<ListViewItem>();
            const auto thirdContainer = strip.ContainerFromIndex(2).as<ListViewItem>();
            firstContainer.Visibility(Visibility::Collapsed);
            thirdContainer.Visibility(Visibility::Collapsed);
            host.UpdateLayout();
            strip.SelectedItem(third);
            host.UpdateLayout();

            VERIFY_IS_TRUE(strip.SelectedItem() == third);
            VERIFY_IS_TRUE(stripImpl->ItemsList().SelectedItem().as<winrt::TerminalApp::TabStripDisplayItem>().Tab() == third);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, strip.ContainerFromIndex(0).as<ListViewItem>().Visibility());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, strip.ContainerFromIndex(1).as<ListViewItem>().Visibility());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, strip.ContainerFromIndex(2).as<ListViewItem>().Visibility());
        });

        TestOnUIThread([&]() {
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            host.UpdateLayout();
            VERIFY_IS_TRUE(strip.SelectedItem() == third);
            VERIFY_IS_TRUE(stripImpl->ItemsList().SelectedItem().as<winrt::TerminalApp::TabStripDisplayItem>().Tab() == third);
        });
    }

    void TabTests::VerticalTabStripHostsPaneGroups()
    {
        winrt::TerminalApp::TabStrip strip;
        Grid host;
        winrt::MUX::Controls::TabViewItem tab;

        TestOnUIThread([&]() {
            host.Width(240);
            host.Height(200);
            strip.Width(240);
            strip.Height(200);
            winrt::TerminalApp::TabHeaderControl header;
            header.Title(L"Split tab");
            header.MetadataText(L"parent metadata");
            header.IsMetadataVisible(true);
            tab.Header(header);
            winrt::MUX::Controls::SymbolIconSource icon;
            icon.Symbol(winrt::Windows::UI::Xaml::Controls::Symbol::Document);
            tab.IconSource(icon);
            MenuFlyout contextFlyout;
            tab.ContextFlyout(contextFlyout);
            strip.TabItems().Append(tab);
            strip.SetTabPresentation(tab, L"Split tab", L"\xE8A5");

            std::vector<winrt::TerminalApp::TabStripPaneItem> panes;
            auto firstPane = winrt::make<winrt::TerminalApp::implementation::TabStripPaneItem>(tab, 11, L"First pane", true);
            const auto firstPaneImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStripPaneItem>(firstPane);
            const winrt::hstring firstPaneMetadata{ L"first metadata\n"
                                                    L"first branch" };
            firstPaneImpl->MetadataText(firstPaneMetadata);
            firstPaneImpl->MetadataVisibility(Visibility::Visible);
            firstPaneImpl->AutomationName(L"First pane, first metadata, first branch");
            panes.emplace_back(std::move(firstPane));
            panes.emplace_back(winrt::make<winrt::TerminalApp::implementation::TabStripPaneItem>(tab, 12, L"Second pane", false));
            strip.SetPaneItems(tab, winrt::single_threaded_vector<winrt::TerminalApp::TabStripPaneItem>(std::move(panes)), true);

            host.Children().Append(strip);
            Window::Current().Content(host);
            Window::Current().Activate();
            host.UpdateLayout();

            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            const auto container = strip.ContainerFromIndex(0).as<ListViewItem>();
            const auto display = stripImpl->ItemsList().ItemFromContainer(container).as<winrt::TerminalApp::TabStripDisplayItem>();
            VERIFY_IS_TRUE(display.Tab() == tab);
            VERIFY_IS_NULL(tab.Header());
            VERIFY_IS_TRUE(display.Header() == header);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Split tab" }, display.Title());
            VERIFY_IS_NOT_NULL(display.Icon());
            VERIFY_IS_TRUE(display.ContextFlyout() == contextFlyout);
            VERIFY_ARE_EQUAL(2u, display.PaneItems().Size());
            VERIFY_ARE_EQUAL(Visibility::Visible, display.ChildrenVisibility());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, display.IconVisibility());
            VERIFY_ARE_EQUAL(40.0, display.HeaderMinHeight());
            VERIFY_IS_FALSE(header.IsMetadataVisible());
            VERIFY_ARE_EQUAL(firstPaneMetadata, display.PaneItems().GetAt(0).MetadataText());
            VERIFY_ARE_EQUAL(Visibility::Visible, display.PaneItems().GetAt(0).MetadataVisibility());
            VERIFY_IS_TRUE(container.ActualHeight() > 32.0);
            VERIFY_IS_NOT_NULL(header.Parent());

            const auto originalIcon = display.Icon();
            const auto firstPaneItem = display.PaneItems().GetAt(0);
            const auto secondPaneItem = display.PaneItems().GetAt(1);
            const auto templateRoot = container.ContentTemplateRoot().as<StackPanel>();
            const auto headerRoot = templateRoot.Children().GetAt(0).as<Grid>();
            const auto iconPresenter = headerRoot.FindName(L"TabIconPresenter").as<ContentPresenter>();
            const auto headerPresenter = headerRoot.FindName(L"TabHeaderPresenter").as<ContentPresenter>();
            VERIFY_ARE_EQUAL(Visibility::Collapsed, iconPresenter.Visibility());
            VERIFY_ARE_EQUAL(40.0, headerRoot.ActualHeight());
            const auto groupTitleOffset = headerPresenter.TransformToVisual(headerRoot).TransformPoint({ 0, 0 }).X;
            const auto paneList = templateRoot.Children().GetAt(1).as<ItemsControl>();
            const auto firstPaneContainer = paneList.ContainerFromIndex(0).as<ContentPresenter>();
            const auto firstPaneRoot = Media::VisualTreeHelper::GetChild(firstPaneContainer, 0).as<Grid>();
            const auto firstPaneBackground = firstPaneRoot.FindName(L"PaneActiveBackground").as<Border>();
            const auto firstPaneActivateButton = firstPaneRoot.FindName(L"PaneActivateButton").as<Button>();
            VERIFY_ARE_EQUAL(Visibility::Visible, firstPaneBackground.Visibility());
            VERIFY_ARE_EQUAL(firstPaneRoot.ActualWidth(), firstPaneBackground.ActualWidth());
            VERIFY_ARE_EQUAL(firstPaneRoot.ActualWidth(), firstPaneActivateButton.ActualWidth());
            uint32_t collectionChanges = 0;
            const auto changed = display.PaneItems().VectorChanged(winrt::auto_revoke, [&](auto&&, auto&&) {
                ++collectionChanges;
            });
            const auto updatePanes = [&](std::vector<winrt::TerminalApp::TabStripPaneItem> values) {
                strip.SetPaneItems(tab, winrt::single_threaded_vector<winrt::TerminalApp::TabStripPaneItem>(std::move(values)), true);
            };
            auto updatedFirstPane = winrt::make<winrt::TerminalApp::implementation::TabStripPaneItem>(tab, 11, L"Renamed pane", false);
            const auto updatedFirstPaneImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStripPaneItem>(updatedFirstPane);
            updatedFirstPaneImpl->MetadataText(L"updated metadata");
            updatedFirstPaneImpl->MetadataVisibility(Visibility::Visible);
            updatedFirstPaneImpl->AutomationName(L"Renamed pane, updated metadata");
            updatePanes({ updatedFirstPane,
                          winrt::make<winrt::TerminalApp::implementation::TabStripPaneItem>(tab, 12, L"Second pane", true) });
            strip.SetTabPresentation(tab, L"Renamed tab", L"\xE8A5");
            host.UpdateLayout();
            VERIFY_ARE_EQUAL(0u, collectionChanges);
            VERIFY_IS_TRUE(display.Icon() == originalIcon);
            VERIFY_IS_TRUE(display.PaneItems().GetAt(0) == firstPaneItem);
            VERIFY_IS_TRUE(display.PaneItems().GetAt(1) == secondPaneItem);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Renamed pane" }, firstPaneItem.Title());
            VERIFY_IS_FALSE(firstPaneItem.IsActive());
            VERIFY_IS_TRUE(secondPaneItem.IsActive());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, firstPaneItem.ActiveIndicatorVisibility());
            VERIFY_ARE_EQUAL(Visibility::Visible, secondPaneItem.ActiveIndicatorVisibility());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, firstPaneBackground.Visibility());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"updated metadata" }, firstPaneItem.MetadataText());
            VERIFY_ARE_EQUAL(Visibility::Visible, firstPaneItem.MetadataVisibility());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Renamed pane, updated metadata" }, firstPaneItem.AutomationName());
            VERIFY_IS_TRUE(paneList.ContainerFromIndex(0) == firstPaneContainer);
            const auto firstPaneTitle = firstPaneRoot.FindName(L"PaneTitleText").as<winrt::TerminalApp::HighlightedTextControl>();
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Renamed pane" }, firstPaneTitle.Text());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, firstPaneRoot.FindName(L"PaneActiveIndicator").as<FrameworkElement>().Visibility());

            stripImpl->SetTabSearchText(tab, L"pane");
            VERIFY_ARE_EQUAL(winrt::hstring{ L"pane" }, display.SearchText());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"pane" }, firstPaneItem.HighlightQuery());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"pane" }, secondPaneItem.HighlightQuery());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"pane" }, firstPaneTitle.SearchText());

            updatePanes({ winrt::make<winrt::TerminalApp::implementation::TabStripPaneItem>(tab, 11, L"Renamed pane", false),
                          winrt::make<winrt::TerminalApp::implementation::TabStripPaneItem>(tab, 12, L"Second pane", true) });
            VERIFY_ARE_EQUAL(winrt::hstring{ L"pane" }, display.PaneItems().GetAt(0).HighlightQuery());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"pane" }, display.PaneItems().GetAt(1).HighlightQuery());
            VERIFY_ARE_EQUAL(winrt::hstring{}, firstPaneItem.MetadataText());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, firstPaneItem.MetadataVisibility());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Renamed pane" }, firstPaneItem.AutomationName());
            VERIFY_ARE_EQUAL(0u, collectionChanges);
            VERIFY_IS_TRUE(paneList.ContainerFromIndex(0) == firstPaneContainer);

            updatePanes({ secondPaneItem, firstPaneItem });
            VERIFY_IS_TRUE(display.PaneItems().GetAt(0) == secondPaneItem);
            VERIFY_IS_TRUE(display.PaneItems().GetAt(1) == firstPaneItem);
            updatePanes({ firstPaneItem });
            VERIFY_ARE_EQUAL(1u, display.PaneItems().Size());
            VERIFY_IS_TRUE(display.PaneItems().GetAt(0) == firstPaneItem);
            updatePanes({ firstPaneItem, secondPaneItem });
            strip.SetTabPresentation(tab, L"Renamed tab", L"");
            const auto fallbackIcon = display.Icon();
            VERIFY_IS_FALSE(fallbackIcon == originalIcon);
            strip.SetTabPresentation(tab, L"Renamed tab", L"");
            VERIFY_IS_TRUE(display.Icon() == fallbackIcon);
            strip.SetTabPresentation(tab, L"Renamed tab", L"\xE8A5");
            VERIFY_IS_FALSE(display.Icon() == fallbackIcon);
            host.UpdateLayout();
            VERIFY_IS_TRUE(container.ContentTemplateRoot().as<FrameworkElement>().FindName(L"TabIconPresenter").as<ContentPresenter>().Content() == display.Icon());

            strip.SetPaneItems(tab, display.PaneItems(), false);
            VERIFY_IS_TRUE(header.IsMetadataVisible());
            VERIFY_ARE_EQUAL(Visibility::Visible, display.IconVisibility());
            host.UpdateLayout();
            VERIFY_IS_TRUE(headerRoot.ActualHeight() > display.HeaderMinHeight());
            VERIFY_ARE_EQUAL(groupTitleOffset, headerPresenter.TransformToVisual(headerRoot).TransformPoint({ 0, 0 }).X);
            strip.SetPaneItems(tab, display.PaneItems(), true);
            VERIFY_IS_FALSE(header.IsMetadataVisible());
            VERIFY_ARE_EQUAL(Visibility::Collapsed, display.IconVisibility());
            host.UpdateLayout();
            VERIFY_ARE_EQUAL(display.HeaderMinHeight(), headerRoot.ActualHeight());

            header.BeginRename();
            VERIFY_IS_TRUE(header.InRename());
            header.CancelRename();
            VERIFY_IS_FALSE(header.InRename());

            strip.SelectedItem(tab);
            VERIFY_IS_TRUE(strip.SelectedItem() == tab);
            VERIFY_IS_TRUE(stripImpl->ItemsList().SelectedItem().as<winrt::TerminalApp::TabStripDisplayItem>().Tab() == tab);
            VERIFY_ARE_EQUAL(Visibility::Visible, display.SelectionVisibility());
            host.UpdateLayout();
            const auto selectionBackground = headerRoot.FindName(L"TabSelectionBackground").as<Border>();
            VERIFY_ARE_EQUAL(Visibility::Visible, selectionBackground.Visibility());
            VERIFY_ARE_EQUAL(headerRoot.ActualHeight(), selectionBackground.ActualHeight());
            VERIFY_IS_TRUE(container.ActualHeight() > selectionBackground.ActualHeight());

            strip.IsRailCollapsed(true);
            host.UpdateLayout();
            VERIFY_ARE_EQUAL(Visibility::Collapsed, display.ChildrenVisibility());
            VERIFY_ARE_EQUAL(Visibility::Visible, display.IconVisibility());
            VERIFY_ARE_EQUAL(32.0, display.HeaderMinHeight());
            VERIFY_IS_TRUE(container.ActualHeight() <= 40.0);

            winrt::MUX::Controls::TabViewItem secondTab;
            winrt::TerminalApp::TabHeaderControl secondHeader;
            secondHeader.Title(L"Second tab");
            secondTab.Header(secondHeader);
            strip.TabItems().Append(secondTab);
            stripImpl->MoveTabItem(0, 1);
            const auto movedDisplay = stripImpl->ItemsList().Items().GetAt(1).as<winrt::TerminalApp::TabStripDisplayItem>();
            VERIFY_IS_TRUE(movedDisplay == display);
            VERIFY_IS_TRUE(movedDisplay.Header() == header);
            VERIFY_ARE_EQUAL(2u, movedDisplay.PaneItems().Size());
            VERIFY_IS_NOT_NULL(header.Parent());

            strip.SetTabItemVisibility(tab, false);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, strip.ContainerFromIndex(1).as<ListViewItem>().Visibility());
            strip.TabItems().RemoveAt(1);
            strip.TabItems().Append(tab);
            host.UpdateLayout();
            VERIFY_ARE_EQUAL(Visibility::Visible, strip.ContainerFromIndex(1).as<ListViewItem>().Visibility());

            strip.TabItems().Clear();
            VERIFY_IS_TRUE(tab.Header() == header);
        });
    }

    void TabTests::VerticalTabSelectionPreservesPresentation()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            const auto firstTab = page->_GetFocusedTabImpl();
            const auto pane = page->_MakePane(nullptr, nullptr, nullptr);
            VERIFY_IS_TRUE(page->_SplitPane(firstTab, SplitDirection::Right, 0.5f, pane));
            NewTerminalArgs args;
            VERIFY_SUCCEEDED(page->_OpenNewTab(args));
            const auto secondTab = page->_GetFocusedTabImpl();
            page->_settings.GlobalSettings().UseAcrylicInTabRow(true);
            page->WindowActivated(true);
            page->_ApplyTabListProjection();
            page->UpdateLayout();

            const auto strip = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            const auto display = strip->ItemsList().Items().GetAt(0).as<winrt::TerminalApp::TabStripDisplayItem>();
            const auto icon = display.Icon();
            const auto firstPane = display.PaneItems().GetAt(0);
            const auto secondPane = display.PaneItems().GetAt(1);
            const auto backdrop = page->TitlebarBrush();
            VERIFY_IS_NOT_NULL(backdrop.try_as<Media::AcrylicBrush>());
            uint32_t collectionChanges = 0;
            const auto changed = display.PaneItems().VectorChanged(winrt::auto_revoke, [&](auto&&, auto&&) {
                ++collectionChanges;
            });

            for (auto iteration = 0; iteration < 3; ++iteration)
            {
                page->_tabStrip.SelectedItem(firstTab->TabViewItem());
                page->_tabStrip.SelectedItem(secondTab->TabViewItem());
                page->UpdateLayout();
                VERIFY_IS_TRUE(display.Icon() == icon);
                VERIFY_IS_TRUE(display.PaneItems().GetAt(0) == firstPane);
                VERIFY_IS_TRUE(display.PaneItems().GetAt(1) == secondPane);
                VERIFY_IS_TRUE(page->TitlebarBrush() == backdrop);
            }
            VERIFY_ARE_EQUAL(0u, collectionChanges);
            const auto headerRoot = page->_tabStrip.ContainerFromIndex(0).as<ListViewItem>().ContentTemplateRoot().as<StackPanel>().Children().GetAt(0).as<Grid>();
            const auto normalBackground = headerRoot.Background();
            VERIFY_SUCCEEDED(page->_OpenNewTab(args));
            VERIFY_ARE_EQUAL(0u, collectionChanges);
            VERIFY_IS_TRUE(headerRoot.Background() == normalBackground);
            VERIFY_IS_TRUE(display.Icon() == icon);
            VERIFY_IS_TRUE(display.PaneItems().GetAt(0) == firstPane);
            VERIFY_IS_TRUE(display.PaneItems().GetAt(1) == secondPane);
        });
    }

    void TabTests::VerticalTabIconChangesUpdatePresentation()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            const auto title = tab->Title();
            const auto strip = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            const auto display = strip->ItemsList().Items().GetAt(0).as<winrt::TerminalApp::TabStripDisplayItem>();
            const auto pane = display.PaneItems().GetAt(0);
            uint32_t collectionChanges = 0;
            const auto changed = display.PaneItems().VectorChanged(winrt::auto_revoke, [&](auto&&, auto&&) {
                ++collectionChanges;
            });
            NewTerminalArgs args;
            VERIFY_SUCCEEDED(page->_OpenNewTab(args));
            VERIFY_IS_FALSE(page->_GetFocusedTabImpl() == tab);

            // Settings reload calls UpdateIcon even when the tab title is unchanged.
            for (const auto glyph : { L"\xE8A5", L"\xE756" })
            {
                const auto previousIcon = display.Icon();
                tab->UpdateIcon(glyph, IconStyle::Default);
                page->UpdateLayout();
                VERIFY_ARE_EQUAL(title, tab->Title());
                VERIFY_IS_FALSE(previousIcon == display.Icon());
                VERIFY_ARE_EQUAL(winrt::hstring{ glyph }, display.Icon().as<FontIcon>().Glyph());
                const auto root = page->_tabStrip.ContainerFromIndex(0).as<ListViewItem>().ContentTemplateRoot().as<FrameworkElement>();
                VERIFY_IS_TRUE(root.FindName(L"TabIconPresenter").as<ContentPresenter>().Content() == display.Icon());
                const auto updatedIcon = display.Icon();
                tab->UpdateIcon(glyph, IconStyle::Default);
                VERIFY_IS_TRUE(display.Icon() == updatedIcon);
                VERIFY_IS_TRUE(root.FindName(L"TabIconPresenter").as<ContentPresenter>().Content() == display.Icon());
                VERIFY_IS_TRUE(display.PaneItems().GetAt(0) == pane);
            }
            VERIFY_ARE_EQUAL(0u, collectionChanges);
        });
    }

    void TabTests::VerticalTabThemeChangesDoNotReprojectPanes()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            uint32_t paneChanges = 0;
            uint32_t colorChanges = 0;
            const auto paneToken = tab->PaneProjectionChanged([&]() { ++paneChanges; });
            const auto colorToken = tab->TabColorChanged([&]() { ++colorChanges; });
            const auto revoke = wil::scope_exit([&]() {
                tab->PaneProjectionChanged(paneToken);
                tab->TabColorChanged(colorToken);
            });

            const auto background = ThemeColor::FromTerminalBackground();
            for (auto iteration = 0; iteration < 3; ++iteration)
            {
                tab->ThemeColor(background, nullptr, til::color{});
            }
            VERIFY_ARE_EQUAL(0u, paneChanges);
            VERIFY_ARE_EQUAL(3u, colorChanges);

            tab->SetRuntimeTabColor(winrt::Windows::UI::Colors::Red());
            tab->ResetRuntimeTabColor();
            VERIFY_ARE_EQUAL(0u, paneChanges);
            VERIFY_ARE_EQUAL(5u, colorChanges);

            const auto pane = page->_MakePane(nullptr, nullptr, nullptr);
            VERIFY_IS_TRUE(page->_SplitPane(tab, SplitDirection::Right, 0.5f, pane));
            VERIFY_IS_TRUE(paneChanges > 0);
            const auto strip = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            const auto display = strip->ItemsList().Items().GetAt(0).as<winrt::TerminalApp::TabStripDisplayItem>();
            VERIFY_ARE_EQUAL(2u, display.PaneItems().Size());
        });
    }

    void TabTests::VerticalTabColorsFollowSidebarTheme()
    {
        const CascadiaSettings settings{ LR"({
            "defaultProfile": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
            "showTabsInTitlebar": false,
            "tabLayout": "vertical",
            "theme": "sidebar",
            "themes": [{
                "name": "sidebar",
                "window": { "applicationTheme": "light" },
                "tab": { "background": "terminalBackground", "unfocusedBackground": "#00000000" }
            }],
            "profiles": [
                { "name": "default", "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}", "background": "#111111", "closeOnExit": "never" },
                { "name": "colored", "guid": "{6239a42c-2222-49a3-80bd-e8fdd045185c}", "tabColor": "#FF0000", "closeOnExit": "never" }
            ]
        })",
                                         {} };
        winrt::com_ptr<winrt::TerminalApp::implementation::TerminalPage> page;
        _initializeTerminalPage(page, settings);

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            const auto terminalColor = ThemeColor::ColorFromBrush(tab->_BackgroundBrush());
            const auto headerGrid = [&](const uint32_t index) {
                page->UpdateLayout();
                return page->_tabStrip.ContainerFromIndex(index).as<ListViewItem>().ContentTemplateRoot().as<StackPanel>().Children().GetAt(0).as<Grid>();
            };
            const auto sidebarTabColor = [&](const uint32_t index) {
                return headerGrid(index).FindName(L"TabColorSelectionBackground").as<Border>().Background().as<Media::SolidColorBrush>().Color();
            };
            const auto verifyNativeBackground = [&]() {
                const auto grid = headerGrid(0);
                VERIFY_ARE_EQUAL(uint8_t{ 0 }, grid.Background().as<Media::SolidColorBrush>().Color().A);
                VERIFY_ARE_EQUAL(uint8_t{ 0 }, sidebarTabColor(0).A);
                VERIFY_ARE_EQUAL(Visibility::Visible, grid.FindName(L"TabSelectionBackground").as<Border>().Visibility());
                const auto header = grid.FindName(L"TabHeaderPresenter").as<ContentPresenter>().Content().as<Control>();
                VERIFY_IS_TRUE(header.ReadLocalValue(Control::ForegroundProperty()) == DependencyProperty::UnsetValue());
                VERIFY_ARE_EQUAL(terminalColor, ThemeColor::ColorFromBrush(tab->_BackgroundBrush()));
            };
            const auto selectedTabColor = [](const winrt::MUX::Controls::TabViewItem& item) {
                const auto resources = item.Resources().ThemeDictionaries().Lookup(winrt::box_value(L"Light")).as<ResourceDictionary>();
                return resources.Lookup(winrt::box_value(L"TabViewItemHeaderBackgroundSelected")).as<Media::SolidColorBrush>().Color();
            };

            for (const auto theme : { ElementTheme::Light, ElementTheme::Dark })
            {
                page->RequestedTheme(theme);
                tab->ThemeColor(ThemeColor::FromTerminalBackground(), nullptr, til::color{});
                VERIFY_ARE_EQUAL(theme, headerGrid(0).ActualTheme());
                verifyNativeBackground();
                for (const auto color : { winrt::Windows::UI::Colors::Black(), winrt::Windows::UI::Colors::White() })
                {
                    tab->SetRuntimeTabColor(color);
                    VERIFY_ARE_EQUAL(color, sidebarTabColor(0));
                    tab->ResetRuntimeTabColor();
                    verifyNativeBackground();
                }
            }

            VERIFY_IS_TRUE(page->_ApplyTabLayout(TabLayout::Horizontal));
            page->_CompleteTabLayoutChange(page->_tabLayoutGeneration);
            VERIFY_ARE_EQUAL(terminalColor, til::color{ selectedTabColor(tab->TabViewItem()) });
            VERIFY_IS_TRUE(page->_ApplyTabLayout(TabLayout::Vertical));
            page->_CompleteTabLayoutChange(page->_tabLayoutGeneration);
            verifyNativeBackground();

            NewTerminalArgs args;
            args.Profile(L"colored");
            VERIFY_SUCCEEDED(page->_OpenNewTab(args));
            const auto coloredTab = page->_GetFocusedTabImpl();
            VERIFY_ARE_EQUAL(winrt::Windows::UI::Colors::Red(), sidebarTabColor(1));
            coloredTab->SetRuntimeTabColor(winrt::Windows::UI::Colors::Blue());
            VERIFY_ARE_EQUAL(winrt::Windows::UI::Colors::Blue(), sidebarTabColor(1));
            coloredTab->ResetRuntimeTabColor();
            VERIFY_ARE_EQUAL(winrt::Windows::UI::Colors::Red(), sidebarTabColor(1));
            VERIFY_IS_TRUE(page->_ApplyTabLayout(TabLayout::Horizontal));
            page->_CompleteTabLayoutChange(page->_tabLayoutGeneration);
            VERIFY_ARE_EQUAL(winrt::Windows::UI::Colors::Red(), selectedTabColor(coloredTab->TabViewItem()));
            VERIFY_IS_TRUE(page->_ApplyTabLayout(TabLayout::Vertical));
            page->_CompleteTabLayoutChange(page->_tabLayoutGeneration);
            VERIFY_ARE_EQUAL(winrt::Windows::UI::Colors::Red(), sidebarTabColor(1));
        });
    }

    void TabTests::VerticalTabStripUsesNativeInteractionStates()
    {
        TestOnUIThread([&]() {
            winrt::TerminalApp::TabStrip strip;
            strip.Width(240);
            strip.Height(200);
            winrt::MUX::Controls::TabViewItem tab;
            winrt::TerminalApp::TabHeaderControl header;
            header.Title(L"Native interaction states");
            tab.Header(header);
            Media::SolidColorBrush tabBrush{ winrt::Windows::UI::Colors::Black() };
            tabBrush.Opacity(0.3);
            tab.Background(tabBrush);
            strip.TabItems().Append(tab);
            Window::Current().Content(strip);
            Window::Current().Activate();
            strip.UpdateLayout();

            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(strip);
            for (const auto key : {
                     L"ListViewItemBackgroundPointerOver",
                     L"ListViewItemBackgroundPressed",
                     L"ListViewItemBackgroundSelected",
                     L"ListViewItemBackgroundSelectedPointerOver",
                     L"ListViewItemBackgroundSelectedPressed" })
            {
                VERIFY_IS_FALSE(impl->ItemsList().Resources().HasKey(winrt::box_value(key)));
            }

            const auto container = strip.ContainerFromIndex(0).as<ListViewItem>();
            const auto root = container.ContentTemplateRoot().as<StackPanel>();
            VERIFY_ARE_EQUAL(uint8_t{ 0 }, container.Background().as<Media::SolidColorBrush>().Color().A);
            const auto headerGrid = root.Children().GetAt(0).as<Grid>();
            const auto selectionBackground = headerGrid.FindName(L"TabSelectionBackground").as<Border>();
            const auto colorSelectionBackground = headerGrid.FindName(L"TabColorSelectionBackground").as<Border>();
            VERIFY_IS_NOT_NULL(selectionBackground);
            VERIFY_IS_NOT_NULL(colorSelectionBackground);
            VERIFY_ARE_EQUAL(CornerRadiusHelper::FromUniformRadius(6), headerGrid.CornerRadius());
            strip.SelectedItem(nullptr);
            VERIFY_ARE_EQUAL(uint8_t{ 0 }, headerGrid.Background().as<Media::SolidColorBrush>().Color().A);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, selectionBackground.Visibility());
            VERIFY_IS_TRUE(header.ReadLocalValue(Control::ForegroundProperty()) == DependencyProperty::UnsetValue());

            for (const auto color : { winrt::Windows::UI::Colors::Black(), winrt::Windows::UI::Colors::White() })
            {
                tabBrush.Color(color);
                strip.SetTabPresentation(tab, header.Title(), L"");
                VERIFY_ARE_EQUAL(uint8_t{ 0 }, colorSelectionBackground.Background().as<Media::SolidColorBrush>().Color().A);
                strip.SelectedItem(tab);
                VERIFY_ARE_EQUAL(Visibility::Visible, selectionBackground.Visibility());
                VERIFY_ARE_EQUAL(color, colorSelectionBackground.Background().as<Media::SolidColorBrush>().Color());
                strip.SelectedItem(nullptr);
                VERIFY_ARE_EQUAL(Visibility::Collapsed, selectionBackground.Visibility());
                VERIFY_ARE_EQUAL(uint8_t{ 0 }, colorSelectionBackground.Background().as<Media::SolidColorBrush>().Color().A);
                VERIFY_IS_TRUE(header.ReadLocalValue(Control::ForegroundProperty()) == DependencyProperty::UnsetValue());
            }

            const auto findPresenter = [](const auto& self, const DependencyObject& element) -> Primitives::ListViewItemPresenter {
                if (const auto presenter = element.try_as<Primitives::ListViewItemPresenter>())
                {
                    return presenter;
                }
                for (auto index = 0; index < Media::VisualTreeHelper::GetChildrenCount(element); ++index)
                {
                    if (const auto presenter = self(self, Media::VisualTreeHelper::GetChild(element, index)))
                    {
                        return presenter;
                    }
                }
                return nullptr;
            };
            const auto presenter = findPresenter(findPresenter, container);
            VERIFY_IS_NOT_NULL(presenter);
            VERIFY_IS_NOT_NULL(presenter.Content());
            VERIFY_IS_TRUE(presenter.Content() == container.Content());
            VERIFY_IS_NOT_NULL(presenter.ContentTemplate());
            VERIFY_IS_TRUE(presenter.ContentTemplate() == impl->ItemsList().ItemTemplate());
            VERIFY_ARE_EQUAL(container.CornerRadius(), presenter.CornerRadius());
            for (const auto brush : { presenter.PointerOverBackground(), presenter.PressedBackground() })
            {
                VERIFY_IS_TRUE(brush.as<Media::SolidColorBrush>().Color().A > 0);
            }
            for (const auto brush : {
                     presenter.SelectedBackground(),
                     presenter.SelectedPointerOverBackground(),
                     presenter.SelectedPressedBackground() })
            {
                VERIFY_ARE_EQUAL(uint8_t{ 0 }, brush.as<Media::SolidColorBrush>().Color().A);
            }
            for (const auto state : { L"Normal", L"PointerOver", L"Pressed", L"Selected", L"PointerOverSelected", L"PressedSelected", L"Normal" })
            {
                VERIFY_IS_TRUE(VisualStateManager::GoToState(container, state, false));
                VERIFY_ARE_EQUAL(CornerRadiusHelper::FromUniformRadius(6), presenter.CornerRadius());
            }
        });
    }

    void TabTests::WindowActivationToleratesTabWithoutStatus()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);

            const auto status = tab->_tabStatus;
            tab->_tabStatus = nullptr;
            page->WindowActivated(true);
            tab->_tabStatus = status;
        });
    }

    void TabTests::AgentViewFiltersSplitPaneChildren()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);

            const auto agentPane = page->_MakePane(nullptr, nullptr, nullptr);
            VERIFY_IS_NOT_NULL(agentPane);
            VERIFY_IS_FALSE(agentPane->IsAgentPane());
            VERIFY_IS_NULL(agentPane->GetContent().try_as<winrt::TerminalApp::AgentPaneContent>());
            const auto agentPaneSessionId = agentPane->GetSessionId();
            VERIFY_IS_TRUE(agentPaneSessionId != winrt::guid{});
            VERIFY_IS_TRUE(page->_SplitPane(tab, SplitDirection::Right, 0.5f, agentPane));
            VERIFY_IS_TRUE(agentPane->ContentId().has_value());

            page->_RefreshTabStripPaneItems(tab);
            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            const auto display = stripImpl->ItemsList().Items().GetAt(0).as<winrt::TerminalApp::TabStripDisplayItem>();
            VERIFY_ARE_EQUAL(2u, display.PaneItems().Size());

            page->_tabFilterMode = winrt::TerminalApp::TabStripFilterMode::AgentsOnly;
            page->_ApplyTabListProjection();
            VERIFY_ARE_EQUAL(0u, display.PaneItems().Size());

            Json::Value event;
            event["params"]["pane_id"] =
                winrt::to_string(::Microsoft::Console::Utils::GuidToString(agentPaneSessionId));
            event["params"]["event"] = "agent.session.start";
            event["params"]["agent"] = "copilot";
            Json::StreamWriterBuilder writer;
            writer["indentation"] = "";
            page->OnPaneAgentSessionChanged(winrt::to_hstring(Json::writeString(writer, event)));

            VERIFY_IS_TRUE(page->_activeCliAgentPanes.contains(agentPaneSessionId));
            VERIFY_IS_FALSE(page->_paneAgentSessions.contains(agentPaneSessionId));
            VERIFY_ARE_EQUAL(1u, display.PaneItems().Size());
            VERIFY_ARE_EQUAL(agentPane->ContentId().value(), display.PaneItems().GetAt(0).ContentId());
            VERIFY_IS_TRUE(display.IsGroup());
            VERIFY_ARE_EQUAL(Visibility::Visible, display.GroupVisibility());
            VERIFY_ARE_EQUAL(Visibility::Visible, display.ChildrenVisibility());

            page->_tabFilterMode = winrt::TerminalApp::TabStripFilterMode::AllTabs;
            page->_ApplyTabListProjection();
            VERIFY_ARE_EQUAL(2u, display.PaneItems().Size());
        });
    }

    void TabTests::VerticalTabGroupingIgnoresAgentPane()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);

            const auto sourcePane = tab->GetActivePane();
            VERIFY_IS_NOT_NULL(sourcePane);
            VERIFY_IS_FALSE(sourcePane->IsAgentPane());
            VERIFY_IS_TRUE(sourcePane->ContentId().has_value());

            const auto agentPane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            VERIFY_IS_NOT_NULL(agentPane);
            agentPane->IsAgentPane(true);
            VERIFY_IS_TRUE(page->_SplitPane(tab, SplitDirection::Right, 0.5f, agentPane));
            VERIFY_IS_TRUE(tab->GetActivePane() == agentPane);

            const auto stripImpl = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            const auto display = stripImpl->ItemsList().Items().GetAt(0).as<winrt::TerminalApp::TabStripDisplayItem>();
            const auto verifySingleSourcePane = [&]() {
                page->_RefreshTabStripPaneItems(tab);
                VERIFY_ARE_EQUAL(1u, display.PaneItems().Size());
                VERIFY_ARE_EQUAL(sourcePane->ContentId().value(), display.PaneItems().GetAt(0).ContentId());
                VERIFY_IS_TRUE(display.PaneItems().GetAt(0).IsActive());
                VERIFY_IS_FALSE(display.IsGroup());
            };

            verifySingleSourcePane();
            tab->StashAgentPane();
            VERIFY_IS_TRUE(tab->HasStashedAgentPane());
            verifySingleSourcePane();
            VERIFY_IS_TRUE(tab->RestoreStashedAgentPane(SplitDirection::Right));
            verifySingleSourcePane();

            VERIFY_IS_TRUE(sourcePane->Id().has_value());
            VERIFY_IS_TRUE(tab->FocusPane(sourcePane->Id().value()));
            const auto secondTerminalPane = page->_MakePane(nullptr, page->_GetFocusedTab(), nullptr);
            VERIFY_IS_TRUE(page->_SplitPane(tab, SplitDirection::Right, 0.5f, secondTerminalPane));
            VERIFY_IS_TRUE(secondTerminalPane->ContentId().has_value());
            VERIFY_IS_TRUE(agentPane->Id().has_value());
            VERIFY_IS_TRUE(tab->FocusPane(agentPane->Id().value()));

            page->_RefreshTabStripPaneItems(tab);
            VERIFY_ARE_EQUAL(2u, display.PaneItems().Size());
            VERIFY_IS_TRUE(display.IsGroup());
            for (const auto& item : display.PaneItems())
            {
                VERIFY_ARE_NOT_EQUAL(agentPane->ContentId().value(), item.ContentId());
                VERIFY_IS_TRUE(item.ContentId() == sourcePane->ContentId().value() ||
                               item.ContentId() == secondTerminalPane->ContentId().value());
                VERIFY_ARE_EQUAL(item.ContentId() == secondTerminalPane->ContentId().value(), item.IsActive());
            }
        });
    }

    void TabTests::AgentTabClassificationTracksSession()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);

            tab->SetAgentOverride(L"copilot", {}, {});
            const auto agentPane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            VERIFY_IS_NOT_NULL(agentPane);
            agentPane->IsAgentPane(true);
            page->_SplitPane(tab, SplitDirection::Right, 0.5f, agentPane);

            const auto content = agentPane->GetContent().as<winrt::TerminalApp::AgentPaneContent>();
            const auto contentImpl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(content);
            uint32_t stateChangeCount = 0;
            content.StateChanged([&](auto&&, auto&&) {
                ++stateChangeCount;
            });

            VERIFY_IS_TRUE(tab->IsAgentTab());

            tab->StashAgentPane();
            VERIFY_IS_FALSE(tab->IsAgentTab());

            contentImpl->SetAgentSessionId(L"copilot-session");
            VERIFY_ARE_EQUAL(1u, stateChangeCount);
            VERIFY_IS_TRUE(tab->IsAgentTab());

            contentImpl->SetAgentSessionId(L"copilot-session");
            VERIFY_ARE_EQUAL(1u, stateChangeCount);

            contentImpl->SetAgentSessionId({});
            VERIFY_ARE_EQUAL(2u, stateChangeCount);
            VERIFY_IS_FALSE(tab->IsAgentTab());
        });
    }

    void TabTests::CliAgentClassifiesTab()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);
            VERIFY_IS_TRUE(page->_IsKnownAgentCliTitle(L"GitHub Copilot"));
            VERIFY_IS_TRUE(page->_IsKnownAgentCliTitle(L"github copilot"));
            VERIFY_IS_FALSE(page->_IsKnownAgentCliTitle(L"PowerShell"));
            VERIFY_IS_TRUE(page->_MatchesPaneAgentScope(
                winrt::TerminalApp::implementation::Tab::VisiblePaneSnapshot{
                    .Title = L"GitHub Copilot",
                }));
            VERIFY_IS_FALSE(page->_MatchesPaneAgentScope(
                winrt::TerminalApp::implementation::Tab::VisiblePaneSnapshot{
                    .Title = L"PowerShell",
                }));
            VERIFY_IS_FALSE(page->_TabHasCliAgent(tab));

            const auto paneSessionId = tab->GetRootPane()->GetSessionId();
            VERIFY_IS_TRUE(paneSessionId != winrt::guid{});
            page->_paneAgentSessions.insert_or_assign(
                paneSessionId,
                winrt::TerminalApp::implementation::TerminalPage::_PaneAgentSession{
                    L"copilot-session",
                    L"copilot",
                    L"copilot --resume copilot-session" });

            VERIFY_IS_TRUE(page->_TabHasCliAgent(tab));
            page->_paneAgentSessions.erase(paneSessionId);
            VERIFY_IS_FALSE(page->_TabHasCliAgent(tab));
        });
    }

    void TabTests::VisibleFieldsControlRichTabComposition()
    {
        using namespace ::Microsoft::Terminal::RichTab::Provider;

        Registration provider;
        provider.manifest.id = "git";
        provider.manifest.fields = {
            { "agentStatus", "Agent status", FieldType::String, true },
            { "workingDirectory", "Current working directory", FieldType::String, true },
            { "repository", "Git repo", FieldType::String, false },
            { "branch", "Git branch", FieldType::String, false },
            { "changes", "Git changes", FieldType::String, false },
        };

        Snapshot snapshot;
        snapshot.fields.emplace("agentStatus", std::string{ "\xE6\xAD\xA3\xE5\x9C\xA8\xE5\xB7\xA5\xE4\xBD\x9C" });
        snapshot.fields.emplace("workingDirectory", std::string{ R"(C:\src\terminal)" });
        snapshot.fields.emplace("repository", std::string{ "\xE7\xBB\x88\xE7\xAB\xAF" });
        snapshot.fields.emplace("branch", std::string{ "\xE4\xB8\xBB\xE5\x88\x86\xE6\x94\xAF" });
        snapshot.fields.emplace("changes", std::string{ "~12 +200 -35" });
        const std::unordered_map<std::string, Snapshot> snapshots{ { "git", snapshot } };

        const auto defaults = ProviderBroker::ComposePresentation({ provider }, snapshots);
        VERIFY_IS_TRUE(defaults.has_value());
        VERIFY_ARE_EQUAL(
            std::wstring{ L"\u6B63\u5728\u5DE5\u4F5C\nC:\\src\\terminal" },
            defaults->text);
        VERIFY_ARE_EQUAL(
            std::wstring{ L"Agent status: \u6B63\u5728\u5DE5\u4F5C, Current working directory: C:\\src\\terminal" },
            defaults->accessibilityText);

        ProviderBroker::VisibleFieldMap visibleFields;
        visibleFields["git"].emplace("branch");
        visibleFields["git"].emplace("changes");
        const auto branchOnly = ProviderBroker::ComposePresentation({ provider }, snapshots, visibleFields);
        VERIFY_IS_TRUE(branchOnly.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"\u4E3B\u5206\u652F\n~12 +200 -35" }, branchOnly->text);
        VERIFY_ARE_EQUAL(
            std::wstring{ L"Git branch: \u4E3B\u5206\u652F, Git changes: ~12 +200 -35" },
            branchOnly->accessibilityText);

        visibleFields["git"].clear();
        VERIFY_IS_FALSE(ProviderBroker::ComposePresentation({ provider }, snapshots, visibleFields).has_value());
    }

    void TabTests::RichTabRequestIncludesFirstPartyFields()
    {
        using namespace ::Microsoft::Terminal::RichTab::Provider;

        Manifest manifest;
        manifest.id = "git";
        manifest.activationEvents.emplace_back(ActivationEvent::ManualRefresh);

        Request request;
        request.requestId = "request";
        request.providerId = manifest.id;
        request.processEpoch = 1;
        request.sessionId = "session";
        request.reason = ActivationEvent::ManualRefresh;
        request.firstPartyFields.emplace("agentStatus", "\xE6\xAD\xA3\xE5\x9C\xA8\xE5\xB7\xA5\xE4\xBD\x9C");

        const auto serialized = SerializeRequest(request, manifest);
        VERIFY_IS_TRUE(static_cast<bool>(serialized));
        VERIFY_IS_TRUE(serialized.value->find(R"("agentStatus":"\u6b63\u5728\u5de5\u4f5c")") != std::string::npos ||
                       serialized.value->find("\"agentStatus\":\"\xE6\xAD\xA3\xE5\x9C\xA8\xE5\xB7\xA5\xE4\xBD\x9C\"") != std::string::npos);
    }

    void TabTests::RichTabMetadataSelectionIsLimitedToTwo()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            auto& tabStrip = page->_tabStrip;
            VERIFY_IS_TRUE(tabStrip.RichTabAgentStatusVisible());
            VERIFY_IS_TRUE(tabStrip.RichTabWorkingDirectoryVisible());
            VERIFY_IS_FALSE(tabStrip.RichTabRepositoryVisible());
            VERIFY_IS_FALSE(tabStrip.RichTabBranchVisible());
            VERIFY_IS_FALSE(tabStrip.RichTabChangesVisible());

            tabStrip.RichTabRepositoryVisible(true);
            VERIFY_IS_FALSE(tabStrip.RichTabRepositoryVisible());

            tabStrip.RichTabWorkingDirectoryVisible(false);
            tabStrip.RichTabBranchVisible(true);
            VERIFY_IS_TRUE(tabStrip.RichTabAgentStatusVisible());
            VERIFY_IS_TRUE(tabStrip.RichTabBranchVisible());
            VERIFY_IS_FALSE(tabStrip.RichTabWorkingDirectoryVisible());
            VERIFY_IS_FALSE(tabStrip.RichTabRepositoryVisible());

            tabStrip.RichTabChangesVisible(true);
            VERIFY_IS_FALSE(tabStrip.RichTabChangesVisible());
        });
    }

    void TabTests::RichTabMetadataExpandsVerticalRow()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            page->_SetVerticalRailVisibility(true);
            const auto tab = page->_GetTabImpl(page->_tabs.GetAt(0));
            VERIFY_IS_NOT_NULL(tab);

            ::Microsoft::Terminal::RichTab::Provider::Presentation presentation;
            presentation.text = L"first line\nsecond line";
            presentation.tooltip = presentation.text;
            presentation.accessibilityText = L"First: first line, Second: second line";
            tab->SetRichTabPresentation(presentation);

            page->UpdateLayout();
            const auto tabStrip = winrt::get_self<winrt::TerminalApp::implementation::TabStrip>(page->_tabStrip);
            const auto container = tabStrip->ItemsList().ContainerFromIndex(0).try_as<ListViewItem>();
            VERIFY_IS_NOT_NULL(container);
            VERIFY_IS_TRUE(container.ActualHeight() > 48.0);
        });
    }

    void TabTests::RichTabMetadataIsVisibleOnlyInVerticalLayout()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto tab = page->_GetFocusedTabImpl();
            VERIFY_IS_NOT_NULL(tab);

            ::Microsoft::Terminal::RichTab::Provider::Presentation presentation;
            presentation.text = L"main\n2 changes";
            presentation.tooltip = L"main, 2 changes";
            presentation.accessibilityText = L"Branch: main, Changes: 2";
            tab->SetRichTabPresentation(presentation);

            const auto tooltipText = [&]() {
                const auto toolTip = ToolTipService::GetToolTip(tab->TabViewItem()).as<ToolTip>();
                const auto textBlock = toolTip.Content().as<TextBlock>();
                std::wstring text;
                for (const auto& inlineElement : textBlock.Inlines())
                {
                    if (const auto run = inlineElement.try_as<Documents::Run>())
                    {
                        text.append(run.Text());
                    }
                    else if (inlineElement.try_as<Documents::LineBreak>())
                    {
                        text.push_back(L'\n');
                    }
                }
                return text;
            };

            const auto title = tab->Title();
            VERIFY_ARE_EQUAL(winrt::hstring{ presentation.text }, tab->_headerControl.MetadataText());
            VERIFY_IS_FALSE(tab->_headerControl.IsMetadataVisible());
            VERIFY_ARE_EQUAL(title, Automation::AutomationProperties::GetName(tab->TabViewItem()));
            VERIFY_ARE_EQUAL(std::wstring::npos, tooltipText().find(presentation.tooltip));

            tab->SetVerticalTabLayout(true);
            VERIFY_IS_TRUE(tab->_headerControl.IsMetadataVisible());
            VERIFY_ARE_EQUAL(
                winrt::hstring{ std::wstring{ title } + L", " + presentation.accessibilityText },
                Automation::AutomationProperties::GetName(tab->TabViewItem()));
            VERIFY_ARE_NOT_EQUAL(std::wstring::npos, tooltipText().find(presentation.tooltip));

            tab->SetVerticalTabLayout(false);
            VERIFY_IS_FALSE(tab->_headerControl.IsMetadataVisible());
            VERIFY_ARE_EQUAL(title, Automation::AutomationProperties::GetName(tab->TabViewItem()));
            VERIFY_ARE_EQUAL(winrt::hstring{ presentation.text }, tab->_headerControl.MetadataText());
            VERIFY_ARE_EQUAL(std::wstring::npos, tooltipText().find(presentation.tooltip));
        });
    }

    void TabTests::RichTabManifestAcceptsCamelCaseFieldIds()
    {
        using namespace ::Microsoft::Terminal::RichTab::Provider;

        constexpr std::string_view manifestJson = R"({
            "schemaVersion": 1,
            "id": "com.microsoft.test",
            "displayName": "Test",
            "publisher": "Microsoft",
            "version": "1.0.0",
            "protocol": { "minVersion": 1, "maxVersion": 1 },
            "runtime": {
                "type": "powerShellV1",
                "entrypoint": "provider.ps1",
                "arguments": []
            },
            "activationEvents": [ "onManualRefresh" ],
            "fields": [
                {
                    "id": "agentStatus",
                    "displayName": "Agent status",
                    "type": "string",
                    "defaultVisible": true
                },
                {
                    "id": "changes",
                    "displayName": "Git changes",
                    "type": "string",
                    "defaultVisible": true
                }
            ]
        })";

        const auto parsed = ParseManifest(manifestJson, LR"(C:\providers\test)");
        VERIFY_IS_TRUE(static_cast<bool>(parsed));
        VERIFY_ARE_EQUAL(static_cast<size_t>(2), parsed.value->fields.size());
        VERIFY_IS_FALSE(IsCanonicalFieldId("WorkingDirectory"));
    }

    void TabTests::VisibleFieldsDoNotFilterTabs()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            page->_tabStrip.RichTabRepositoryVisible(false);
            page->_tabStrip.RichTabBranchVisible(false);
            page->_tabStrip.RichTabAgentStatusVisible(false);
            page->_tabStrip.RichTabWorkingDirectoryVisible(false);
            page->_tabStrip.RichTabChangesVisible(false);
            page->_ApplyTabListProjection();
            page->UpdateLayout();

            const auto container = page->_tabStrip.ContainerFromIndex(0).as<ListViewItem>();
            VERIFY_ARE_EQUAL(Visibility::Visible, container.Visibility());
            VERIFY_ARE_EQUAL(winrt::TerminalApp::TabStripFilterMode::AllTabs, page->_tabStrip.FilterMode());
        });
    }

    void TabTests::LiveTabLayoutLatestRequestWins()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            VERIFY_IS_TRUE(page->_isVerticalLayout);
            VERIFY_IS_TRUE(page->_ApplyTabLayout(TabLayout::Horizontal));
            VERIFY_IS_TRUE(page->_changingTabLayout);

            VERIFY_IS_TRUE(page->_ApplyTabLayout(TabLayout::Vertical));
            VERIFY_IS_TRUE(page->_pendingTabLayout.has_value());
            VERIFY_ARE_EQUAL(TabLayout::Vertical, *page->_pendingTabLayout);

            page->_CompleteTabLayoutChange(page->_tabLayoutGeneration);
            VERIFY_IS_FALSE(page->_isVerticalLayout);

            page->_ApplyPendingTabLayout();
            VERIFY_IS_TRUE(page->_changingTabLayout);
            page->_CompleteTabLayoutChange(page->_tabLayoutGeneration);
            VERIFY_IS_TRUE(page->_isVerticalLayout);
            VERIFY_IS_FALSE(page->_pendingTabLayout.has_value());
        });
    }

    void TabTests::TabLayoutSwitchMenuTracksOrientation()
    {
        auto page = _commonSetup(nullptr, nullptr, std::nullopt, true);

        TestOnUIThread([&]() {
            const auto tab = winrt::get_self<winrt::TerminalApp::implementation::Tab>(page->_tabs.GetAt(0));

            tab->SetVerticalTabLayout(true);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Switch to horizontal tabs" }, tab->_switchTabLayoutMenuItem.Text());
            VERIFY_ARE_EQUAL(TabLayout::Horizontal, tab->_switchTabLayoutTarget);

            tab->SetVerticalTabLayout(false);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"Switch to vertical tabs" }, tab->_switchTabLayoutMenuItem.Text());
            VERIFY_ARE_EQUAL(TabLayout::Vertical, tab->_switchTabLayoutTarget);
        });
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
        std::optional<int32_t> historySize,
        const bool verticalLayout)
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

        if (verticalLayout)
        {
            settings0.GlobalSettings().TabLayout(TabLayout::Vertical);
        }

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
        VERIFY_IS_TRUE(tab->GetAgentOverrideOrigin() == fixture.agentOverrideOrigin);
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
        VERIFY_IS_TRUE(std::wstring_view{ args.Commandline() }.find(L"--agent-override") == std::wstring_view::npos);
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

    void TabTests::_verifyContentTransferReviewZoom(bool hidden, bool zoomed, bool freshReceiver, bool twoLeaves, bool restoredAgent)
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
            if (restoredAgent)
            {
                fixture->agentOverrideOrigin = winrt::TerminalApp::implementation::Tab::AgentOverrideOrigin::Restore;
                tab->SetAgentOverride(tab->AgentIdOverride(),
                                      tab->AgentModelOverride(),
                                      tab->AgentCustomCommandOverride(),
                                      tab->AgentSourceOverride(),
                                      tab->AgentWslDistroOverride(),
                                      fixture->agentOverrideOrigin);
            }
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

    void TabTests::ContentTransferRestoredAgentBindingKeepsOrigin()
    {
        _verifyContentTransferReviewZoom(true, false, false, true, true);
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

    void TabTests::DetachLastTerminalPaneClosesAgentOnlyTab()
    {
        for (const bool hidden : { false, true })
        {
            auto fixture = _createContentTransferFixture(false, hidden, false, true);
            const auto cleanup = wil::scope_exit([&]() {
                RunOnUIThread([&]() {
                    _closeContentTransferFixture(*fixture, false);
                    fixture.reset();
                });
            });
            TestOnUIThread([&]() {
                const auto tab = fixture->original.tab;
                const auto selected = tab->GetActivePane();
                const auto contentId = selected->GetTerminalControl().ContentId();
                VERIFY_IS_FALSE(selected->IsAgentPane());
                VERIFY_ARE_EQUAL(hidden, tab->HasStashedAgentPane());
                unsigned int closed = 0;
                const auto token = tab->Closed([&](auto&&, auto&&) { ++closed; });
                const auto revoke = wil::scope_exit([&]() { tab->Closed(token); });

                const auto detached = tab->DetachPane(selected);
                VERIFY_IS_TRUE(detached == selected);
                VERIFY_ARE_EQUAL(1u, closed);
                VERIFY_ARE_EQUAL(0u, fixture->source->_tabs.Size());
                VERIFY_IS_TRUE(tab->GetRootPane()->GetActivePane() == nullptr);
                VERIFY_IS_NULL(tab->FindAgentPaneContent());
                VERIFY_IS_NULL(fixture->source->_manager.TryLookupCore(fixture->agentContentId));
                VERIFY_IS_NOT_NULL(fixture->source->_manager.TryLookupCore(contentId));
                VERIFY_ARE_EQUAL(contentId, detached->GetTerminalControl().ContentId());

                detached->Shutdown();
                _closeContentTransferFixture(*fixture, true);
            });
        }
    }

    void TabTests::DetachTerminalPanePreservesRemainingFocus()
    {
        auto fixture = _createContentTransferFixture(false, false);
        const auto cleanup = wil::scope_exit([&]() {
            RunOnUIThread([&]() {
                _closeContentTransferFixture(*fixture, false);
                fixture.reset();
            });
        });
        TestOnUIThread([&]() {
            const auto tab = fixture->original.tab;
            fixture->source->_TeardownAgentPane(tab);
            const auto selected = tab->GetActivePane();
            const auto contentId = selected->GetTerminalControl().ContentId();
            const auto remaining = std::find_if(fixture->original.leaves.begin(), fixture->original.leaves.end(), [&](const auto& leaf) {
                return leaf.contentId != fixture->agentContentId && leaf.contentId != contentId;
            });
            VERIFY_IS_TRUE(remaining != fixture->original.leaves.end());

            const auto detached = tab->DetachPane(selected);
            VERIFY_IS_TRUE(detached == selected);
            VERIFY_ARE_EQUAL(1u, fixture->source->_tabs.Size());
            VERIFY_ARE_EQUAL(1, tab->GetLeafPaneCount());
            VERIFY_IS_TRUE(tab->GetActivePane() != nullptr);
            VERIFY_IS_TRUE(tab->GetRootPane()->GetActivePane() == tab->GetActivePane());
            VERIFY_ARE_EQUAL(remaining->contentId, tab->GetActiveTerminalControl().ContentId());
            VERIFY_IS_TRUE(tab->GetActiveTerminalControl().Connection() == remaining->connection);
            VERIFY_ARE_EQUAL(0u, remaining->closed->load());
            VERIFY_IS_NOT_NULL(fixture->source->_manager.TryLookupCore(contentId));

            detached->Shutdown();
            _closeContentTransferFixture(*fixture, true);
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

    void TabTests::SessionsDisabledHintFollowsViewAndSettings()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto globals = page->_settings.GlobalSettings();
            VERIFY_IS_FALSE(globals.IsAgentSessionHooksPolicyLocked());
            globals.AgentSessionManagementEnabled(false);
            const auto pane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            const auto content = pane->GetContent().as<winrt::TerminalApp::AgentPaneContent>();
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(content);
            const auto root = impl->GetRoot();
            const auto hint = root.FindName(L"SessionsHintRoot").as<Border>();
            const auto text = root.FindName(L"SessionsDisabledHintText").as<TextBlock>();
            const auto terminal = impl->GetTermControl();

            VERIFY_ARE_EQUAL(Visibility::Collapsed, hint.Visibility());
            impl->SetSessionsView(true);
            VERIFY_ARE_EQUAL(Visibility::Visible, hint.Visibility());
            VERIFY_IS_FALSE(text.Text().empty());
            VERIFY_ARE_EQUAL(TextWrapping::Wrap, text.TextWrapping());

            const auto foreground = text.Foreground().as<winrt::Windows::UI::Xaml::Media::SolidColorBrush>();
            const auto color = foreground.Color();
            const winrt::Windows::UI::Color footerColor{ 255, 0x8b, 0x8b, 0x8b };
            VERIFY_ARE_EQUAL(footerColor, color);
            const auto opacity = text.Opacity() * foreground.Opacity() * color.A / 255.0;
            VERIFY_IS_TRUE(opacity > 0.0 && opacity < 1.0);
            for (const auto background : { 12.0, 255.0 })
            {
                const auto blended = opacity * color.R + (1.0 - opacity) * background;
                VERIFY_IS_TRUE(std::abs(blended - background) < std::abs(footerColor.R - background));
            }

            // Working hooks can still report status while Sessions is off.
            impl->SetAgentSessionId(L"existing-session");
            impl->UpdateAgentStatus(L"Copilot", L"v1", L"model", L"connected", L"Windows");
            VERIFY_ARE_EQUAL(Visibility::Visible, hint.Visibility());

            globals.AgentSessionManagementEnabled(true);
            impl->UpdateSettings(page->_settings);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, hint.Visibility());
            globals.AgentSessionManagementEnabled(false);
            impl->UpdateSettings(page->_settings);
            VERIFY_ARE_EQUAL(Visibility::Visible, hint.Visibility());

            impl->SetSessionsView(false);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, hint.Visibility());
            impl->SetSessionsView(true);
            impl->SetSessionsView(true);
            VERIFY_ARE_EQUAL(Visibility::Visible, hint.Visibility());
            VERIFY_IS_TRUE(impl->GetTermControl() == terminal);
            VERIFY_IS_TRUE(impl->AgentSessionId() == L"existing-session");
            VERIFY_IS_TRUE(impl->IsAgentConnected());
        });
    }

    void TabTests::SessionsDisabledHintUpdatesWhileStashed()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto globals = page->_settings.GlobalSettings();
            VERIFY_IS_FALSE(globals.IsAgentSessionHooksPolicyLocked());
            globals.AgentSessionManagementEnabled(false);
            const auto tab = page->_GetFocusedTabImpl();
            const auto pane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            pane->IsAgentPane(true);
            page->_SplitPane(tab, SplitDirection::Left, 0.5f, pane);
            const auto content = tab->FindAgentPaneContent();
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(content);
            const auto hint = impl->GetRoot().FindName(L"SessionsHintRoot").as<Border>();
            impl->SetSessionsView(true);
            VERIFY_ARE_EQUAL(Visibility::Visible, hint.Visibility());

            tab->StashAgentPane();
            VERIFY_IS_TRUE(tab->HasStashedAgentPane());
            globals.AgentSessionManagementEnabled(true);
            tab->UpdateSettings(page->_settings);
            VERIFY_ARE_EQUAL(Visibility::Collapsed, hint.Visibility());
            VERIFY_IS_TRUE(tab->RestoreStashedAgentPane(SplitDirection::Left));
            VERIFY_ARE_EQUAL(Visibility::Collapsed, hint.Visibility());

            tab->StashAgentPane();
            globals.AgentSessionManagementEnabled(false);
            tab->UpdateSettings(page->_settings);
            VERIFY_IS_TRUE(tab->RestoreStashedAgentPane(SplitDirection::Left));
            VERIFY_ARE_EQUAL(Visibility::Visible, hint.Visibility());
            VERIFY_IS_TRUE(tab->FindAgentPaneContent() == content);
            VERIFY_IS_TRUE(impl->IsSessionsView());
        });
    }

    void TabTests::SessionsDisabledHintWrapsAndPreservesTerminalGrid()
    {
        auto page = _commonSetup();

        TestOnUIThread([&]() {
            const auto globals = page->_settings.GlobalSettings();
            VERIFY_IS_FALSE(globals.IsAgentSessionHooksPolicyLocked());
            globals.AgentSessionManagementEnabled(false);
            const auto pane = page->_WrapInAgentPaneContent(page->_MakePane(nullptr, nullptr, nullptr));
            pane->IsAgentPane(true);
            page->_SplitPane(page->_GetFocusedTabImpl(), SplitDirection::Left, 0.5f, pane);
            const auto content = pane->GetContent().as<winrt::TerminalApp::AgentPaneContent>();
            const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(content);
            const auto root = impl->GetRoot();
            const auto hint = root.FindName(L"SessionsHintRoot").as<Border>();
            const auto inner = root.FindName(L"InnerContent").as<ContentPresenter>();
            const auto terminal = winrt::get_self<winrt::TerminalApp::implementation::TerminalPaneContent>(impl->GetTerminalContent());
            const auto layout = [&](const float width) {
                root.Width(width);
                root.Height(600);
                root.UpdateLayout();
                VERIFY_ARE_EQUAL(width, static_cast<float>(root.ActualWidth()));
            };

            layout(900);
            const auto baselineMinimum = impl->MinimumSize();
            impl->SetSessionsView(true);
            layout(900);
            const auto wideHeight = hint.ActualHeight();
            VERIFY_IS_TRUE(wideHeight > 0.0);

            layout(200);
            VERIFY_IS_TRUE(hint.ActualHeight() > wideHeight);
            const auto hintHeight = hint.DesiredSize().Height;
            VERIFY_ARE_EQUAL(baselineMinimum.Height + hintHeight, impl->MinimumSize().Height);
            VERIFY_ARE_EQUAL(baselineMinimum.Width, impl->MinimumSize().Width);
            const auto hintTop = hint.TransformToVisual(root).TransformPoint({ 0, 0 }).Y;
            const auto innerTop = inner.TransformToVisual(root).TransformPoint({ 0, 0 }).Y;
            VERIFY_ARE_EQUAL(36.0f, hintTop);
            VERIFY_ARE_EQUAL(hintTop + hintHeight, innerTop);

            const auto chromeHeight = 36.0f + hintHeight;
            VERIFY_ARE_EQUAL(
                terminal->SnapDownToGrid(PaneSnapDirection::Height, 600.0f - chromeHeight) + chromeHeight,
                impl->SnapDownToGrid(PaneSnapDirection::Height, 600.0f));
            VERIFY_ARE_EQUAL(
                terminal->SnapDownToGrid(PaneSnapDirection::Width, 200.0f),
                impl->SnapDownToGrid(PaneSnapDirection::Width, 200.0f));

            impl->SetSessionsView(false);
            layout(200);
            VERIFY_ARE_EQUAL(baselineMinimum.Height, impl->MinimumSize().Height);
            VERIFY_ARE_EQUAL(36.0f, inner.TransformToVisual(root).TransformPoint({ 0, 0 }).Y);
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
