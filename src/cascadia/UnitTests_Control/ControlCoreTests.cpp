// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "../TerminalControl/EventArgs.h"
#include "../TerminalControl/ControlCore.h"
#include "MockControlSettings.h"
#include "MockConnection.h"
#include "../../inc/TestUtils.h"
#include "../inc/OmniDirectoryUtils.h"

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
        TEST_METHOD(OmniCurrentColorsReadRendererOverrides);

        TEST_METHOD(TestFreeAfterClose);

        TEST_METHOD(TestFontInitializedInCtor);

        TEST_METHOD(TestClearScrollback);
        TEST_METHOD(TestClearScreen);
        TEST_METHOD(TestClearAll);
        TEST_METHOD(TestReadEntireBuffer);
        TEST_METHOD(TestReadBufferTail);
        TEST_METHOD(TestNavigateBufferMatch);
        TEST_METHOD(TestNavigateBufferMatchAfterReflow);
        TEST_METHOD(TestNavigateBufferMatchAfterRotation);
        TEST_METHOD(TestNavigateBufferMatchDuringOutput);
        TEST_METHOD(TestNavigateBufferMatchDirectoryMetadata);
        TEST_METHOD(TestNavigateBufferMatchResizeMatrix);
        TEST_METHOD(TestNavigateBufferMatchChunkBoundary);
        TEST_METHOD(TestNavigateBufferMatchAtRightEdge);
        TEST_METHOD(TestNavigateMetadataConversationWithoutLiteral);
        TEST_METHOD(TestNavigateMetadataConversationExpansion);
        TEST_METHOD(TestNavigateMetadataPrioritizesDistinctContext);
        TEST_METHOD(TestNavigateBufferMatchOutputInterleavings);
        TEST_METHOD(TestNavigateBufferMatchCancellationAndBufferReturn);
        TEST_METHOD(TestNavigateBufferMatchRejectsExpiredAndAmbiguous);
        TEST_METHOD(TestReadLastPromptBoundedNoOutput);
        TEST_METHOD(TestReadLastPromptBoundedSkipsUnfinished);
        TEST_METHOD(TestReadLastPromptBoundedWrapAndLimits);
        TEST_METHOD(TestReadLastPromptBoundedUnicode);
        TEST_METHOD(TestReadLastPromptBoundedInvalidLimits);

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

        std::wstring _selectedText(const winrt::com_ptr<Control::implementation::ControlCore>& core)
        {
            const auto lock = core->_terminal->LockForReading();
            const auto& buffer = core->_terminal->GetTextBuffer();
            return buffer.GetPlainText(TextBuffer::CopyRequest{
                buffer, core->_terminal->GetSelectionAnchor(), core->_terminal->GetSelectionEnd(), false, false, false, false, true });
        }
    };

    void ControlCoreTests::ComPtrSettings()
    {
        Log::Comment(L"Just make sure we can instantiate a settings obj in a com_ptr");
        auto settings = winrt::make_self<MockControlSettings>();

        Log::Comment(L"Verify literally any setting, it doesn't matter");
        VERIFY_ARE_EQUAL(DEFAULT_FOREGROUND, settings->DefaultForeground());
    }

    void ControlCoreTests::OmniCurrentColorsReadRendererOverrides()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        const auto core = createCore(*settings, *conn);
        _standardInit(core);
        const auto before = core->CurrentColorScheme();
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]10;#123456\x07\x1b]11;#234567\x07\x1b]4;1;#345678\x07"));
        const auto actual = core->CurrentColorScheme();
        VERIFY_ARE_EQUAL(20u, actual.size());
        VERIFY_IS_TRUE((actual[0] == til::color{ 0x12, 0x34, 0x56 }));
        VERIFY_IS_TRUE((actual[1] == til::color{ 0x23, 0x45, 0x67 }));
        VERIFY_IS_TRUE((actual[5] == til::color{ 0x34, 0x56, 0x78 }));
        VERIFY_IS_TRUE(actual[4] == before[4]);
        core->SetOverrideColorScheme(settings.as<Core::ICoreScheme>());
        const auto reset = core->CurrentColorScheme();
        for (uint32_t i = 0; i < before.size(); ++i)
            VERIFY_IS_TRUE(before[i] == reset[i]);
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
        const auto state = core.as<Control::ICoreState>();
        VERIFY_ARE_EQUAL(30, state.ViewWidth());
        VERIFY_ARE_EQUAL(20, state.ViewHeight());

        core->SizeChanged(450, 380);
        VERIFY_ARE_EQUAL(50, core->_terminal->GetViewport().Width());
        VERIFY_ARE_EQUAL(50, state.ViewWidth());
        VERIFY_ARE_EQUAL(20, state.ViewHeight());
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

    void ControlCoreTests::TestReadBufferTail()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        auto core = createCore(*settings, *conn);
        VERIFY_IS_NOT_NULL(core);
        _standardInit(core);

        for (auto i = 0; i < 200; ++i)
        {
            conn->WriteInput(winrt_wstring_to_array_view(fmt::format(L"line-{:03}\r\n", i)));
        }

        VERIFY_ARE_EQUAL(L"line-199\r\n", core->ReadBufferTail(1, 100));
        VERIFY_ARE_EQUAL(
            L"line-197\r\nline-198\r\nline-199\r\n",
            core->ReadBufferTail(3, 100));
        VERIFY_ARE_EQUAL(L"-199\r\n", core->ReadBufferTail(10, 6));

        VERIFY_THROWS_SPECIFIC(
            core->ReadBufferTail(0, 100),
            wil::ResultException,
            [](const wil::ResultException& e) { return e.GetErrorCode() == E_INVALIDARG; });
        VERIFY_THROWS_SPECIFIC(
            core->ReadBufferTail(1, 0),
            wil::ResultException,
            [](const wil::ResultException& e) { return e.GetErrorCode() == E_INVALIDARG; });

        auto [unicodeSettings, unicodeConn] = _createSettingsAndConnection();
        auto unicodeCore = createCore(*unicodeSettings, *unicodeConn);
        VERIFY_IS_NOT_NULL(unicodeCore);
        _standardInit(unicodeCore);
        unicodeConn->WriteInput(winrt_wstring_to_array_view(L"A\U0001F366B\r\n"));
        VERIFY_ARE_EQUAL(L"\U0001F366B\r\n", unicodeCore->ReadBufferTail(1, 4));
    }

    void ControlCoreTests::TestNavigateBufferMatch()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        auto core = createCore(*settings, *conn);
        VERIFY_IS_TRUE(core->FindTextMatches({ L"acrylic" }, [] { return false; }).empty());
        _standardInit(core);
        conn->WriteInput(winrt_wstring_to_array_view(L"Discussing acrylic settings\r\n"));
        for (auto i = 0; i < 60; ++i)
            conn->WriteInput(winrt_wstring_to_array_view(L"unrelated output\r\n"));
        const auto matches = core->FindTextMatches({ L"ACRYLIC" }, [] { return false; });
        VERIFY_ARE_EQUAL(size_t{ 1 }, matches.size());
        VERIFY_ARE_EQUAL(L"Discussing acrylic settings", matches[0].AnchorText);
        VERIFY_IS_FALSE(core->_terminal->IsSelectionActive());
        VERIFY_IS_TRUE(core->_terminal->GetViewport().Top() > 0);
        VERIFY_IS_TRUE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
        VERIFY_IS_TRUE(core->RevealTextMatch(matches[0].Id));
        VERIFY_IS_TRUE(core->_terminal->IsSelectionActive());
        VERIFY_ARE_EQUAL(0, core->_terminal->GetViewport().Top());
        VERIFY_IS_FALSE(core->RevealTextMatch(matches[0].Id + 1));
        VERIFY_IS_TRUE(core->FindTextMatches({ L"acrylic" }, [] { return true; }).empty());
    }

    void ControlCoreTests::TestNavigateBufferMatchAfterReflow()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        auto core = createCore(*settings, *conn);
        _standardInit(core);
        conn->WriteInput(winrt_wstring_to_array_view(L"12345678901234567890123456789\U0001F600 discussing acrylic settings across wrapped rows.\r\n"));
        const auto matches = core->FindTextMatches({ L"acrylic settings" }, [] { return false; });
        VERIFY_ARE_EQUAL(size_t{ 1 }, matches.size());
        core->SizeChanged(450, 380);
        conn->WriteInput(winrt_wstring_to_array_view(L"more output\r\n"));
        VERIFY_IS_FALSE(core->RevealTextMatch(matches[0].Id));
        VERIFY_IS_TRUE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
        VERIFY_IS_TRUE(core->RevealTextMatch(matches[0].Id));
        core->SizeChanged(270, 380);
        VERIFY_IS_TRUE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
        conn->WriteInput(winrt_wstring_to_array_view(L"unrelated output after preparation\r\n"));
        VERIFY_IS_TRUE(core->RevealTextMatch(matches[0].Id));
    }

    void ControlCoreTests::TestNavigateBufferMatchRejectsExpiredAndAmbiguous()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        auto core = createCore(*settings, *conn);
        _standardInit(core);
        conn->WriteInput(winrt_wstring_to_array_view(L"acrylic discussion\r\n"));
        auto matches = core->FindTextMatches({ L"acrylic" }, [] { return false; });
        conn->WriteInput(winrt_wstring_to_array_view(L"acrylic discussion\r\n"));
        VERIFY_IS_TRUE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
        VERIFY_IS_TRUE(core->RevealTextMatch(matches[0].Id));
        VERIFY_ARE_EQUAL(0, core->_terminal->GetSelectionAnchor().y);
        matches = core->FindTextMatches({ L"acrylic" }, [] { return false; });
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b[?1049h"));
        VERIFY_IS_FALSE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b[?1049l"));
        core->ClearBuffer(Control::ClearBufferType::All);
        VERIFY_IS_FALSE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
        VERIFY_IS_FALSE(core->RevealTextMatch(matches[0].Id));
    }

    void ControlCoreTests::TestNavigateBufferMatchAfterRotation()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        settings->HistorySize(40);
        auto core = createCore(*settings, *conn);
        _standardInit(core);
        for (auto i = 0; i < 40; ++i)
            conn->WriteInput(winrt_wstring_to_array_view(L"earlier output\r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\u8ba8\u8bba acrylic \U0001F600\r\n"));
        const auto matches = core->FindTextMatches({ L"\u8ba8\u8bba" }, [] { return false; });
        VERIFY_ARE_EQUAL(size_t{ 1 }, matches.size());
        for (auto i = 0; i < 50; ++i)
            conn->WriteInput(winrt_wstring_to_array_view(L"later output\r\n"));
        VERIFY_IS_TRUE(core->RevealTextMatch(matches[0].Id));
        VERIFY_IS_TRUE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
        VERIFY_IS_TRUE(core->RevealTextMatch(matches[0].Id));
        VERIFY_IS_TRUE(core->_terminal->GetSelectionAnchor().y < 40);
        VERIFY_ARE_EQUAL(L"\u8ba8\u8bba acrylic \U0001F600", matches[0].AnchorText);
    }

    void ControlCoreTests::TestNavigateBufferMatchDuringOutput()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        settings->HistorySize(40);
        auto core = createCore(*settings, *conn);
        _standardInit(core);
        conn->WriteInput(winrt_wstring_to_array_view(L"acrylic acrylic discussion\r\nacrylic acrylic discussion\r\n"));
        auto matches = core->FindTextMatches({ L"acrylic" }, [] { return false; });
        VERIFY_ARE_EQUAL(size_t{ 1 }, matches.size());
        core->SizeChanged(450, 380);
        VERIFY_IS_TRUE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
        conn->WriteInput(winrt_wstring_to_array_view(L"unrelated output\r\n"));
        VERIFY_IS_TRUE(core->RevealTextMatch(matches[0].Id));
        VERIFY_ARE_EQUAL(1, core->_terminal->GetSelectionAnchor().y);
        VERIFY_ARE_EQUAL(8, core->_terminal->GetSelectionAnchor().x);
        // Recycling the original occurrence must not select a new identical line.
        for (auto i = 0; i < 100; ++i)
            conn->WriteInput(winrt_wstring_to_array_view(L"acrylic acrylic discussion\r\n"));
        VERIFY_IS_FALSE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
        VERIFY_IS_FALSE(core->RevealTextMatch(matches[0].Id));
        matches = core->FindTextMatches({ L"acrylic" }, [] { return false; });
        VERIFY_IS_TRUE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
        // A newer search retires IDs from an earlier request.
        core->FindTextMatches({ L"unrelated" }, [] { return false; });
        VERIFY_IS_FALSE(core->RevealTextMatch(matches[0].Id));
    }

    void ControlCoreTests::TestNavigateBufferMatchDirectoryMetadata()
    {
        using namespace ::Microsoft::Terminal::Omni;
        VERIFY_IS_TRUE(DirectoryMatches(L"C:\\Users\\test\\hello_world", L"hello_world"));
        VERIFY_IS_TRUE(DirectoryMatches(L"C:\\Users\\test\\hello_world", L"c:/users/test/HELLO_WORLD/"));
        VERIFY_IS_FALSE(DirectoryMatches(L"C:\\Users\\test\\hello_world2", L"hello_world"));
        VERIFY_IS_FALSE(DirectoryMatches(L"C:\\Users\\test\\hello_world", L"test\\hello_world"));
        VERIFY_IS_TRUE(DirectoryMatches(L"/home/test/hello_world/", L"hello_world"));
        VERIFY_IS_FALSE(DirectoryMatches(L"/home/test/Hello_World", L"hello_world"));
        VERIFY_IS_FALSE(DirectoryMatches(L"/home/test/hello_world", L"C:\\home\\test\\hello_world"));
        VERIFY_IS_TRUE(DirectoryMatches(L"\\\\server\\share\\hello_world", L"//server/share/hello_world"));
        VERIFY_IS_TRUE(DirectorySelectorInPrompt(L"go to pane in hello_world folder", L"hello_world"));
        VERIFY_IS_FALSE(DirectorySelectorInPrompt(L"go to pane in hello_world folder", L"C:\\work\\hello_world"));
        VERIFY_IS_TRUE(DirectorySelectorInPrompt(L"go to C:/work/hello_world/", L"C:\\work\\hello_world"));
        VERIFY_IS_FALSE(DirectorySelectorInPrompt(L"go to Hello_World", L"hello_world", true));
        // Two windows/WSL paths with the same leaf remain two candidates.
        size_t matches{};
        for (const auto path : { L"C:\\work\\hello_world", L"/home/test/hello_world" })
            matches += DirectoryMatches(path, L"hello_world") ? 1 : 0;
        VERIFY_ARE_EQUAL(size_t{ 2 }, matches);
        VERIFY_IS_TRUE(DirectoryMatches(L"C:\\", L"c:/"));
        VERIFY_IS_FALSE(DirectoryMatches(L"D:\\", L"C:\\"));
        VERIFY_IS_TRUE(DirectoryMatches(L"/", L"/"));
        VERIFY_IS_TRUE(DirectoryMatches(L"\\\\server\\share\\project space\\", L"project space"));
        VERIFY_IS_TRUE(DirectoryMatches(L"C:\\work\\\u9879\u76ee", L"\u9879\u76ee"));
        VERIFY_IS_FALSE(DirectoryMatches(L"C:\\work\\not_hello_world", L"hello_world"));
        VERIFY_IS_FALSE(DirectoryMatches(L"C:\\work\\hello_world", L"C:\\other\\hello_world"));
        VERIFY_IS_FALSE(DirectoryMatches(L"C:\\work\\hello_world", L"%USERPROFILE%\\hello_world"));
        VERIFY_IS_FALSE(DirectoryMatches(L"C:\\work\\hello_world", L"work"));
        VERIFY_IS_FALSE(DirectoryMatches(L"C:\\work\\hello_world", L".."));
        VERIFY_IS_TRUE(DirectorySelectorInPrompt(L"go to \"C:\\work\\project space\"", L"C:\\work\\project space"));
        VERIFY_IS_FALSE(DirectorySelectorInPrompt(L"go to missing_folder", L"hello_world"));
    }

    void ControlCoreTests::TestNavigateBufferMatchResizeMatrix()
    {
        const std::wstring query = L"target \u754c\U0001F600";
        for (const auto prefix : { 0, 7, 29 })
        {
            auto [settings, conn] = _createSettingsAndConnection();
            auto core = createCore(*settings, *conn);
            _standardInit(core);
            conn->WriteInput(winrt_wstring_to_array_view(std::wstring(prefix, L'x') + query + L" stable suffix\r\n"));
            const auto matches = core->FindTextMatches({ hstring{ query } }, [] { return false; });
            VERIFY_ARE_EQUAL(size_t{ 1 }, matches.size());
            for (const auto width : { 9, 19, 47, 12, 31, 30 })
            {
                core->SizeChanged(static_cast<float>(width * 9), 380.f);
                VERIFY_IS_TRUE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
                conn->WriteInput(winrt_wstring_to_array_view(L"unrelated\r\n"));
                VERIFY_IS_TRUE(core->RevealTextMatch(matches[0].Id));
                VERIFY_ARE_EQUAL(query, _selectedText(core));
            }
        }
    }

    void ControlCoreTests::TestNavigateBufferMatchChunkBoundary()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        settings->HistorySize(512);
        auto core = createCore(*settings, *conn);
        _standardInit(core);
        for (auto i = 0; i < 275; ++i)
            conn->WriteInput(winrt_wstring_to_array_view(L"filler\r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(std::wstring(25, L'x') + L"needle[1].* suffix\r\n"));
        for (auto i = 0; i < 40; ++i)
            conn->WriteInput(winrt_wstring_to_array_view(L"needle11anything\r\n"));
        VERIFY_IS_TRUE(core->FindTextMatches({ L"needle[2].*" }, [] { return false; }).empty());
        const auto matches = core->FindTextMatches({ L"needle[1].*" }, [] { return false; });
        VERIFY_ARE_EQUAL(size_t{ 1 }, matches.size());
        VERIFY_IS_TRUE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
        VERIFY_IS_TRUE(core->RevealTextMatch(matches[0].Id));
        VERIFY_ARE_EQUAL(275, core->_terminal->GetSelectionAnchor().y);
        VERIFY_ARE_EQUAL(L"needle[1].*", _selectedText(core));
    }

    void ControlCoreTests::TestNavigateBufferMatchOutputInterleavings()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        settings->HistorySize(200);
        auto core = createCore(*settings, *conn);
        _standardInit(core);
        conn->WriteInput(winrt_wstring_to_array_view(L"original target discussion\r\n"));
        const auto matches = core->FindTextMatches({ L"target" }, [] { return false; });
        VERIFY_ARE_EQUAL(size_t{ 1 }, matches.size());
        for (auto i = 0; i < 80; ++i)
        {
            conn->WriteInput(winrt_wstring_to_array_view(L"building unrelated file\r\n"));
            VERIFY_IS_TRUE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
            conn->WriteInput(winrt_wstring_to_array_view(L"original target discussion\r\n"));
            VERIFY_IS_TRUE(core->RevealTextMatch(matches[0].Id));
            VERIFY_ARE_EQUAL(0, core->_terminal->GetSelectionAnchor().y);
            VERIFY_ARE_EQUAL(L"target", _selectedText(core));
        }
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b[3J"));
        VERIFY_IS_FALSE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
    }

    void ControlCoreTests::TestNavigateMetadataConversationWithoutLiteral()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        auto core = createCore(*settings, *conn);
        _standardInit(core);
        conn->WriteInput(winrt_wstring_to_array_view(L"user: what does this program do?\r\nassistant: It watches files and rebuilds.\r\nPS> "));
        VERIFY_IS_TRUE(core->FindTextMatches({ L"Rust app" }, [] { return false; }).empty());
        const auto matches = core->FindTextMatches({ L"Rust app" }, [] { return false; }, 128);
        const auto match = std::find_if(matches.begin(), matches.end(), [](const auto& value) {
            return std::wstring_view{ value.AnchorText }.find(L"what does this program do?") != std::wstring_view::npos;
        });
        VERIFY_IS_TRUE(match != matches.end());
        VERIFY_IS_TRUE(std::wstring_view{ match->Text }.find(L"watches files") != std::wstring_view::npos);
        core->SizeChanged(450.f, 380.f);
        VERIFY_IS_TRUE(core->PrepareTextMatch(match->Id, [] { return false; }));
        VERIFY_IS_TRUE(core->RevealTextMatch(match->Id));
        VERIFY_ARE_EQUAL(0, core->_terminal->GetSelectionAnchor().y);
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b[Hdifferent question"));
        VERIFY_IS_FALSE(core->PrepareTextMatch(match->Id, [] { return false; }));
    }

    void ControlCoreTests::TestNavigateMetadataConversationExpansion()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        settings->HistorySize(512);
        auto core = createCore(*settings, *conn);
        _standardInit(core);
        conn->WriteInput(winrt_wstring_to_array_view(L"user: what does it do?\r\nassistant: It watches files.\r\n"));
        for (auto i = 0; i < 200; ++i)
            conn->WriteInput(winrt_wstring_to_array_view(L"unrelated build progress\r\n"));
        const auto first = core->FindTextMatches({ L"Rust app" }, [] { return false; }, 128);
        VERIFY_IS_TRUE(std::none_of(first.begin(), first.end(), [](const auto& value) {
            return std::wstring_view{ value.Text }.find(L"watches files") != std::wstring_view::npos;
        }));
        const auto expanded = core->FindTextMatches({ L"Rust app" }, [] { return false; }, 512);
        const auto match = std::find_if(expanded.begin(), expanded.end(), [](const auto& value) {
            return std::wstring_view{ value.Text }.find(L"watches files") != std::wstring_view::npos;
        });
        VERIFY_IS_TRUE(match != expanded.end());
        VERIFY_IS_TRUE(core->PrepareTextMatch(match->Id, [] { return false; }));
        VERIFY_IS_TRUE(core->RevealTextMatch(match->Id));
        VERIFY_ARE_EQUAL(0, core->_terminal->GetViewport().Top());
        if (!first.empty())
            VERIFY_IS_FALSE(core->RevealTextMatch(first[0].Id));
    }
    void ControlCoreTests::TestNavigateMetadataPrioritizesDistinctContext()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        settings->HistorySize(200);
        auto core = createCore(*settings, *conn);
        _standardInit(core);
        conn->WriteInput(winrt_wstring_to_array_view(L"user: why is revenue wrong?\r\nassistant: Acme rows must be added.\r\n"));
        for (auto i = 0; i < 48; ++i)
            conn->WriteInput(winrt_wstring_to_array_view(fmt::format(L"review note {}: saved\r\n", i)));
        conn->WriteInput(winrt_wstring_to_array_view(L"PS> "));
        const auto matches = core->FindTextMatches({ L"MyRustApp" }, [] { return false; }, 128);
        const auto end = matches.begin() + std::min(size_t{ 4 }, matches.size());
        const auto match = std::find_if(matches.begin(), end, [](const auto& value) {
            return std::wstring_view{ value.Text }.find(L"Acme rows must be added") != std::wstring_view::npos;
        });
        VERIFY_IS_TRUE(match != end);
        VERIFY_IS_TRUE(core->PrepareTextMatch(match->Id, [] { return false; }));
        VERIFY_IS_TRUE(core->RevealTextMatch(match->Id));
        VERIFY_ARE_EQUAL(0, core->_terminal->GetViewport().Top());
    }

    void ControlCoreTests::TestNavigateBufferMatchAtRightEdge()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        auto core = createCore(*settings, *conn);
        _standardInit(core);
        conn->WriteInput(winrt_wstring_to_array_view(std::wstring(24, L'x') + L"target\r\n"));
        const auto matches = core->FindTextMatches({ L"target" }, [] { return false; });
        VERIFY_ARE_EQUAL(size_t{ 1 }, matches.size());
        conn->WriteInput(winrt_wstring_to_array_view(L"unrelated next logical line\r\n"));
        VERIFY_IS_TRUE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
        VERIFY_IS_TRUE(core->RevealTextMatch(matches[0].Id));
        VERIFY_ARE_EQUAL(til::point(24, 0), core->_terminal->GetSelectionAnchor());
        VERIFY_ARE_EQUAL(til::point(30, 0), core->_terminal->GetSelectionEnd());
        VERIFY_ARE_EQUAL(L"target", _selectedText(core));
    }

    void ControlCoreTests::TestNavigateBufferMatchCancellationAndBufferReturn()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        auto core = createCore(*settings, *conn);
        _standardInit(core);
        conn->WriteInput(winrt_wstring_to_array_view(L"main target discussion\r\n"));
        const auto matches = core->FindTextMatches({ L"target" }, [] { return false; });
        VERIFY_ARE_EQUAL(size_t{ 1 }, matches.size());
        VERIFY_IS_FALSE(core->PrepareTextMatch(matches[0].Id, [] { return true; }));
        VERIFY_IS_FALSE(core->_terminal->IsSelectionActive());
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b[?1049hmain target discussion\r\n"));
        VERIFY_IS_FALSE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
        VERIFY_IS_FALSE(core->RevealTextMatch(matches[0].Id));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b[?1049l"));
        VERIFY_IS_TRUE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
        VERIFY_IS_TRUE(core->RevealTextMatch(matches[0].Id));
        VERIFY_ARE_EQUAL(L"target", _selectedText(core));
        core->FindTextMatches({ L"absent" }, [] { return false; });
        VERIFY_IS_FALSE(core->PrepareTextMatch(matches[0].Id, [] { return false; }));
        VERIFY_IS_FALSE(core->RevealTextMatch(matches[0].Id));
    }

    static void _writePrompt(const winrt::com_ptr<MockConnection>& conn, const std::wstring_view& path, const std::optional<int32_t> exitCode = std::nullopt)
    {
        conn->WriteInput(winrt_wstring_to_array_view(exitCode ? fmt::format(L"\x1b]133;D;{}\x7", *exitCode) : L"\x1b]133;D\x7"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;A\x7"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]9;9;"));
        conn->WriteInput(winrt_wstring_to_array_view(path));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x7"));
        conn->WriteInput(winrt_wstring_to_array_view(L"PWSH "));
        conn->WriteInput(winrt_wstring_to_array_view(path));
        conn->WriteInput(winrt_wstring_to_array_view(L"> "));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;B\x7"));
    }

    void ControlCoreTests::TestReadLastPromptBoundedNoOutput()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        auto core = createCore(*settings, *conn);
        _standardInit(core);

        VERIFY_IS_TRUE(core->ReadLastPromptBounded(10, 100).empty());
        _writePrompt(conn, L"");
        conn->WriteInput(winrt_wstring_to_array_view(L"cd\x1b]133;C\x7"));
        VERIFY_IS_TRUE(core->ReadLastPromptBounded(10, 100).empty());
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;D;0\x7"));

        const auto marks = core->_terminal->GetMarkExtents();
        VERIFY_ARE_EQUAL(size_t{ 1 }, marks.size());
        VERIFY_IS_TRUE(marks.back().HasCommand());
        VERIFY_IS_FALSE(marks.back().outputEnd.has_value());
        VERIFY_IS_TRUE(marks.back().data.exitCode.has_value());
        VERIFY_ARE_EQUAL(L"cd", core->ReadLastPromptBounded(10, 100));
        VERIFY_ARE_EQUAL(L"c", core->ReadLastPromptBounded(1, 1));
        VERIFY_ARE_EQUAL(L"cd", core->ReadLastPromptBounded(1, 2));
    }

    void ControlCoreTests::TestReadLastPromptBoundedSkipsUnfinished()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        auto core = createCore(*settings, *conn);
        _standardInit(core);

        _writePrompt(conn, L"");
        conn->WriteInput(winrt_wstring_to_array_view(L"cd\x1b]133;C\x7\x1b]133;D;0\x7\r\n"));
        _writePrompt(conn, L"", 0);
        conn->WriteInput(winrt_wstring_to_array_view(L"sleep\x1b]133;C\x7 streaming"));

        const auto marks = core->_terminal->GetMarkExtents();
        VERIFY_ARE_EQUAL(size_t{ 2 }, marks.size());
        VERIFY_IS_TRUE(marks.back().HasCommand());
        VERIFY_IS_TRUE(marks.back().HasOutput());
        VERIFY_IS_FALSE(marks.back().data.exitCode.has_value());
        VERIFY_IS_TRUE(marks.front().data.exitCode.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"cd" }, std::wstring{ core->ReadLastPromptBounded(10, 100) });

        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;D;7\x7"));
        VERIFY_ARE_EQUAL(L"sleep streaming", core->ReadLastPromptBounded(10, 100));
    }

    void ControlCoreTests::TestReadLastPromptBoundedWrapAndLimits()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        auto core = createCore(*settings, *conn);
        _standardInit(core);

        _writePrompt(conn, L"");
        const std::wstring command(25, L'C');
        conn->WriteInput(winrt_wstring_to_array_view(command));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;C\x7\r\nout\r\nend\x1b]133;D;0\x7"));

        const auto& buffer = core->_terminal->GetTextBuffer();
        VERIFY_IS_TRUE(buffer.GetRowByOffset(0).WasWrapForced());
        VERIFY_IS_FALSE(buffer.GetRowByOffset(1).WasWrapForced());
        VERIFY_IS_FALSE(buffer.GetRowByOffset(2).WasWrapForced());

        // The seven-column prompt leaves 23 command cells on row 0.
        // Non-final hard-break rows retain their unused cells in this reader.
        const auto firstLine = command + std::wstring(28, L' ');
        const auto secondLine = L"out" + std::wstring(27, L' ');
        VERIFY_ARE_EQUAL(hstring{ firstLine }, core->ReadLastPromptBounded(1, 1000));
        VERIFY_ARE_EQUAL(hstring{ firstLine + L"\n" + secondLine }, core->ReadLastPromptBounded(2, 1000));
        VERIFY_ARE_EQUAL(hstring{ firstLine + L"\n" + secondLine + L"\n"
                                                                   L"end" },
                         core->ReadLastPromptBounded(3, 1000));

        VERIFY_ARE_EQUAL(hstring{ std::wstring(23, L'C') }, core->ReadLastPromptBounded(3, 23));
        VERIFY_ARE_EQUAL(hstring{ std::wstring(24, L'C') }, core->ReadLastPromptBounded(3, 24));
        const auto firstLineLength = static_cast<int32_t>(firstLine.size());
        VERIFY_ARE_EQUAL(hstring{ firstLine }, core->ReadLastPromptBounded(2, firstLineLength));
        VERIFY_ARE_EQUAL(hstring{ firstLine + L"\n" }, core->ReadLastPromptBounded(2, firstLineLength + 1));
        VERIFY_ARE_EQUAL(hstring{ firstLine + L"\no" }, core->ReadLastPromptBounded(2, firstLineLength + 2));
        VERIFY_ARE_EQUAL(hstring{ firstLine }, core->ReadLastPromptBounded(1, firstLineLength + 1));
    }

    void ControlCoreTests::TestReadLastPromptBoundedUnicode()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        auto core = createCore(*settings, *conn);
        _standardInit(core);

        _writePrompt(conn, L"");
        conn->WriteInput(winrt_wstring_to_array_view(L"A\U0001F366B\x1b]133;C\x7\x1b]133;D;0\x7"));
        VERIFY_ARE_EQUAL(L"A", core->ReadLastPromptBounded(1, 1));
        VERIFY_ARE_EQUAL(L"A\U0001F366", core->ReadLastPromptBounded(1, 2));
        VERIFY_ARE_EQUAL(L"A\U0001F366B", core->ReadLastPromptBounded(1, 3));
        VERIFY_ARE_EQUAL(L"A\U0001F366B", core->ReadLastPromptBounded(1, 4));

        conn->WriteInput(winrt_wstring_to_array_view(L"\r\n"));
        _writePrompt(conn, L"");
        const std::wstring prefix(22, L'A');
        conn->WriteInput(winrt_wstring_to_array_view(prefix + L"\U0001F366B\x1b]133;C\x7\x1b]133;D;0\x7"));

        const auto& row = core->_terminal->GetTextBuffer().GetRowByOffset(1);
        VERIFY_IS_TRUE(row.WasWrapForced());
        VERIFY_IS_TRUE(row.WasDoubleBytePadded());
        VERIFY_ARE_EQUAL(hstring{ prefix }, core->ReadLastPromptBounded(1, 22));
        VERIFY_ARE_EQUAL(hstring{ prefix + L"\U0001F366" }, core->ReadLastPromptBounded(1, 23));
        VERIFY_ARE_EQUAL(hstring{ prefix + L"\U0001F366B" }, core->ReadLastPromptBounded(1, 24));
    }

    void ControlCoreTests::TestReadLastPromptBoundedInvalidLimits()
    {
        auto [settings, conn] = _createSettingsAndConnection();
        auto core = createCore(*settings, *conn);
        _standardInit(core);

        for (const auto limit : { 0, -1 })
        {
            VERIFY_THROWS_SPECIFIC(
                core->ReadLastPromptBounded(limit, 100),
                wil::ResultException,
                [](const wil::ResultException& e) { return e.GetErrorCode() == E_INVALIDARG; });
            VERIFY_THROWS_SPECIFIC(
                core->ReadLastPromptBounded(10, limit),
                wil::ResultException,
                [](const wil::ResultException& e) { return e.GetErrorCode() == E_INVALIDARG; });
        }
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
