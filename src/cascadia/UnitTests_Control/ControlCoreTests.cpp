// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "../TerminalControl/EventArgs.h"
#include "../TerminalControl/ControlCore.h"
#include "MockControlSettings.h"
#include "MockConnection.h"
#include "../../inc/TestUtils.h"
#include <winrt/Windows.Data.Json.h>

using namespace Microsoft::Console;
using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;

using namespace winrt;
using namespace winrt::Microsoft::Terminal;

namespace ControlUnitTests
{
    namespace Json = winrt::Windows::Data::Json;

    class ProgramStatusConnection : public winrt::implements<ProgramStatusConnection, TerminalConnection::ITerminalConnection>
    {
    public:
        void Initialize(const winrt::Windows::Foundation::Collections::ValueSet&) {}
        void Start() {}
        void Resize(uint32_t, uint32_t) {}
        void Close() {}
        winrt::guid SessionId() const { return {}; }
        TerminalConnection::ConnectionState State() const { return _state; }
        void WriteInput(const winrt::array_view<const char16_t> data)
        {
            input.append(reinterpret_cast<const wchar_t*>(data.data()), data.size());
        }
        void Emit(const std::wstring_view data)
        {
            TerminalOutput.raise(winrt_wstring_to_array_view(data));
        }
        void Exit(const bool failed = false)
        {
            _state = failed ? TerminalConnection::ConnectionState::Failed : TerminalConnection::ConnectionState::Closed;
            StateChanged.raise(*this, nullptr);
        }

        std::wstring input;
        til::event<TerminalConnection::TerminalOutputHandler> TerminalOutput;
        til::typed_event<TerminalConnection::ITerminalConnection, IInspectable> StateChanged;

    private:
        TerminalConnection::ConnectionState _state{ TerminalConnection::ConnectionState::Connected };
    };

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

        TEST_METHOD(TestFontInitializedInCtor);

        TEST_METHOD(TestClearScrollback);
        TEST_METHOD(TestClearScreen);
        TEST_METHOD(TestClearAll);
        TEST_METHOD(TestReadEntireBuffer);
        TEST_METHOD(TestReadBufferTail);
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
        TEST_METHOD(ProgramStatusJsonAndTaskbar);
        TEST_METHOD(ProgramStatusResultsAndAcknowledgment);
        TEST_METHOD(ProgramStatusConnectionLifecycle);
        TEST_METHOD(ProgramStatusCoalescesInvalidations);
        TEST_METHOD(ProgramStatusCallbackDoesNotWaitForDispatcherSetup);

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

    void ControlCoreTests::ProgramStatusJsonAndTaskbar()
    {
        const auto settings = winrt::make_self<MockControlSettings>();
        const auto conn = winrt::make_self<ProgramStatusConnection>();
        const auto core = createCore(*settings, *conn);
        _standardInit(core);
        core->_terminal->SetProgramStatusEnabled(true);
        conn->Emit(L"\x1b]9;4;1;17\x7");
        VERIFY_ARE_EQUAL(1u, core->TaskbarState());

        conn->Emit(L"\x1b]7501;state=done:app=test\x7"
                   L"\x1b]7501;id=job:state=working:progress=42:title=SGk=:msg=b2s=\x7");
        auto json = Json::JsonObject::Parse(core->ProgramStatusJson());
        VERIFY_IS_TRUE(json.GetNamedBoolean(L"observed"));
        VERIFY_ARE_EQUAL(L"working", json.GetNamedString(L"state"));
        VERIFY_ARE_EQUAL(42.0, json.GetNamedNumber(L"progress"));
        VERIFY_ARE_EQUAL(L"ok", json.GetNamedString(L"message"));
        VERIFY_ARE_EQUAL(L"job", json.GetNamedString(L"task_id"));
        VERIFY_ARE_EQUAL(L"Hi", json.GetNamedString(L"title"));
        VERIFY_ARE_EQUAL(1.0, json.GetNamedNumber(L"working"));
        VERIFY_ARE_EQUAL(1.0, json.GetNamedNumber(L"done"));
        const auto records = json.GetNamedArray(L"records");
        VERIFY_ARE_EQUAL(2u, records.Size());
        bool found = false;
        for (const auto& value : records)
        {
            const auto record = value.GetObject();
            if (record.GetNamedString(L"id") == L"job")
            {
                found = true;
                VERIFY_ARE_EQUAL(L"test", record.GetNamedString(L"app"));
                VERIFY_ARE_EQUAL(L"Hi", record.GetNamedString(L"title"));
                VERIFY_ARE_EQUAL(L"ok", record.GetNamedString(L"message"));
                VERIFY_IS_FALSE(record.GetNamedBoolean(L"unread"));
            }
        }
        VERIFY_IS_TRUE(found);
        VERIFY_ARE_EQUAL(1u, core->TaskbarState());
        VERIFY_ARE_EQUAL(42u, core->TaskbarProgress());

        conn->Emit(L"\x1b]7501;id=other:state=working:progress=65\x7");
        json = Json::JsonObject::Parse(core->ProgramStatusJson());
        VERIFY_ARE_EQUAL(Json::JsonValueType::Null, json.GetNamedValue(L"progress").ValueType());
        VERIFY_ARE_EQUAL(3u, core->TaskbarState());
        conn->Emit(L"\x1b]7501;id=approval:state=blocked:kind=permission\x7");
        json = Json::JsonObject::Parse(core->ProgramStatusJson());
        VERIFY_ARE_EQUAL(L"blocked", json.GetNamedString(L"state"));
        VERIFY_ARE_EQUAL(L"permission", json.GetNamedString(L"kind"));
        VERIFY_ARE_EQUAL(4u, core->TaskbarState());

        conn->Emit(L"\x1b]7501;state=clear\x7\x1b]9;4;1;99\x7");
        VERIFY_ARE_EQUAL(0u, core->TaskbarState());
        VERIFY_ARE_EQUAL(0u, core->TaskbarProgress());
        VERIFY_IS_TRUE(Json::JsonObject::Parse(core->ProgramStatusJson()).GetNamedBoolean(L"observed"));
    }

    void ControlCoreTests::ProgramStatusResultsAndAcknowledgment()
    {
        const auto settings = winrt::make_self<MockControlSettings>();
        const auto conn = winrt::make_self<ProgramStatusConnection>();
        const auto core = createCore(*settings, *conn);
        _standardInit(core);
        core->_terminal->SetProgramStatusEnabled(true);
        conn->Emit(L"\x1b]7501;state=done\x7");
        const auto firstRevision = core->_terminal->GetProgramStatusSnapshot().revision;
        conn->Emit(L"\x1b]7501;id=job:state=done:msg=ZmluaXNoZWQ=\x7\x1b]7501;id=job:state=idle\x7");
        auto json = Json::JsonObject::Parse(core->ProgramStatusJson());
        VERIFY_ARE_EQUAL(2.0, json.GetNamedNumber(L"unseen_done"));
        VERIFY_ARE_EQUAL(0u, core->TaskbarState());
        bool retainedResult = false;
        for (const auto& value : json.GetNamedArray(L"results"))
        {
            const auto result = value.GetObject();
            if (result.GetNamedString(L"id") == L"job")
            {
                retainedResult = true;
                VERIFY_ARE_EQUAL(hstring{ L"done" }, result.GetNamedString(L"state"));
                VERIFY_ARE_EQUAL(hstring{ L"finished" }, result.GetNamedString(L"message"));
                VERIFY_IS_TRUE(result.GetNamedBoolean(L"unread"));
            }
        }
        VERIFY_IS_TRUE(retainedResult);

        conn->Emit(L"\x1b]7501;?\x1b\\");
        VERIFY_IS_FALSE(conn->input.empty());
        core->SendInput(L"programmatic");
        json = Json::JsonObject::Parse(core->ProgramStatusJson());
        VERIFY_ARE_EQUAL(2.0, json.GetNamedNumber(L"unseen_done"));
        core->AcknowledgeProgramStatus(firstRevision);
        json = Json::JsonObject::Parse(core->ProgramStatusJson());
        VERIFY_ARE_EQUAL(1.0, json.GetNamedNumber(L"unseen_done"));
        const auto observedRevision = core->_terminal->GetProgramStatusSnapshot().revision;
        conn->Emit(L"\x1b]7501;id=job:state=error\x7");
        core->AcknowledgeProgramStatus(observedRevision);
        json = Json::JsonObject::Parse(core->ProgramStatusJson());
        VERIFY_ARE_EQUAL(1.0, json.GetNamedNumber(L"unseen_error"));
        VERIFY_ARE_EQUAL(2u, core->TaskbarState());
        core->AcknowledgeProgramStatus(core->_terminal->GetProgramStatusSnapshot().revision);
        VERIFY_ARE_EQUAL(0u, core->TaskbarState());
        conn->Emit(L"\x1b]7501;id=job:state=error:msg=Zmlyc3Q=\x7");
        core->ProgramStatusJson();
        conn->Emit(L"\x1b]7501;id=job:state=error:msg=bGF0ZXI=\x7");
        core->AcknowledgeProgramStatusForUserInput();
        VERIFY_ARE_EQUAL(1.0, Json::JsonObject::Parse(core->ProgramStatusJson()).GetNamedNumber(L"unseen_error"));
        core->AcknowledgeProgramStatusForUserInput();
        VERIFY_ARE_EQUAL(0u, core->TaskbarState());
    }

    void ControlCoreTests::ProgramStatusConnectionLifecycle()
    {
        const auto settings = winrt::make_self<MockControlSettings>();
        const auto conn = winrt::make_self<ProgramStatusConnection>();
        const auto core = createCore(*settings, *conn);
        _standardInit(core);
        core->_terminal->SetProgramStatusEnabled(true);
        conn->Emit(L"\x1b]7501;id=live:state=working\x7\x1b]7501;id=final:state=done\x7");
        core->Detach();
        core->AttachToNewControl();
        VERIFY_ARE_EQUAL(2u, core->_terminal->GetProgramStatusSnapshot().records.size());
        conn->Exit();
        auto json = Json::JsonObject::Parse(core->ProgramStatusJson());
        VERIFY_ARE_EQUAL(0.0, json.GetNamedNumber(L"working"));
        VERIFY_ARE_EQUAL(1.0, json.GetNamedNumber(L"done"));
        VERIFY_ARE_EQUAL(1.0, json.GetNamedNumber(L"unseen_done"));
        conn->Emit(L"\x1b]7501;id=late-live:state=working\x7\x1b]7501;id=late-result:state=done\x7");
        json = Json::JsonObject::Parse(core->ProgramStatusJson());
        VERIFY_ARE_EQUAL(0.0, json.GetNamedNumber(L"working"));
        VERIFY_ARE_EQUAL(2.0, json.GetNamedNumber(L"unseen_done"));
        const auto oldRevision = core->_terminal->GetProgramStatusSnapshot().revision;

        const auto replacement = winrt::make_self<ProgramStatusConnection>();
        core->Connection(*replacement);
        json = Json::JsonObject::Parse(core->ProgramStatusJson());
        VERIFY_IS_FALSE(json.GetNamedBoolean(L"observed"));
        VERIFY_ARE_EQUAL(0u, json.GetNamedArray(L"records").Size());
        replacement->Emit(L"\x1b]7501;state=done\x7");
        core->AcknowledgeProgramStatus(oldRevision);
        VERIFY_ARE_EQUAL(1.0, Json::JsonObject::Parse(core->ProgramStatusJson()).GetNamedNumber(L"unseen_done"));
        replacement->Emit(L"\x1b]7501;state=working\x7");
        replacement->Exit(true);
        core->AcknowledgeProgramStatus(core->_terminal->GetProgramStatusSnapshot().revision);
        VERIFY_ARE_EQUAL(2u, core->TaskbarState());
        core->Close();
        json = Json::JsonObject::Parse(core->ProgramStatusJson());
        VERIFY_IS_FALSE(json.GetNamedBoolean(L"observed"));
        VERIFY_ARE_EQUAL(0u, json.GetNamedArray(L"records").Size());
        VERIFY_ARE_EQUAL(0.0, json.GetNamedNumber(L"unseen_done"));
    }

    void ControlCoreTests::ProgramStatusCoalescesInvalidations()
    {
        const auto settings = winrt::make_self<MockControlSettings>();
        const auto conn = winrt::make_self<ProgramStatusConnection>();
        const auto core = createCore(*settings, *conn);
        _standardInit(core);
        core->_terminal->SetProgramStatusEnabled(true);
        wil::unique_event changed;
        changed.create();
        std::atomic<uint32_t> count{ 0 };
        const auto revoker = core.as<Control::ControlCore>().ProgramStatusChanged(winrt::auto_revoke, [&](const auto&, const auto&) {
            ++count;
            changed.SetEvent();
        });
        conn->Emit(L"\x1b]7501;id=job:state=working\x7\x1b]7501;id=job:state=done\x7\x1b]7501;id=job:state=idle\x7");
        VERIFY_IS_TRUE(changed.wait(3000));
        VERIFY_ARE_EQUAL(1u, count.load());
        VERIFY_ARE_EQUAL(1.0, Json::JsonObject::Parse(core->ProgramStatusJson()).GetNamedNumber(L"unseen_done"));
    }

    void ControlCoreTests::ProgramStatusCallbackDoesNotWaitForDispatcherSetup()
    {
        const auto settings = winrt::make_self<MockControlSettings>();
        const auto conn = winrt::make_self<ProgramStatusConnection>();
        const auto core = createCore(*settings, *conn);
        _standardInit(core);
        core->_terminal->SetProgramStatusEnabled(true);
        {
            const auto setupLock = core->_shared.lock();
            const auto streamLock = core->_terminal->LockForWriting();
            core->_terminal->ApplyProgramStatus(*::Microsoft::Console::VirtualTerminal::ProgramStatus::Parse(L"state=done"));
        }
        VERIFY_ARE_EQUAL(1u, core->_terminal->GetProgramStatusSnapshot().results.size());
    }

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
        VERIFY_ARE_EQUAL(hstring{ firstLine + L"\n" + secondLine + L"\n" L"end" }, core->ReadLastPromptBounded(3, 1000));

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
