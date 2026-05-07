// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
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
    class EditLineStateTests
    {
        BEGIN_TEST_CLASS(EditLineStateTests)
            TEST_CLASS_PROPERTY(L"TestTimeout", L"0:0:10") // 10s timeout
        END_TEST_CLASS()

        TEST_METHOD(EmptyBuffer_NoPromptMark);
        TEST_METHOD(PromptMarkOnly_NoCommand);
        TEST_METHOD(TypedCommand_CursorAtEnd);
        TEST_METHOD(TypedCommand_CursorMidLine);
        TEST_METHOD(GhostTextNotIncluded);
        TEST_METHOD(CommandRunning_AfterEnter);
        TEST_METHOD(InAltBuffer);
        TEST_METHOD(MultiplePrompts_ReturnLatest);
        TEST_METHOD(IsPasteInProgressFlag);
        TEST_METHOD(IsInAlternateScreenBuffer_Method);

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
            core->Initialize(270, 380, 1.0);
#ifndef NDEBUG
            core->_terminal->_suppressLockChecks = true;
#endif
            VERIFY_IS_TRUE(core->_initializedTerminal);
            VERIFY_ARE_EQUAL(20, core->_terminal->GetViewport().Height());
        }

        winrt::com_ptr<Control::implementation::ControlCore> _createInitializedCore(winrt::com_ptr<MockConnection>& conn)
        {
            auto [settings, createdConn] = _createSettingsAndConnection();
            conn = createdConn;

            auto core = createCore(*settings, *conn);
            VERIFY_IS_NOT_NULL(core);
            _standardInit(core);
            return core;
        }

        static void _writePromptMarkOnly(const winrt::com_ptr<MockConnection>& conn)
        {
            conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;A\x7"));
            conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;B\x7"));
        }
    };

    void EditLineStateTests::EmptyBuffer_NoPromptMark()
    {
        winrt::com_ptr<MockConnection> conn;
        auto core = _createInitializedCore(conn);

        const auto state = core->GetEditLineState();
        VERIFY_ARE_EQUAL(L"", state.CursorPrefix);
        VERIFY_IS_TRUE(state.CursorAtEnd);
        VERIFY_IS_FALSE(state.HasPromptMark);
        VERIFY_IS_FALSE(state.CommandRunning);
        VERIFY_IS_FALSE(state.InAltBuffer);
    }

    void EditLineStateTests::PromptMarkOnly_NoCommand()
    {
        winrt::com_ptr<MockConnection> conn;
        auto core = _createInitializedCore(conn);

        _writePromptMarkOnly(conn);

        const auto state = core->GetEditLineState();
        VERIFY_ARE_EQUAL(L"", state.CursorPrefix);
        VERIFY_IS_TRUE(state.CursorAtEnd);
        VERIFY_IS_TRUE(state.HasPromptMark);
        VERIFY_IS_FALSE(state.CommandRunning);
        VERIFY_IS_FALSE(state.InAltBuffer);
    }

    void EditLineStateTests::TypedCommand_CursorAtEnd()
    {
        winrt::com_ptr<MockConnection> conn;
        auto core = _createInitializedCore(conn);

        _writePromptMarkOnly(conn);
        conn->WriteInput(winrt_wstring_to_array_view(L"git status"));

        const auto state = core->GetEditLineState();
        VERIFY_ARE_EQUAL(L"git status", state.CursorPrefix);
        VERIFY_IS_TRUE(state.CursorAtEnd);
        VERIFY_IS_TRUE(state.HasPromptMark);
        VERIFY_IS_FALSE(state.CommandRunning);
        VERIFY_IS_FALSE(state.InAltBuffer);
    }

    void EditLineStateTests::TypedCommand_CursorMidLine()
    {
        winrt::com_ptr<MockConnection> conn;
        auto core = _createInitializedCore(conn);

        _writePromptMarkOnly(conn);
        conn->WriteInput(winrt_wstring_to_array_view(L"BarBar"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b[D\x1b[D"));

        const auto state = core->GetEditLineState();
        VERIFY_ARE_EQUAL(L"BarB", state.CursorPrefix);
        VERIFY_IS_FALSE(state.CursorAtEnd);
        VERIFY_IS_TRUE(state.HasPromptMark);
        VERIFY_IS_FALSE(state.CommandRunning);
        VERIFY_IS_FALSE(state.InAltBuffer);
    }

    // Models the scenario where PSReadLine prediction has rendered "ar" after the
    // cursor (cursor is at offset 4, but characters 4-5 contain "ar" written by
    // PSReadLine). CurrentEditLineSnapshot uses clipAtCursor=true, so CursorPrefix
    // returns "BarB" (just the user-typed prefix), not the full row text "BarBar".
    void EditLineStateTests::GhostTextNotIncluded()
    {
        winrt::com_ptr<MockConnection> conn;
        auto core = _createInitializedCore(conn);

        _writePromptMarkOnly(conn);
        conn->WriteInput(winrt_wstring_to_array_view(L"BarB"));
        conn->WriteInput(winrt_wstring_to_array_view(L"ar"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b[D\x1b[D"));

        const auto state = core->GetEditLineState();
        VERIFY_ARE_EQUAL(L"BarB", state.CursorPrefix);
        VERIFY_IS_FALSE(state.CursorAtEnd);
        VERIFY_IS_TRUE(state.HasPromptMark);
        VERIFY_IS_FALSE(state.CommandRunning);
        VERIFY_IS_FALSE(state.InAltBuffer);
    }

    void EditLineStateTests::CommandRunning_AfterEnter()
    {
        winrt::com_ptr<MockConnection> conn;
        auto core = _createInitializedCore(conn);

        _writePromptMarkOnly(conn);
        conn->WriteInput(winrt_wstring_to_array_view(L"git status"));
        // OSC 133;C marks command execution beginning (StartOutput), which sets commandEnd.
        // NOTE: The production PowerShell shell-integration script does not currently emit ;C;
        // Phase 0 follow-up tracks either emitting ;C there or refining commandRunning derivation.
        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b]133;C\x7"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\r\n"));
        conn->WriteInput(winrt_wstring_to_array_view(L"On branch main\r\n"));

        const auto state = core->GetEditLineState();
        VERIFY_IS_TRUE(state.HasPromptMark);
        VERIFY_IS_TRUE(state.CommandRunning);
        VERIFY_IS_FALSE(state.InAltBuffer);
    }

    void EditLineStateTests::InAltBuffer()
    {
        winrt::com_ptr<MockConnection> conn;
        auto core = _createInitializedCore(conn);

        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b[?1049h"));

        const auto state = core->GetEditLineState();
        VERIFY_ARE_EQUAL(L"", state.CursorPrefix);
        VERIFY_IS_TRUE(state.CursorAtEnd);
        VERIFY_IS_FALSE(state.HasPromptMark);
        VERIFY_IS_FALSE(state.CommandRunning);
        VERIFY_IS_TRUE(state.InAltBuffer);
    }

    void EditLineStateTests::MultiplePrompts_ReturnLatest()
    {
        winrt::com_ptr<MockConnection> conn;
        auto core = _createInitializedCore(conn);

        _writePromptMarkOnly(conn);
        conn->WriteInput(winrt_wstring_to_array_view(L"first"));
        conn->WriteInput(winrt_wstring_to_array_view(L"\r\noutput\r\n"));
        _writePromptMarkOnly(conn);

        const auto state = core->GetEditLineState();
        VERIFY_ARE_EQUAL(L"", state.CursorPrefix);
        VERIFY_IS_TRUE(state.CursorAtEnd);
        VERIFY_IS_TRUE(state.HasPromptMark);
        VERIFY_IS_FALSE(state.CommandRunning);
        VERIFY_IS_FALSE(state.InAltBuffer);
    }

    void EditLineStateTests::IsPasteInProgressFlag()
    {
        winrt::com_ptr<MockConnection> conn;
        auto core = _createInitializedCore(conn);

        VERIFY_IS_FALSE(core->IsPasteInProgress());
    }

    void EditLineStateTests::IsInAlternateScreenBuffer_Method()
    {
        winrt::com_ptr<MockConnection> conn;
        auto core = _createInitializedCore(conn);

        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b[?1049h"));
        VERIFY_IS_TRUE(core->IsInAlternateScreenBuffer());

        conn->WriteInput(winrt_wstring_to_array_view(L"\x1b[?1049l"));
        VERIFY_IS_FALSE(core->IsInAlternateScreenBuffer());
    }
}
