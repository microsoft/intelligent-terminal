// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "../TerminalControl/EventArgs.h"
#include "../TerminalControl/ControlCore.h"
#include "MockControlSettings.h"
#include "MockConnection.h"
#include "../../inc/TestUtils.h"

using namespace Microsoft::Console;
using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;

using namespace winrt;
using namespace winrt::Microsoft::Terminal;

namespace ControlUnitTests
{
    class ControlCoreTests
    {
        BEGIN_TEST_CLASS(ControlCoreTests)
            TEST_CLASS_PROPERTY(L"TestTimeout", L"0:0:10") // 10s timeout
        END_TEST_CLASS()

        TEST_METHOD(ComPtrSettings);
        TEST_METHOD(InstantiateCore);
        TEST_METHOD(TestInitialize);
        TEST_METHOD(TestAdjustAcrylic);

        TEST_METHOD(TestFreeAfterClose);
        TEST_METHOD(TestHoverPathWorkerCoalescing);
        TEST_METHOD(TestHoverPathWorkerBoundedPending);
        TEST_METHOD(TestHoverPathWorkerControlLifetime);
        TEST_METHOD(TestHoverPathRefreshAfterOutputInvalidation);

        TEST_METHOD(TestFontInitializedInCtor);

        TEST_METHOD(TestClearScrollback);
        TEST_METHOD(TestClearScreen);
        TEST_METHOD(TestClearAll);
        TEST_METHOD(TestReadEntireBuffer);

        TEST_METHOD(TestSelectCommandSimple);
        TEST_METHOD(TestSelectOutputSimple);
        TEST_METHOD(TestCommandContext);
        TEST_METHOD(TestCommandContextWithPwshGhostText);

        TEST_METHOD(TestSelectOutputScrolling);
        TEST_METHOD(TestSelectOutputExactWrap);

        TEST_METHOD(TestSimpleClickSelection);

        TEST_CLASS_SETUP(ModuleSetup)
        {
            winrt::init_apartment(winrt::apartment_type::single_threaded);

            return true;
        }
        TEST_CLASS_CLEANUP(ClassCleanup)
        {
            winrt::uninit_apartment();
            return true;
        }

        std::tuple<winrt::com_ptr<MockControlSettings>, winrt::com_ptr<MockConnection>> _createSettingsAndConnection()
        {
            Log::Comment(L"Create settings object");
            auto settings = winrt::make_self<MockControlSettings>();
            VERIFY_IS_NOT_NULL(settings);

            Log::Comment(L"Create connection object");
            auto conn = winrt::make_self<MockConnection>();
            VERIFY_IS_NOT_NULL(conn);

            return { settings, conn };
        }

        winrt::com_ptr<Control::implementation::ControlCore> createCore(Control::IControlSettings settings,
                                                                        TerminalConnection::ITerminalConnection conn)
        {
            Log::Comment(L"Create ControlCore object");

            auto core = winrt::make_self<Control::implementation::ControlCore>(settings, settings, conn);
            core->_inUnitTests = true;
            return core;
        }

        void _standardInit(winrt::com_ptr<Control::implementation::ControlCore> core)
        {
            // "Consolas" ends up with an actual size of 9x19 at 96DPI. So
            // let's just arbitrarily start with a 270x380px (30x20 chars) window
            core->Initialize(270, 380, 1.0);
#ifndef NDEBUG
            core->_terminal->_suppressLockChecks = true;
#endif
            VERIFY_IS_TRUE(core->_initializedTerminal);
            VERIFY_ARE_EQUAL(20, core->_terminal->GetViewport().Height());
        }
    };

    void ControlCoreTests::ComPtrSettings()
    {
        Log::Comment(L"Just make sure we can instantiate a settings obj in a com_ptr");
        auto settings = winrt::make_self<MockControlSettings>();

        Log::Comment(L"Verify literally any setting, it doesn't matter");
        VERIFY_ARE_EQUAL(DEFAULT_FOREGROUND, settings->DefaultForeground());
    }

    void ControlCoreTests::InstantiateCore()
    {
        auto [settings, conn] = _createSettingsAndConnection();

        auto core = createCore(*settings, *conn);
        VERIFY_IS_NOT_NULL(core);
    }

    void ControlCoreTests::TestInitialize()
    {
        auto [settings, conn] = _createSettingsAndConnection();

        auto core = createCore(*settings, *conn);
        VERIFY_IS_NOT_NULL(core);

        VERIFY_IS_FALSE(core->_initializedTerminal);
        // "Consolas" ends up with an actual size of 9x19 at 96DPI. So
        // let's just arbitrarily start with a 270x380px (30x20 chars) window
        core->Initialize(270, 380, 1.0);
#ifndef NDEBUG
        core->_terminal->_suppressLockChecks = true;
#endif
        VERIFY_IS_TRUE(core->_initializedTerminal);
        VERIFY_ARE_EQUAL(30, core->_terminal->GetViewport().Width());
    }

    void ControlCoreTests::TestAdjustAcrylic()
    {
        auto [settings, conn] = _createSettingsAndConnection();

        settings->UseAcrylic(true);
        settings->Opacity(0.5f);

        auto core = createCore(*settings, *conn);
        VERIFY_IS_NOT_NULL(core);

        // A callback to make sure that we're raising TransparencyChanged events
        auto expectedOpacity = 0.5f;
        auto opacityCallback = [&](auto&&, Control::TransparencyChangedEventArgs args) mutable {
            VERIFY_ARE_EQUAL(expectedOpacity, args.Opacity());
            VERIFY_ARE_EQUAL(expectedOpacity, core->Opacity());
            // The Settings object's opacity shouldn't be changed
            VERIFY_ARE_EQUAL(0.5f, settings->Opacity());

            if (expectedOpacity < 1.0f)
            {
                VERIFY_IS_TRUE(settings->UseAcrylic());
                VERIFY_IS_TRUE(core->_settings.UseAcrylic());
            }

            // GH#603: Adjusting opacity shouldn't change whether or not we
            // requested acrylic.

            auto expectedUseAcrylic = expectedOpacity < 1.0f;
            VERIFY_IS_TRUE(core->_settings.UseAcrylic());
            VERIFY_ARE_EQUAL(expectedUseAcrylic, core->UseAcrylic());
        };
        core->TransparencyChanged(opacityCallback);

        VERIFY_IS_FALSE(core->_initializedTerminal);
        // "Cascadia Mono" ends up with an actual size of 9x19 at 96DPI. So
        // let's just arbitrarily start with a 270x380px (30x20 chars) window
        core->Initialize(270, 380, 1.0);
        VERIFY_IS_TRUE(core->_initializedTerminal);

        Log::Comment(L"Increasing opacity till fully opaque");
        expectedOpacity += 0.1f; // = 0.6;
        core->AdjustOpacity(0.1f);
        expectedOpacity += 0.1f; // = 0.7;
        core->AdjustOpacity(0.1f);
        expectedOpacity += 0.1f; // = 0.8;
        core->AdjustOpacity(0.1f);
        expectedOpacity += 0.1f; // = 0.9;
        core->AdjustOpacity(0.1f);
        expectedOpacity += 0.1f; // = 1.0;
        // cast to float because floating point numbers are mean
        VERIFY_ARE_EQUAL(1.0f, expectedOpacity);
        core->AdjustOpacity(0.1f);

        Log::Comment(L"Increasing opacity more doesn't actually change it to be >1.0");

        expectedOpacity = 1.0f;
        core->AdjustOpacity(0.1f);

        Log::Comment(L"Decrease opacity");
        expectedOpacity -= 0.25f; // = 0.75;
        core->AdjustOpacity(-0.25f);
        expectedOpacity -= 0.25f; // = 0.5;
        core->AdjustOpacity(-0.25f);
        expectedOpacity -= 0.25f; // = 0.25;
        core->AdjustOpacity(-0.25f);
        expectedOpacity -= 0.25f; // = 0.05;
        // cast to float because floating point numbers are mean
        VERIFY_ARE_EQUAL(0.0f, expectedOpacity);
        core->AdjustOpacity(-0.25f);

        Log::Comment(L"Decreasing opacity more doesn't actually change it to be < 0");
        expectedOpacity = 0.0f;
        core->AdjustOpacity(-0.25f);
    }

    void ControlCoreTests::TestFreeAfterClose()
    {
        {
            auto [settings, conn] = _createSettingsAndConnection();

            auto core = createCore(*settings, *conn);
            VERIFY_IS_NOT_NULL(core);

            Log::Comment(L"Close the Core, like a TermControl would");
            core->Close();
        }

        VERIFY_IS_TRUE(true, L"Make sure that the test didn't crash when the core when out of scope");
    }

    void ControlCoreTests::TestFontInitializedInCtor()
    {
        // This is to catch a dumb programming mistake I made while working on
        // the core/control split. We want the font initialized in the ctor,
        // before we even get to Core::Initialize.

        auto [settings, conn] = _createSettingsAndConnection();

        // Make sure to use something dumb like "Impact" as a font name here so
        // that you don't default to Cascadia*
        settings->FontFace(L"Impact");

        auto core = createCore(*settings, *conn);
        VERIFY_IS_NOT_NULL(core);

        VERIFY_ARE_EQUAL(L"Impact", std::wstring_view{ core->_actualFont.GetFaceName() });
    }

    void ControlCoreTests::TestClearScrollback()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        Log::Comment(L"Create ControlCore object");
        auto core = createCore(*settings, *conn);
        VERIFY_IS_NOT_NULL(core);
        _standardInit(core);

        Log::Comment(L"Print 40 rows of 'Foo', and a single row of 'Bar' "
                     L"(leaving the cursor after 'Bar')");
        for (auto i = 0; i < 40; ++i)
        {
            conn->WriteInput(winrt_wstring_to_array_view(L"Foo\r\n"));
        }
        conn->WriteInput(winrt_wstring_to_array_view(L"Bar"));

        // We printed that 40 times, but the final \r\n bumped the view down one MORE row.
        Log::Comment(L"Check the buffer viewport before the clear");
        VERIFY_ARE_EQUAL(20, core->_terminal->GetViewport().Height());
        VERIFY_ARE_EQUAL(21, core->ScrollOffset());
        VERIFY_ARE_EQUAL(20, core->ViewHeight());
        VERIFY_ARE_EQUAL(41, core->BufferHeight());

        Log::Comment(L"Clear the buffer");
        core->ClearBuffer(Control::ClearBufferType::Scrollback);

        Log::Comment(L"Check the buffer after the clear");
        VERIFY_ARE_EQUAL(20, core->_terminal->GetViewport().Height());
        VERIFY_ARE_EQUAL(0, core->ScrollOffset());
        VERIFY_ARE_EQUAL(20, core->ViewHeight());
        VERIFY_ARE_EQUAL(20, core->BufferHeight());

        // In this test, we can't actually check if we cleared the buffer
        // contents. ConPTY will handle the actual clearing of the buffer
        // contents. We can only ensure that the viewport moved when we did a
        // clear scrollback.
    }
    void ControlCoreTests::TestClearScreen()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        Log::Comment(L"Create ControlCore object");
        auto core = createCore(*settings, *conn);
        VERIFY_IS_NOT_NULL(core);
        _standardInit(core);

        Log::Comment(L"Print 40 rows of 'Foo', and a single row of 'Bar' "
                     L"(leaving the cursor after 'Bar')");
        for (auto i = 0; i < 40; ++i)
        {
            conn->WriteInput(winrt_wstring_to_array_view(L"Foo\r\n"));
        }
        conn->WriteInput(winrt_wstring_to_array_view(L"Bar"));

        // We printed that 40 times, but the final \r\n bumped the view down one MORE row.
        Log::Comment(L"Check the buffer viewport before the clear");
        VERIFY_ARE_EQUAL(20, core->_terminal->GetViewport().Height());
        VERIFY_ARE_EQUAL(21, core->ScrollOffset());
        VERIFY_ARE_EQUAL(20, core->ViewHeight());
        VERIFY_ARE_EQUAL(41, core->BufferHeight());

        Log::Comment(L"Clear the buffer");
        core->ClearBuffer(Control::ClearBufferType::Screen);

        Log::Comment(L"Check the buffer after the clear");
        VERIFY_ARE_EQUAL(20, core->_terminal->GetViewport().Height());
        VERIFY_ARE_EQUAL(21, core->ScrollOffset());
        VERIFY_ARE_EQUAL(20, core->ViewHeight());
        VERIFY_ARE_EQUAL(41, core->BufferHeight());

        // In this test, we can't actually check if we cleared the buffer
        // contents. ConPTY will handle the actual clearing of the buffer
        // contents. We can only ensure that the viewport moved when we did a
        // clear scrollback.
    }
    void ControlCoreTests::TestClearAll()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        Log::Comment(L"Create ControlCore object");
        auto core = createCore(*settings, *conn);
        VERIFY_IS_NOT_NULL(core);
        _standardInit(core);

        Log::Comment(L"Print 40 rows of 'Foo', and a single row of 'Bar' "
                     L"(leaving the cursor after 'Bar')");
        for (auto i = 0; i < 40; ++i)
        {
            conn->WriteInput(winrt_wstring_to_array_view(L"Foo\r\n"));
        }
        conn->WriteInput(winrt_wstring_to_array_view(L"Bar"));

        // We printed that 40 times, but the final \r\n bumped the view down one MORE row.
        Log::Comment(L"Check the buffer viewport before the clear");
        VERIFY_ARE_EQUAL(20, core->_terminal->GetViewport().Height());
        VERIFY_ARE_EQUAL(21, core->ScrollOffset());
        VERIFY_ARE_EQUAL(20, core->ViewHeight());
        VERIFY_ARE_EQUAL(41, core->BufferHeight());

        Log::Comment(L"Clear the buffer");
        core->ClearBuffer(Control::ClearBufferType::All);

        Log::Comment(L"Check the buffer after the clear");
        VERIFY_ARE_EQUAL(20, core->_terminal->GetViewport().Height());
        VERIFY_ARE_EQUAL(0, core->ScrollOffset());
        VERIFY_ARE_EQUAL(20, core->ViewHeight());
        VERIFY_ARE_EQUAL(20, core->BufferHeight());

        // In this test, we can't actually check if we cleared the buffer
        // contents. ConPTY will handle the actual clearing of the buffer
        // contents. We can only ensure that the viewport moved when we did a
        // clear scrollback.
    }

    void ControlCoreTests::TestReadEntireBuffer()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        Log::Comment(L"Create ControlCore object");
        auto core = createCore(*settings, *conn);
        VERIFY_IS_NOT_NULL(core);
        _standardInit(core);

        Log::Comment(L"Print some text");
        conn->WriteInput(winrt_wstring_to_array_view(L"This is some text     \r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"with varying amounts  \r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"of whitespace         \r\n"));

        Log::Comment(L"Check the buffer contents");
        VERIFY_ARE_EQUAL(L"This is some text\r\nwith varying amounts\r\nof whitespace\r\n",
                         core->ReadEntireBuffer());
    }

    static void _writePrompt(const winrt::com_ptr<MockConnection>& conn, const std::wstring_view& path)
    {
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;D\x7"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;A\x7"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]9;9;"));
        conn->WriteInput(winrt_wstring_to_array_view(path));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x7"));
        conn->WriteInput(winrt_wstring_to_array_view(L"PWSH "));
        conn->WriteInput(winrt_wstring_to_array_view(path));
        conn->WriteInput(winrt_wstring_to_array_view(L"> "));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;B\x7"));
    }

    void ControlCoreTests::TestSelectCommandSimple()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        Log::Comment(L"Create ControlCore object");
        auto core = createCore(*settings, *conn);
        VERIFY_IS_NOT_NULL(core);
        _standardInit(core);

        Log::Comment(L"Print some text");

        _writePrompt(conn, L"C:\\Windows");
        conn->WriteInput(winrt_wstring_to_array_view(L"Foo-bar"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;C\x7"));

        conn->WriteInput(winrt_wstring_to_array_view(L"\r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"This is some text     \r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"with varying amounts  \r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"of whitespace         \r\n"));

        _writePrompt(conn, L"C:\\Windows");

        Log::Comment(L"Check the buffer contents");
        const auto& buffer = core->_terminal->GetTextBuffer();
        const auto& cursor = buffer.GetCursor();

        {
            const til::point expectedCursor{ 17, 4 };
            VERIFY_ARE_EQUAL(expectedCursor, cursor.GetPosition());
        }

        VERIFY_IS_FALSE(core->HasSelection());
        core->SelectCommand(true);
        VERIFY_IS_TRUE(core->HasSelection());
        {
            const auto& start = core->_terminal->GetSelectionAnchor();
            const auto& end = core->_terminal->GetSelectionEnd();
            const til::point expectedStart{ 17, 0 };
            const til::point expectedEnd{ 24, 0 };
            VERIFY_ARE_EQUAL(expectedStart, start);
            VERIFY_ARE_EQUAL(expectedEnd, end);
        }

        core->_terminal->ClearSelection();
        conn->WriteInput(winrt_wstring_to_array_view(L"Boo-far"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;C\x7"));

        VERIFY_IS_FALSE(core->HasSelection());
        {
            const til::point expectedCursor{ 24, 4 };
            VERIFY_ARE_EQUAL(expectedCursor, cursor.GetPosition());
        }
        VERIFY_IS_FALSE(core->HasSelection());
        core->SelectCommand(true);
        VERIFY_IS_TRUE(core->HasSelection());
        {
            const auto& start = core->_terminal->GetSelectionAnchor();
            const auto& end = core->_terminal->GetSelectionEnd();
            const til::point expectedStart{ 17, 4 };
            const til::point expectedEnd{ 24, 4 };
            VERIFY_ARE_EQUAL(expectedStart, start);
            VERIFY_ARE_EQUAL(expectedEnd, end);
        }
        core->SelectCommand(true);
        VERIFY_IS_TRUE(core->HasSelection());
        {
            const auto& start = core->_terminal->GetSelectionAnchor();
            const auto& end = core->_terminal->GetSelectionEnd();
            const til::point expectedStart{ 17, 0 };
            const til::point expectedEnd{ 24, 0 };
            VERIFY_ARE_EQUAL(expectedStart, start);
            VERIFY_ARE_EQUAL(expectedEnd, end);
        }
        core->SelectCommand(false);
        VERIFY_IS_TRUE(core->HasSelection());
        {
            const auto& start = core->_terminal->GetSelectionAnchor();
            const auto& end = core->_terminal->GetSelectionEnd();
            const til::point expectedStart{ 17, 4 };
            const til::point expectedEnd{ 24, 4 };
            VERIFY_ARE_EQUAL(expectedStart, start);
            VERIFY_ARE_EQUAL(expectedEnd, end);
        }
    }

    void ControlCoreTests::TestHoverPathWorkerCoalescing()
    {
        auto& worker = Control::implementation::HoverPathWorker::Instance();
        VERIFY_IS_TRUE(worker._waitForIdleForTests(std::chrono::seconds{ 2 }));

        struct State
        {
            std::mutex mutex;
            std::condition_variable condition;
            bool firstStarted = false;
            bool releaseFirst = false;
        };
        const auto state = std::make_shared<State>();
        std::vector<uint64_t> completed;

        worker._setResolverForTests(
            [state](const auto& request, auto&) -> std::optional<::Microsoft::Terminal::Core::Terminal::HoverPathResult> {
                if (request.requestId == 1)
                {
                    std::unique_lock lock{ state->mutex };
                    state->firstStarted = true;
                    state->condition.notify_all();
                    state->condition.wait(lock, [&]() { return state->releaseFirst; });
                }
                return ::Microsoft::Terminal::Core::Terminal::HoverPathResult{
                    .candidateIndex = 0,
                    .uri = std::to_wstring(request.requestId),
                };
            });
        auto restoreResolver = wil::scope_exit([&]() {
            {
                const std::lock_guard guard{ state->mutex };
                state->releaseFirst = true;
            }
            state->condition.notify_all();
            worker._waitForIdleForTests(std::chrono::seconds{ 2 });
            worker._resetResolverForTests();
        });

        std::mutex completionMutex;
        std::condition_variable completionCondition;
        const auto completion = [&](auto, auto, auto request, auto result) {
            VERIFY_IS_TRUE(result.has_value());
            const std::lock_guard guard{ completionMutex };
            completed.emplace_back(request.requestId);
            completionCondition.notify_all();
        };

        const auto firstTarget = worker.CreateTarget();
        const auto secondTarget = worker.CreateTarget();
        worker.Submit(firstTarget,
                      ::Microsoft::Terminal::Core::Terminal::HoverPathRequest{ .requestId = 1 },
                      completion);
        {
            std::unique_lock lock{ state->mutex };
            VERIFY_IS_TRUE(state->condition.wait_for(lock, std::chrono::seconds{ 2 }, [&]() { return state->firstStarted; }));
        }

        worker.Submit(firstTarget,
                      ::Microsoft::Terminal::Core::Terminal::HoverPathRequest{ .requestId = 2 },
                      completion);
        worker.Submit(firstTarget,
                      ::Microsoft::Terminal::Core::Terminal::HoverPathRequest{ .requestId = 3 },
                      completion);
        worker.Submit(secondTarget,
                      ::Microsoft::Terminal::Core::Terminal::HoverPathRequest{ .requestId = 4 },
                      completion);
        {
            const std::lock_guard guard{ state->mutex };
            state->releaseFirst = true;
        }
        state->condition.notify_all();

        {
            std::unique_lock lock{ completionMutex };
            VERIFY_IS_TRUE(completionCondition.wait_for(lock, std::chrono::seconds{ 2 }, [&]() { return completed.size() == 2; }));
            VERIFY_ARE_EQUAL(uint64_t{ 3 }, completed[0],
                             L"The in-flight stale generation and superseded pending request are discarded.");
            VERIFY_ARE_EQUAL(uint64_t{ 4 }, completed[1],
                             L"Distinct targets retain FIFO fairness.");
        }
        VERIFY_IS_TRUE(worker._waitForIdleForTests(std::chrono::seconds{ 2 }));
        VERIFY_ARE_EQUAL(size_t{ 1 }, worker._maxActiveProbeCountForTests(),
                         L"The process-wide resolver runs at most one filesystem probe at a time.");
    }

    void ControlCoreTests::TestHoverPathWorkerBoundedPending()
    {
        auto& worker = Control::implementation::HoverPathWorker::Instance();
        VERIFY_IS_TRUE(worker._waitForIdleForTests(std::chrono::seconds{ 2 }));

        struct State
        {
            std::mutex mutex;
            std::condition_variable condition;
            bool firstStarted = false;
            bool releaseFirst = false;
        };
        const auto state = std::make_shared<State>();
        worker._setResolverForTests(
            [state](const auto& request, auto&) -> std::optional<::Microsoft::Terminal::Core::Terminal::HoverPathResult> {
                if (request.requestId == 1)
                {
                    std::unique_lock lock{ state->mutex };
                    state->firstStarted = true;
                    state->condition.notify_all();
                    state->condition.wait(lock, [&]() { return state->releaseFirst; });
                }
                return ::Microsoft::Terminal::Core::Terminal::HoverPathResult{
                    .candidateIndex = 0,
                    .uri = std::to_wstring(request.requestId),
                };
            });
        auto restoreResolver = wil::scope_exit([&]() {
            {
                const std::lock_guard guard{ state->mutex };
                state->releaseFirst = true;
            }
            state->condition.notify_all();
            worker._waitForIdleForTests(std::chrono::seconds{ 2 });
            worker._resetResolverForTests();
        });

        std::atomic<size_t> resolved{ 0 };
        std::atomic<size_t> rejected{ 0 };
        const auto completion = [&](auto, auto, auto, auto result) {
            (result ? resolved : rejected).fetch_add(1, std::memory_order_relaxed);
        };

        std::vector<Control::implementation::HoverPathWorker::TargetPtr> targets;
        targets.reserve(Control::implementation::HoverPathWorker::MaxPendingTargets + 2);
        targets.emplace_back(worker.CreateTarget());
        worker.Submit(targets.back(),
                      ::Microsoft::Terminal::Core::Terminal::HoverPathRequest{ .requestId = 1 },
                      completion);
        {
            std::unique_lock lock{ state->mutex };
            VERIFY_IS_TRUE(state->condition.wait_for(lock, std::chrono::seconds{ 2 }, [&]() { return state->firstStarted; }));
        }

        for (size_t index = 0; index <= Control::implementation::HoverPathWorker::MaxPendingTargets; ++index)
        {
            targets.emplace_back(worker.CreateTarget());
            worker.Submit(targets.back(),
                          ::Microsoft::Terminal::Core::Terminal::HoverPathRequest{ .requestId = 100 + index },
                          completion);
        }

        VERIFY_ARE_EQUAL(Control::implementation::HoverPathWorker::MaxPendingTargets,
                         worker._pendingCountForTests(),
                         L"Pending work is hard-bounded to one entry for at most 64 targets.");
        VERIFY_ARE_EQUAL(size_t{ 1 }, rejected.load(std::memory_order_relaxed),
                         L"Overflow rejects the newest target without evicting admitted targets.");

        {
            const std::lock_guard guard{ state->mutex };
            state->releaseFirst = true;
        }
        state->condition.notify_all();
        VERIFY_IS_TRUE(worker._waitForIdleForTests(std::chrono::seconds{ 2 }));
        VERIFY_ARE_EQUAL(Control::implementation::HoverPathWorker::MaxPendingTargets + 1,
                         resolved.load(std::memory_order_relaxed));
        VERIFY_ARE_EQUAL(size_t{ 1 }, worker._maxActiveProbeCountForTests());
    }

    void ControlCoreTests::TestHoverPathWorkerControlLifetime()
    {
        auto& worker = Control::implementation::HoverPathWorker::Instance();
        VERIFY_IS_TRUE(worker._waitForIdleForTests(std::chrono::seconds{ 2 }));
        const auto pool = worker._pool;
        const auto work = worker._work;
        VERIFY_IS_NOT_NULL(pool);
        VERIFY_IS_NOT_NULL(work);

        struct State
        {
            std::mutex mutex;
            std::condition_variable condition;
            size_t started = 0;
            bool release = false;
        };
        const auto state = std::make_shared<State>();
        worker._setResolverForTests(
            [state](const auto&, auto&) -> std::optional<::Microsoft::Terminal::Core::Terminal::HoverPathResult> {
                std::unique_lock lock{ state->mutex };
                ++state->started;
                state->condition.notify_all();
                state->condition.wait(lock, [&]() { return state->release; });
                return std::nullopt;
            });
        auto restoreResolver = wil::scope_exit([&]() {
            {
                const std::lock_guard guard{ state->mutex };
                state->release = true;
            }
            state->condition.notify_all();
            worker._waitForIdleForTests(std::chrono::seconds{ 2 });
            worker._resetResolverForTests();
        });

        std::weak_ptr<Control::implementation::HoverPathWorker::Target> firstTarget;
        {
            auto [settings, connection] = _createSettingsAndConnection();
            auto core = createCore(*settings, *connection);
            core->_hoverPathRequestId = 1;
            core->_hoverPathPendingOrActive.store(true, std::memory_order_relaxed);
            core->_queueHoverPathRequest({ .requestId = 1 }, 0);
            firstTarget = core->_hoverPathTarget;

            {
                std::unique_lock lock{ state->mutex };
                VERIFY_IS_TRUE(state->condition.wait_for(lock, std::chrono::seconds{ 2 }, [&]() { return state->started == 1; }));
            }

            const auto destroyStarted = std::chrono::steady_clock::now();
            core = nullptr;
            VERIFY_IS_TRUE(std::chrono::steady_clock::now() - destroyStarted < std::chrono::seconds{ 1 },
                           L"Closing a control never waits for its blocked filesystem probe.");
        }
        VERIFY_IS_TRUE(firstTarget.expired(),
                       L"The shared worker retains only a weak identity for an in-flight control.");

        for (uint64_t requestId = 2; requestId < 18; ++requestId)
        {
            auto [settings, connection] = _createSettingsAndConnection();
            auto core = createCore(*settings, *connection);
            core->_hoverPathRequestId = requestId;
            core->_hoverPathPendingOrActive.store(true, std::memory_order_relaxed);
            core->_queueHoverPathRequest({ .requestId = requestId }, 0);
            std::weak_ptr<Control::implementation::HoverPathWorker::Target> target = core->_hoverPathTarget;
            core = nullptr;

            VERIFY_IS_TRUE(target.expired());
            VERIFY_ARE_EQUAL(pool, worker._pool);
            VERIFY_ARE_EQUAL(work, worker._work);
            VERIFY_ARE_EQUAL(size_t{ 0 }, worker._pendingCountForTests(),
                             L"Destroyed controls remove their bounded pending entry.");
        }

        {
            const std::lock_guard guard{ state->mutex };
            state->release = true;
        }
        state->condition.notify_all();
        VERIFY_IS_TRUE(worker._waitForIdleForTests(std::chrono::seconds{ 2 }));
        {
            const std::lock_guard guard{ state->mutex };
            VERIFY_ARE_EQUAL(size_t{ 1 }, state->started,
                             L"Repeated control creation reuses the one blocked process-wide service.");
        }
        VERIFY_ARE_EQUAL(size_t{ 1 }, worker._maxActiveProbeCountForTests());
    }

    void ControlCoreTests::TestHoverPathRefreshAfterOutputInvalidation()
    {
        static std::atomic<uint64_t> counter{ 0 };
        const auto scratchRoot = std::filesystem::current_path() /
                                 fmt::format(FMT_COMPILE(L"ControlHoverPaths-{}-{}-{}"),
                                             ::GetCurrentProcessId(),
                                             ::GetTickCount64(),
                                             counter.fetch_add(1, std::memory_order_relaxed));
        std::filesystem::create_directories(scratchRoot);
        const auto filePath = scratchRoot / L"README";
        {
            std::ofstream file{ filePath };
            file << "test";
        }
        auto cleanupScratch = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(scratchRoot, error);
        });

        auto expectedUri = filePath.lexically_normal().native();
        std::replace(expectedUri.begin(), expectedUri.end(), L'\\', L'/');
        expectedUri.insert(0, L"file:///");

        auto& worker = Control::implementation::HoverPathWorker::Instance();
        VERIFY_IS_TRUE(worker._waitForIdleForTests(std::chrono::seconds{ 2 }));
        struct State
        {
            std::mutex mutex;
            std::condition_variable condition;
            size_t calls = 0;
            bool firstStarted = false;
            bool releaseFirst = false;
            std::optional<::Microsoft::Terminal::Core::Terminal::HoverPathRequest> firstRequest;
        };
        const auto state = std::make_shared<State>();
        worker._setResolverForTests(
            [state](const auto& request, auto& cache) -> std::optional<::Microsoft::Terminal::Core::Terminal::HoverPathResult> {
                {
                    std::unique_lock lock{ state->mutex };
                    ++state->calls;
                    if (state->calls == 1)
                    {
                        state->firstRequest = request;
                        state->firstStarted = true;
                        state->condition.notify_all();
                        state->condition.wait(lock, [&]() { return state->releaseFirst; });
                        return ::Microsoft::Terminal::Core::Terminal::HoverPathResult{
                            .candidateIndex = 0,
                            .uri = L"file:///stale-pre-output",
                        };
                    }
                }
                return ::Microsoft::Terminal::Core::Terminal::ResolveHoverPathRequest(request, &cache);
            });
        auto restoreResolver = wil::scope_exit([&]() {
            {
                const std::lock_guard guard{ state->mutex };
                state->releaseFirst = true;
            }
            state->condition.notify_all();
            worker._waitForIdleForTests(std::chrono::seconds{ 2 });
            worker._resetResolverForTests();
        });

        auto [settings, connection] = _createSettingsAndConnection();
        auto core = createCore(*settings, *connection);
        _standardInit(core);
        {
            const auto lock = core->_terminal->LockForWriting();
            core->_terminal->SetWorkingDirectory(scratchRoot.native());
        }
        connection->WriteInput(winrt_wstring_to_array_view(L"README"));

        std::mutex appliedMutex;
        std::condition_variable appliedCondition;
        std::vector<std::wstring> appliedUris;
        core->HoveredHyperlinkChanged([&](auto&&, auto&&) {
            const auto uri = std::wstring{ core->HoveredUriText() };
            if (!uri.empty())
            {
                const std::lock_guard guard{ appliedMutex };
                appliedUris.emplace_back(uri);
                appliedCondition.notify_all();
            }
        });

        const til::point hoveredCell{ 2, 0 };
        core->SetHoveredCell(hoveredCell.to_core_point());
        {
            std::unique_lock lock{ state->mutex };
            VERIFY_IS_TRUE(state->condition.wait_for(lock, std::chrono::seconds{ 2 }, [&]() { return state->firstStarted; }));
        }

        const auto preOutputRequestId = core->_hoverPathRequestId;
        const auto preOutputMutationGeneration = core->_hoverPathMutationGeneration.load(std::memory_order_acquire);
        const auto outputMutationGeneration = core->_hoverPathMutationGeneration.fetch_add(1, std::memory_order_acq_rel) + 1;
        std::optional<::Microsoft::Terminal::Core::Terminal::HoverPathRequest> staleRequest;
        {
            const std::lock_guard guard{ state->mutex };
            staleRequest = state->firstRequest;
        }
        VERIFY_IS_TRUE(staleRequest.has_value());
        core->_completeHoverPathRequest(*staleRequest,
                                        ::Microsoft::Terminal::Core::Terminal::HoverPathResult{
                                            .candidateIndex = 0,
                                            .uri = L"file:///stale-pre-output",
                                        },
                                        preOutputMutationGeneration);
        VERIFY_IS_TRUE(core->HoveredUriText().empty(),
                       L"An output generation change rejects a result before queued UI invalidation runs.");

        core->_hoverPathHandledMutationGeneration = outputMutationGeneration;
        core->_invalidateHoverPathRequests();
        VERIFY_IS_TRUE(core->_lastHoveredCell.has_value());
        VERIFY_ARE_EQUAL(hoveredCell, *core->_lastHoveredCell,
                         L"Output invalidation preserves the physical hovered cell.");
        VERIFY_IS_TRUE(core->_hoverPathRequestId > preOutputRequestId);
        VERIFY_IS_TRUE(core->HoveredUriText().empty());

        core->RefreshHoveredCell();
        VERIFY_IS_TRUE(core->_lastHoveredCell.has_value());
        VERIFY_ARE_EQUAL(hoveredCell, *core->_lastHoveredCell);

        {
            const std::lock_guard guard{ state->mutex };
            state->releaseFirst = true;
        }
        state->condition.notify_all();

        {
            std::unique_lock lock{ appliedMutex };
            VERIFY_IS_TRUE(appliedCondition.wait_for(lock, std::chrono::seconds{ 2 }, [&]() { return !appliedUris.empty(); }));
        }
        VERIFY_IS_TRUE(worker._waitForIdleForTests(std::chrono::seconds{ 2 }));

        {
            const std::lock_guard guard{ state->mutex };
            VERIFY_ARE_EQUAL(size_t{ 2 }, state->calls,
                             L"Output-idle refresh requeues the unchanged hovered cell.");
        }
        {
            const std::lock_guard guard{ appliedMutex };
            VERIFY_ARE_EQUAL(size_t{ 1 }, appliedUris.size(),
                             L"The pre-output stale result is never applied.");
            VERIFY_ARE_EQUAL(expectedUri, appliedUris.front());
        }
        VERIFY_ARE_EQUAL(expectedUri, std::wstring{ core->HoveredUriText() },
                         L"The resolved hover link reappears without pointer movement.");

        const auto clickInfo = core->GetHyperlinkInfo(hoveredCell.to_core_point());
        VERIFY_ARE_EQUAL(expectedUri, clickInfo.uri,
                         L"Click-time lookup revalidates and activates the refreshed bare path.");
        VERIFY_IS_TRUE(clickInfo.isAutoDetectedFilePath);

        core->ClearHoveredCell();
        VERIFY_IS_FALSE(core->_lastHoveredCell.has_value(),
                        L"Pointer exit still clears the physical hovered cell.");
    }

    void ControlCoreTests::TestSelectOutputSimple()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        Log::Comment(L"Create ControlCore object");
        auto core = createCore(*settings, *conn);
        VERIFY_IS_NOT_NULL(core);
        _standardInit(core);

        Log::Comment(L"Print some text");

        _writePrompt(conn, L"C:\\Windows");
        conn->WriteInput(winrt_wstring_to_array_view(L"Foo-bar"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;C\x7"));

        conn->WriteInput(winrt_wstring_to_array_view(L"\r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"This is some text     \r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"with varying amounts  \r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"of whitespace         \r\n"));

        _writePrompt(conn, L"C:\\Windows");

        Log::Comment(L"Check the buffer contents");
        const auto& buffer = core->_terminal->GetTextBuffer();
        const auto& cursor = buffer.GetCursor();

        {
            const til::point expectedCursor{ 17, 4 };
            VERIFY_ARE_EQUAL(expectedCursor, cursor.GetPosition());
        }

        VERIFY_IS_FALSE(core->HasSelection());
        core->SelectOutput(true);
        VERIFY_IS_TRUE(core->HasSelection());
        {
            const auto& start = core->_terminal->GetSelectionAnchor();
            const auto& end = core->_terminal->GetSelectionEnd();
            const til::point expectedStart{ 24, 0 }; // The character after the prompt
            const til::point expectedEnd{ 22, 3 }; // x = the end of the text + 1 (exclusive end)
            VERIFY_ARE_EQUAL(expectedStart, start);
            VERIFY_ARE_EQUAL(expectedEnd, end);
        }
    }
    void ControlCoreTests::TestCommandContext()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        Log::Comment(L"Create ControlCore object");
        auto core = createCore(*settings, *conn);
        VERIFY_IS_NOT_NULL(core);
        _standardInit(core);

        Log::Comment(L"Print some text");

        _writePrompt(conn, L"C:\\Windows");
        conn->WriteInput(winrt_wstring_to_array_view(L"Foo-bar"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;C\x7"));

        conn->WriteInput(winrt_wstring_to_array_view(L"\r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"This is some text     \r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"with varying amounts  \r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"of whitespace         \r\n"));

        _writePrompt(conn, L"C:\\Windows");

        Log::Comment(L"Check the command context");

        const WEX::TestExecution::DisableVerifyExceptions disableExceptionsScope;
        {
            auto historyContext{ core->CommandHistory() };
            VERIFY_ARE_EQUAL(1u, historyContext.History().Size());
            VERIFY_ARE_EQUAL(L"", historyContext.CurrentCommandline());
        }

        Log::Comment(L"Write 'Bar' to the command...");
        conn->WriteInput(winrt_wstring_to_array_view(L"Bar"));
        {
            auto historyContext{ core->CommandHistory() };
            // Bar shouldn't be in the history, it should be the current command
            VERIFY_ARE_EQUAL(1u, historyContext.History().Size());
            VERIFY_ARE_EQUAL(L"Bar", historyContext.CurrentCommandline());
        }

        Log::Comment(L"then delete it");
        conn->WriteInput(winrt_wstring_to_array_view(L"\b \b"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\b \b"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\b \b"));
        {
            auto historyContext{ core->CommandHistory() };
            VERIFY_ARE_EQUAL(1u, historyContext.History().Size());
            // The current commandline is now empty
            VERIFY_ARE_EQUAL(L"", historyContext.CurrentCommandline());
        }
    }

    void ControlCoreTests::TestCommandContextWithPwshGhostText()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        Log::Comment(L"Create ControlCore object");
        auto core = createCore(*settings, *conn);
        VERIFY_IS_NOT_NULL(core);
        _standardInit(core);

        Log::Comment(L"Print some text");

        _writePrompt(conn, L"C:\\Windows");
        conn->WriteInput(winrt_wstring_to_array_view(L"Foo-bar"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;C\x7"));

        conn->WriteInput(winrt_wstring_to_array_view(L"\r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"This is some text     \r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"with varying amounts  \r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"of whitespace         \r\n"));

        _writePrompt(conn, L"C:\\Windows");

        Log::Comment(L"Check the command context");

        const WEX::TestExecution::DisableVerifyExceptions disableExceptionsScope;
        {
            auto historyContext{ core->CommandHistory() };
            VERIFY_ARE_EQUAL(1u, historyContext.History().Size());
            VERIFY_ARE_EQUAL(L"", historyContext.CurrentCommandline());
        }

        Log::Comment(L"Write 'BarBar' to the command...");
        conn->WriteInput(winrt_wstring_to_array_view(L"BarBar"));
        {
            auto historyContext{ core->CommandHistory() };
            // BarBar shouldn't be in the history, it should be the current command
            VERIFY_ARE_EQUAL(1u, historyContext.History().Size());
            VERIFY_ARE_EQUAL(L"BarBar", historyContext.CurrentCommandline());
        }

        Log::Comment(L"then move the cursor to the left");
        // This emulates the state the buffer is in when pwsh does its "ghost
        // text" thing. We don't want to include all that ghost text in the
        // current commandline.
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b[D"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b[D"));
        {
            auto historyContext{ core->CommandHistory() };
            VERIFY_ARE_EQUAL(1u, historyContext.History().Size());
            // The current commandline is only the text to the left of the cursor
            auto curr{ historyContext.CurrentCommandline() };
            VERIFY_ARE_EQUAL(4u, curr.size());
            VERIFY_ARE_EQUAL(L"BarB", curr);
        }
    }

    void ControlCoreTests::TestSelectOutputScrolling()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        Log::Comment(L"Create ControlCore object");
        auto core = createCore(*settings, *conn);
        VERIFY_IS_NOT_NULL(core);
        _standardInit(core);

        Log::Comment(L"Print some text");

        _writePrompt(conn, L"C:\\Windows"); // row 0
        conn->WriteInput(winrt_wstring_to_array_view(L"Foo-bar")); // row 0
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;C\x7"));

        conn->WriteInput(winrt_wstring_to_array_view(L"\r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"This is some text     \r\n")); // row 1
        conn->WriteInput(winrt_wstring_to_array_view(L"with varying amounts  \r\n")); // row 2
        conn->WriteInput(winrt_wstring_to_array_view(L"of whitespace         \r\n")); // row 3

        _writePrompt(conn, L"C:\\Windows"); // row 4
        conn->WriteInput(winrt_wstring_to_array_view(L"gci"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;C\x7"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\r\n"));

        // enough to scroll
        for (auto i = 0; i < 30; i++) // row 5-34
        {
            conn->WriteInput(winrt_wstring_to_array_view(L"-a--- 2/8/2024  9:47 README\r\n"));
        }

        _writePrompt(conn, L"C:\\Windows");

        Log::Comment(L"Check the buffer contents");
        const auto& buffer = core->_terminal->GetTextBuffer();
        const auto& cursor = buffer.GetCursor();

        {
            const til::point expectedCursor{ 17, 35 };
            VERIFY_ARE_EQUAL(expectedCursor, cursor.GetPosition());
        }

        VERIFY_IS_FALSE(core->HasSelection());

        // The second mark is the first one we'll see
        core->SelectOutput(true);
        VERIFY_IS_TRUE(core->HasSelection());
        {
            const auto& start = core->_terminal->GetSelectionAnchor();
            const auto& end = core->_terminal->GetSelectionEnd();
            const til::point expectedStart{ 20, 4 }; // The character after the prompt
            const til::point expectedEnd{ 27, 34 }; // x = the end of the text + 1 (exclusive end)
            VERIFY_ARE_EQUAL(expectedStart, start);
            VERIFY_ARE_EQUAL(expectedEnd, end);
        }
        core->SelectOutput(true);
        VERIFY_IS_TRUE(core->HasSelection());
        {
            const auto& start = core->_terminal->GetSelectionAnchor();
            const auto& end = core->_terminal->GetSelectionEnd();
            const til::point expectedStart{ 24, 0 }; // The character after the prompt
            const til::point expectedEnd{ 22, 3 }; // x = the end of the text + 1 (exclusive end)
            VERIFY_ARE_EQUAL(expectedStart, start);
            VERIFY_ARE_EQUAL(expectedEnd, end);
        }
    }

    void ControlCoreTests::TestSelectOutputExactWrap()
    {
        // Just like the TestSelectOutputScrolling test, but these lines will
        // exactly wrap to the right edge of the buffer, to catch an edge case
        // present in `ControlCore::_selectSpan`
        auto [settings, conn] = _createSettingsAndConnection();
        Log::Comment(L"Create ControlCore object");
        auto core = createCore(*settings, *conn);
        VERIFY_IS_NOT_NULL(core);
        _standardInit(core);

        Log::Comment(L"Print some text");

        _writePrompt(conn, L"C:\\Windows"); // row 0
        conn->WriteInput(winrt_wstring_to_array_view(L"Foo-bar")); // row 0
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;C\x7"));

        conn->WriteInput(winrt_wstring_to_array_view(L"\r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"This is some text     \r\n")); // row 1
        conn->WriteInput(winrt_wstring_to_array_view(L"with varying amounts  \r\n")); // row 2
        conn->WriteInput(winrt_wstring_to_array_view(L"of whitespace         \r\n")); // row 3

        _writePrompt(conn, L"C:\\Windows"); // row 4
        conn->WriteInput(winrt_wstring_to_array_view(L"gci"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;C\x7"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\r\n"));

        // enough to scroll
        for (auto i = 0; i < 30; i++) // row 5-35
        {
            conn->WriteInput(winrt_wstring_to_array_view(L"-a--- 2/8/2024  9:47 README.md\r\n"));
        }

        _writePrompt(conn, L"C:\\Windows");

        Log::Comment(L"Check the buffer contents");
        const auto& buffer = core->_terminal->GetTextBuffer();
        const auto& cursor = buffer.GetCursor();

        {
            const til::point expectedCursor{ 17, 35 };
            VERIFY_ARE_EQUAL(expectedCursor, cursor.GetPosition());
        }

        VERIFY_IS_FALSE(core->HasSelection());
        // The second mark is the first one we'll see
        core->SelectOutput(true);
        VERIFY_IS_TRUE(core->HasSelection());
        {
            const auto& start = core->_terminal->GetSelectionAnchor();
            const auto& end = core->_terminal->GetSelectionEnd();
            const til::point expectedStart{ 20, 4 }; // The character after the prompt
            const til::point expectedEnd{ 30, 34 }; // x = the end of the text + 1 (exclusive end)
            VERIFY_ARE_EQUAL(expectedStart, start);
            VERIFY_ARE_EQUAL(expectedEnd, end);
        }
        core->SelectOutput(true);
        VERIFY_IS_TRUE(core->HasSelection());
        {
            const auto& start = core->_terminal->GetSelectionAnchor();
            const auto& end = core->_terminal->GetSelectionEnd();
            const til::point expectedStart{ 24, 0 }; // The character after the prompt
            const til::point expectedEnd{ 22, 3 }; // x = the end of the text + 1 (exclusive end)
            VERIFY_ARE_EQUAL(expectedStart, start);
            VERIFY_ARE_EQUAL(expectedEnd, end);
        }
    }

    void ControlCoreTests::TestSimpleClickSelection()
    {
        // Create a simple selection with the mouse, then click somewhere else,
        // and confirm the selection got updated.

        auto [settings, conn] = _createSettingsAndConnection();
        Log::Comment(L"Create ControlCore object");
        auto core = createCore(*settings, *conn);
        VERIFY_IS_NOT_NULL(core);
        _standardInit(core);

        // Here, we're using the UpdateSelectionMarkers as a stand-in to check
        // if the selection got updated with the renderer. Standing up a whole
        // dummy renderer for this test would be not very ergonomic. Instead, we
        // are relying on ControlCore::_updateSelectionUI both
        // TriggerSelection()'ing and also rasing this event
        bool expectedSelectionUpdate = false;
        bool gotSelectionUpdate = false;
        core->UpdateSelectionMarkers([&](auto&& /*sender*/, auto&& /*args*/) {
            VERIFY_IS_TRUE(expectedSelectionUpdate);
            expectedSelectionUpdate = false;
            gotSelectionUpdate = true;
        });

        auto needToCopy = false;
        expectedSelectionUpdate = true;
        core->LeftClickOnTerminal(til::point{ 1, 1 },
                                  1,
                                  false,
                                  true,
                                  false,
                                  needToCopy);

        VERIFY_IS_TRUE(core->HasSelection());
        {
            const auto& start = core->_terminal->GetSelectionAnchor();
            const auto& end = core->_terminal->GetSelectionEnd();
            const til::point expectedStart{ 1, 1 };
            const til::point expectedEnd{ 1, 1 };
            VERIFY_ARE_EQUAL(expectedStart, start);
            VERIFY_ARE_EQUAL(expectedEnd, end);
        }
        VERIFY_IS_TRUE(gotSelectionUpdate);

        expectedSelectionUpdate = true;
        core->LeftClickOnTerminal(til::point{ 1, 2 },
                                  1,
                                  false,
                                  true,
                                  false,
                                  needToCopy);

        VERIFY_IS_TRUE(core->HasSelection());
        {
            const auto& start = core->_terminal->GetSelectionAnchor();
            const auto& end = core->_terminal->GetSelectionEnd();
            const til::point expectedStart{ 1, 1 };
            const til::point expectedEnd{ 2, 2 };
            VERIFY_ARE_EQUAL(expectedStart, start);
            VERIFY_ARE_EQUAL(expectedEnd, end);
        }
        VERIFY_IS_TRUE(gotSelectionUpdate);
    }
}
