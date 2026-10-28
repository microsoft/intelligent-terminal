// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include <WexTestClass.h>
#include "../TerminalCore/Terminal.hpp"
#include "../renderer/inc/DummyRenderer.hpp"

using namespace Microsoft::Terminal::Core;
namespace PS = Microsoft::Console::VirtualTerminal::ProgramStatus;
using Microsoft::Console::VirtualTerminal::DispatchTypes::TaskbarState;
using namespace WEX::TestExecution;

namespace TerminalCoreUnitTests
{
    class ProgramStatusTests
    {
        TEST_CLASS(ProgramStatusTests);

        struct Fixture
        {
            Terminal terminal{ Terminal::TestDummyMarker{} };
            DummyRenderer renderer{ &terminal };

            Fixture(const bool enabled = true)
            {
                terminal.Create({ 80, 25 }, 0, renderer);
                if (enabled)
                {
                    terminal.SetProgramStatusEnabled(true);
                }
            }

            void Report(const std::wstring_view body)
            {
                terminal.GetStateMachine().ProcessString(std::wstring{ L"\x1b]7501;" } + std::wstring{ body } + L"\x1b\\");
            }
        };

        TEST_METHOD(CapabilityRequiresExplicitOptIn)
        {
            Terminal uninitialized;
            uninitialized.ResetProgramStatusForConnection();
            VERIFY_IS_FALSE(uninitialized.GetProgramStatusSnapshot().observed);
            Fixture f{ false };
            size_t changes = 0;
            std::wstring reply;
            f.terminal.ProgramStatusChangedCallback([&]() { ++changes; });
            f.terminal.SetWriteInputCallback([&](const auto text) { reply.append(text); });
            VERIFY_IS_FALSE(f.terminal.SupportsProgramStatus());
            f.terminal.GetStateMachine().ProcessString(L"\x1b]7501;?\a");
            f.Report(L"state=done");
            VERIFY_IS_TRUE(reply.empty());
            VERIFY_IS_FALSE(f.terminal.GetProgramStatusSnapshot().observed);
            VERIFY_ARE_EQUAL(size_t{ 0 }, changes);
            f.terminal.SetProgramStatusEnabled(true);
            VERIFY_IS_TRUE(f.terminal.SupportsProgramStatus());
            f.Report(L"state=done");
            VERIFY_IS_TRUE(f.terminal.GetProgramStatusSnapshot().observed);
            VERIFY_ARE_EQUAL(size_t{ 1 }, changes);
            f.terminal.SetProgramStatusEnabled(false);
            VERIFY_IS_FALSE(f.terminal.SupportsProgramStatus());
            VERIFY_IS_FALSE(f.terminal.GetProgramStatusSnapshot().observed);
            VERIFY_IS_TRUE(f.terminal.GetProgramStatusSnapshot().results.empty());
            VERIFY_ARE_EQUAL(size_t{ 2 }, changes);
            f.terminal.SetProgramStatusEnabled(true);
            f.terminal.GetStateMachine().ProcessString(L"\033c");
            VERIFY_IS_TRUE(f.terminal.SupportsProgramStatus());
            f.terminal.GetStateMachine().ProcessString(L"\x1b]7501;?\a");
            VERIFY_IS_TRUE(reply == L"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\");
            VERIFY_IS_FALSE(f.terminal.GetProgramStatusSnapshot().observed);
        }

        TEST_METHOD(TaskbarLatchDoesNotResurrectLegacyProgress)
        {
            Fixture f;
            const auto lock = f.terminal.LockForWriting();
            size_t legacyChanges = 0;
            f.terminal.TaskbarProgressChangedCallback([&]() { ++legacyChanges; });
            f.terminal.SetTaskbarProgress(TaskbarState::Set, 17);
            f.Report(L"state=working");
            f.terminal.SetTaskbarProgress(TaskbarState::Set, 99);
            VERIFY_ARE_EQUAL(size_t{ 17 }, f.terminal.GetTaskbarProgress());
            f.terminal.GetStateMachine().ProcessString(L"\033c");
            VERIFY_ARE_EQUAL(size_t{ 0 }, f.terminal.GetTaskbarState());
            VERIFY_ARE_EQUAL(size_t{ 0 }, f.terminal.GetTaskbarProgress());
            VERIFY_ARE_EQUAL(size_t{ 1 }, legacyChanges);
        }

        TEST_METHOD(InvalidIdsAndAtomicLimits)
        {
            Fixture f;
            for (const auto id : { L"", L"/a", L"a/", L"a//b", L"a!", L"a b", L"a/b/c/d/e/f/g/h/i" })
            {
                f.Report(std::wstring{ L"state=working:id=" } + id);
                VERIFY_IS_FALSE(f.terminal.GetProgramStatusSnapshot().observed);
            }
            for (const auto& id : { std::wstring(33, L'a'), std::wstring(32, L'a') + L"/" + std::wstring(32, L'b') + L"/" + std::wstring(32, L'c') + L"/" + std::wstring(30, L'd') })
            {
                f.Report(L"state=working:id=" + id);
                VERIFY_IS_FALSE(f.terminal.GetProgramStatusSnapshot().observed);
            }
            f.Report(L"state=working:id=build:progress=41");
            const auto revision = f.terminal.GetProgramStatusSnapshot().revision;
            for (const auto& body : {
                     std::wstring{ L"state=clear:id=bad!:id=build" },
                     std::wstring{ L"state=clear:id=build:msg=AA==" },
                     std::wstring{ L"state=clear:id=build:title=" } + std::wstring(257, L'A') + L":title=b2s=",
                     std::wstring{ L"state=idle:app=" } + std::wstring(33, L'a') + L":app=ok",
                     std::wstring{ L"state=idle:msg=" } + std::wstring(2733, L'A') + L":msg=b2s=",
                     std::wstring{ L"state=idle:abcdefghijklmnopq=x" },
                 })
            {
                f.Report(body);
                const auto snapshot = f.terminal.GetProgramStatusSnapshot();
                VERIFY_ARE_EQUAL(revision, snapshot.revision);
                VERIFY_ARE_EQUAL(size_t{ 1 }, snapshot.records.size());
                VERIFY_IS_TRUE(snapshot.records.front().state == PS::State::Working);
            }
        }

        TEST_METHOD(DuplicateMalformedAndReplacementFields)
        {
            Fixture f;
            f.Report(L"state=unknown");
            f.Report(L"?:states=idle,working");
            f.Report(L"id=a");
            VERIFY_IS_FALSE(f.terminal.GetProgramStatusSnapshot().observed);
            f.Report(L" state = idle :state=working:id=a:id=b:kind=permission:progress=25:app=builder:title=b2s=:msg=b2s=:ignored=x:bad pair");
            auto snapshot = f.terminal.GetProgramStatusSnapshot();
            VERIFY_ARE_EQUAL(size_t{ 1 }, snapshot.records.size());
            VERIFY_IS_TRUE(snapshot.records[0].id == "b");
            VERIFY_IS_TRUE(snapshot.records[0].state == PS::State::Working);
            VERIFY_IS_FALSE(snapshot.records[0].kind.has_value());
            VERIFY_ARE_EQUAL(uint32_t{ 25 }, snapshot.records[0].progress.value());
            VERIFY_IS_TRUE(snapshot.records[0].message == L"ok");
            f.Report(L"state=blocked:id=b:kind=question:kind=auth:progress=100");
            snapshot = f.terminal.GetProgramStatusSnapshot();
            VERIFY_IS_TRUE(snapshot.records[0].kind == PS::BlockKind::Auth);
            VERIFY_IS_FALSE(snapshot.records[0].app.has_value());
            VERIFY_IS_FALSE(snapshot.records[0].title.has_value());
            VERIFY_IS_FALSE(snapshot.records[0].message.has_value());
            for (const auto value : { L"-1", L"101", L"1.0", L"x", L"999999999999999999999999" })
            {
                f.Report(std::wstring{ L"state=blocked:id=b:kind=unknown:app=bad!:progress=" } + value);
                snapshot = f.terminal.GetProgramStatusSnapshot();
                VERIFY_IS_FALSE(snapshot.records[0].progress.has_value());
                VERIFY_IS_FALSE(snapshot.records[0].kind.has_value());
                VERIFY_IS_FALSE(snapshot.records[0].app.has_value());
            }
            f.Report(L"state=working:id=b:state=bad!");
            VERIFY_IS_TRUE(f.terminal.GetProgramStatusSnapshot().records[0].state == PS::State::Working);
            f.Report(L"state=working:id=b:msg=Zmlyc3Q=:msg=bGF0ZXI=:message=AA==");
            VERIFY_IS_TRUE(f.terminal.GetProgramStatusSnapshot().records[0].message == L"later");
            f.Report(L"state=idle:id=b:message=AA==");
            snapshot = f.terminal.GetProgramStatusSnapshot();
            VERIFY_IS_TRUE(snapshot.records[0].state == PS::State::Idle);
            VERIFY_IS_FALSE(snapshot.records[0].message.has_value());
        }

        TEST_METHOD(StrictTextDecoding)
        {
            Fixture f;
            f.Report(L"state=done:title=8J+agA:msg=5L2g5aW9");
            auto snapshot = f.terminal.GetProgramStatusSnapshot();
            VERIFY_IS_TRUE(snapshot.records[0].title.value() == L"\U0001f680");
            VERIFY_IS_TRUE(snapshot.records[0].message.value() == L"\u4f60\u597d");
            const auto revision = snapshot.revision;
            for (const auto text : { L"AA==", L"fw==", L"woA=", L"wK8=", L"7aCA", L"9JCAgA==", L"/w==", L"Y===", L"YQ=", L"YQ===", L"YQ==x", L"Y", L"!!!!" })
            {
                f.Report(std::wstring{ L"state=clear:msg=" } + text);
                VERIFY_ARE_EQUAL(revision, f.terminal.GetProgramStatusSnapshot().revision);
            }
            std::wstring title;
            std::wstring message;
            for (size_t i = 0; i < 64; ++i)
            {
                title += L"QUFB";
            }
            for (size_t i = 0; i < 682; ++i)
            {
                message += L"QUFB";
            }
            f.Report(L"state=working:title=" + title + L":msg=" + message + L"QUFB");
            VERIFY_ARE_EQUAL(revision, f.terminal.GetProgramStatusSnapshot().revision); // Decoded message is 2049 bytes.
            f.Report(L"state=working:title=" + title + L":msg=" + message + L"QUE=");
            snapshot = f.terminal.GetProgramStatusSnapshot();
            VERIFY_ARE_EQUAL(size_t{ 192 }, snapshot.records[0].title->size());
            VERIFY_ARE_EQUAL(size_t{ 2048 }, snapshot.records[0].message->size());
        }

        TEST_METHOD(QueryFragmentationAndRawObservation)
        {
            const std::wstring query = L"\x1b]7501;?\x1b\\";
            for (size_t split = 0; split <= query.size(); ++split)
            {
                Fixture f;
                std::vector<std::wstring> replies;
                size_t bells = 0;
                size_t raw = 0;
                f.terminal.SetWriteInputCallback([&](const auto text) { replies.emplace_back(text); });
                f.terminal.SetWarningBellCallback([&]() { ++bells; });
                f.terminal.SetVtSequenceCallback([&](const auto) { ++raw; });
                f.terminal.GetStateMachine().ProcessString(std::wstring_view{ query }.substr(0, split));
                f.terminal.GetStateMachine().ProcessString(std::wstring{ std::wstring_view{ query }.substr(split) } + L"\x1b[c");
                VERIFY_ARE_EQUAL(size_t{ 2 }, replies.size());
                VERIFY_IS_TRUE(replies[0] == L"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\");
                VERIFY_IS_FALSE(f.terminal.GetProgramStatusSnapshot().observed);
                f.terminal.GetStateMachine().ProcessString(L"\x1b]7501;?\a");
                VERIFY_ARE_EQUAL(size_t{ 3 }, replies.size());
                VERIFY_ARE_EQUAL(size_t{ 0 }, bells);
                VERIFY_ARE_EQUAL(size_t{ 0 }, raw);
                f.terminal.GetStateMachine().ProcessString(L"\x1b]9999;unknown\a");
                VERIFY_ARE_EQUAL(size_t{ 1 }, raw);
            }
            const std::wstring report = L"\x1b]7501;state=blocked:id=a:kind=permission\a";
            for (size_t split = 0; split <= report.size(); ++split)
            {
                Fixture f;
                f.terminal.GetStateMachine().ProcessString(std::wstring_view{ report }.substr(0, split));
                f.terminal.GetStateMachine().ProcessString(std::wstring_view{ report }.substr(split));
                VERIFY_IS_TRUE(f.terminal.GetProgramStatusSnapshot().records[0].kind == PS::BlockKind::Permission);
            }
        }

        TEST_METHOD(HierarchyInheritanceAndLru)
        {
            Fixture f;
            f.Report(L"state=working:app=root");
            f.Report(L"state=working:id=build:app=builder");
            f.Report(L"state=working:id=build/child");
            f.Report(L"state=working:id=builder");
            auto snapshot = f.terminal.GetProgramStatusSnapshot();
            const auto find = [](const PS::Snapshot& s, const std::string_view id) -> const PS::Record& {
                return *std::find_if(s.records.begin(), s.records.end(), [&](const auto& r) { return r.id == id; });
            };
            VERIFY_IS_TRUE(find(snapshot, "build/child").effectiveApp == "builder");
            f.Report(L"state=idle:id=build");
            snapshot = f.terminal.GetProgramStatusSnapshot();
            VERIFY_IS_TRUE(find(snapshot, "build/child").effectiveApp == "root");
            f.Report(L"state=clear:id=build");
            snapshot = f.terminal.GetProgramStatusSnapshot();
            VERIFY_ARE_EQUAL(size_t{ 2 }, snapshot.records.size());
            VERIFY_IS_TRUE(find(snapshot, "builder").effectiveApp == "root");
            f.Report(L"state=clear");
            VERIFY_IS_TRUE(f.terminal.GetProgramStatusSnapshot().observed);
            f.Report(L"state=working:id=old");
            f.Report(L"state=working:id=old/child");
            for (size_t i = 0; i < 254; ++i)
            {
                f.Report(L"state=idle:id=n" + std::to_wstring(i));
            }
            const auto beforeRefresh = f.terminal.GetProgramStatusSnapshot().revision;
            f.Report(L"state=working:id=old/child");
            VERIFY_ARE_EQUAL(beforeRefresh, f.terminal.GetProgramStatusSnapshot().revision);
            f.Report(L"state=working:id=new");
            snapshot = f.terminal.GetProgramStatusSnapshot();
            VERIFY_ARE_EQUAL(size_t{ 256 }, snapshot.records.size());
            VERIFY_IS_TRUE(std::none_of(snapshot.records.begin(), snapshot.records.end(), [](const auto& r) { return r.id == "old"; }));
            VERIFY_IS_TRUE(find(snapshot, "old/child").id == "old/child");
        }

        TEST_METHOD(LifecycleAndResultAcknowledgment)
        {
            Fixture f;
            size_t changes = 0;
            f.terminal.ProgramStatusChangedCallback([&]() { ++changes; });
            f.Report(L"state=done:id=a:msg=b2s=");
            VERIFY_ARE_EQUAL(size_t{ 1 }, changes);
            const auto done = f.terminal.GetProgramStatusSnapshot().revision;
            f.Report(L"state=idle:id=a");
            auto snapshot = f.terminal.GetProgramStatusSnapshot();
            VERIFY_ARE_EQUAL(size_t{ 1 }, snapshot.results.size());
            VERIFY_IS_TRUE(snapshot.results[0].record.state == PS::State::Done);
            VERIFY_IS_TRUE(snapshot.results[0].unread);
            f.terminal.AcknowledgeProgramStatus(done - 1);
            VERIFY_IS_TRUE(f.terminal.GetProgramStatusSnapshot().results[0].unread);
            VERIFY_ARE_EQUAL(size_t{ 2 }, changes);
            f.terminal.AcknowledgeProgramStatus(done);
            VERIFY_IS_FALSE(f.terminal.GetProgramStatusSnapshot().results[0].unread);
            VERIFY_ARE_EQUAL(size_t{ 3 }, changes);
            f.Report(L"state=done:id=a:msg=b2s=");
            VERIFY_IS_TRUE(f.terminal.GetProgramStatusSnapshot().results[0].unread); // New transition.
            const auto repeated = f.terminal.GetProgramStatusSnapshot().revision;
            f.terminal.AcknowledgeProgramStatus(repeated);
            const auto acknowledged = f.terminal.GetProgramStatusSnapshot().revision;
            f.Report(L"state=done:id=a:msg=b2s=");
            VERIFY_ARE_EQUAL(acknowledged, f.terminal.GetProgramStatusSnapshot().revision);
            VERIFY_IS_FALSE(f.terminal.GetProgramStatusSnapshot().results[0].unread);
            f.Report(L"state=error:id=a:msg=ZXJyb3I=");
            f.terminal.AcknowledgeProgramStatus(repeated);
            VERIFY_IS_TRUE(f.terminal.GetProgramStatusSnapshot().results[0].unread);
            f.Report(L"state=working:id=b");
            f.Report(L"state=blocked:id=c");
            f.Report(L"state=idle:id=d");
            f.terminal.GetStateMachine().ProcessString(L"\x1b]133;D;0\a");
            VERIFY_ARE_EQUAL(size_t{ 4 }, f.terminal.GetProgramStatusSnapshot().records.size());
            f.terminal.GetStateMachine().ProcessString(L"\x1b[!p\x1b[?1049h\x1b[?1049l");
            VERIFY_ARE_EQUAL(size_t{ 4 }, f.terminal.GetProgramStatusSnapshot().records.size());
            f.terminal.GetStateMachine().ProcessString(L"\x1b]133;A\a");
            VERIFY_ARE_EQUAL(size_t{ 1 }, f.terminal.GetProgramStatusSnapshot().records.size());
            f.Report(L"state=working:id=b");
            f.terminal.GetStateMachine().ProcessString(L"\x1b]1337;SetMark\a");
            VERIFY_ARE_EQUAL(size_t{ 1 }, f.terminal.GetProgramStatusSnapshot().records.size());
            f.Report(L"state=working:id=b");
            f.terminal.ProgramStatusProcessExited();
            VERIFY_ARE_EQUAL(size_t{ 1 }, f.terminal.GetProgramStatusSnapshot().records.size());
            VERIFY_IS_TRUE(f.terminal.GetProgramStatusSnapshot().results[0].unread);
            f.Report(L"state=clear:id=a");
            VERIFY_IS_TRUE(f.terminal.GetProgramStatusSnapshot().results.empty());
            VERIFY_IS_TRUE(f.terminal.GetProgramStatusSnapshot().observed);
            f.terminal.GetStateMachine().ProcessString(L"\033c");
            VERIFY_IS_FALSE(f.terminal.GetProgramStatusSnapshot().observed);
            f.Report(L"state=done");
            const auto oldRevision = f.terminal.GetProgramStatusSnapshot().revision;
            f.terminal.ResetProgramStatusForConnection();
            f.Report(L"state=error");
            f.terminal.AcknowledgeProgramStatus(oldRevision);
            snapshot = f.terminal.GetProgramStatusSnapshot();
            VERIFY_IS_TRUE(snapshot.results[0].unread);
            VERIFY_IS_GREATER_THAN(snapshot.revision, oldRevision);
            VERIFY_IS_GREATER_THAN(changes, size_t{ 0 });
        }
    };
}
