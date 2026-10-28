// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include <windows.h>
#include <WexTestClass.h>
#include "../ProgramStatus.hpp"
#include <algorithm>

using namespace WEX::TestExecution;
namespace PS = Microsoft::Console::VirtualTerminal::ProgramStatus;

class ProgramStatusModelTests
{
    TEST_CLASS(ProgramStatusModelTests);

    static void apply(PS::Store& store, const std::wstring_view body)
    {
        const auto command = PS::Parse(body);
        VERIFY_IS_TRUE(command.has_value());
        store.Apply(command.value());
    }

    TEST_METHOD(MalformedIdsAndDuplicateAtomicity)
    {
        PS::Store store;
        for (const auto id : { L"", L"/a", L"a/", L"a//b", L"bad!", L"bad id", L"a/b/c/d/e/f/g/h/i" })
        {
            VERIFY_IS_FALSE(PS::Parse(std::wstring{ L"state=clear:id=" } + id).has_value());
            VERIFY_IS_FALSE(PS::Parse(std::wstring{ L"state=working:id=" } + id + L":id=ok").has_value());
        }
        VERIFY_IS_FALSE(PS::Parse(L"state=working:id=" + std::wstring(33, L'x')).has_value());
        VERIFY_IS_FALSE(PS::Parse(L"state=working:id=" + std::wstring(32, L'a') + L"/" + std::wstring(32, L'b') + L"/" + std::wstring(32, L'c') + L"/" + std::wstring(30, L'd')).has_value());
        VERIFY_IS_TRUE(PS::Parse(L"state=working:id=" + std::wstring(31, L'a') + L"/" + std::wstring(31, L'b') + L"/" + std::wstring(31, L'c') + L"/" + std::wstring(32, L'd')).has_value());
        VERIFY_IS_TRUE(PS::Parse(L"state=working:id=a/b/c/d/e/f/g/h").has_value());
        apply(store, L" state = idle :state=working:id=a:id=b:progress=50:kind=permission:unknown=ok:bad pair");
        auto snapshot = store.GetSnapshot();
        VERIFY_IS_TRUE(snapshot.records[0].id == "b");
        VERIFY_IS_TRUE(snapshot.records[0].state == PS::State::Working);
        VERIFY_IS_FALSE(snapshot.records[0].kind.has_value());
        VERIFY_IS_TRUE(snapshot.records[0].progress == uint32_t{ 50 });
        VERIFY_IS_FALSE(PS::Parse(L"state=unknown").has_value());
        VERIFY_IS_FALSE(PS::Parse(L"id=a").has_value());
        VERIFY_IS_FALSE(PS::Parse(L"state=clear:msg=AA==").has_value());
        apply(store, L"state=blocked:id=b:kind=auth:progress=-1:app=bad!");
        snapshot = store.GetSnapshot();
        VERIFY_IS_TRUE(snapshot.records[0].kind == PS::BlockKind::Auth);
        VERIFY_IS_FALSE(snapshot.records[0].progress.has_value());
        VERIFY_IS_FALSE(snapshot.records[0].app.has_value());
    }

    TEST_METHOD(AllBoundsBeforeMutation)
    {
        for (const auto& body : {
                 L"state=clear:app=" + std::wstring(33, L'a') + L":app=ok",
                 L"state=clear:title=" + std::wstring(257, L'A') + L":title=b2s=",
                 L"state=clear:msg=" + std::wstring(2733, L'A') + L":msg=b2s=",
                 std::wstring{ L"state=clear:abcdefghijklmnopq=x" },
             })
        {
            VERIFY_IS_FALSE(PS::Parse(body).has_value());
        }
        auto body = std::wstring{ L"state=working:" };
        body.resize(PS::MaximumBodyLength, L':');
        VERIFY_IS_TRUE(PS::Parse(body).has_value());
        body += L':';
        VERIFY_IS_FALSE(PS::Parse(body).has_value());

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
        const auto valid = PS::Parse(L"state=working:title=" + title + L":msg=" + message + L"QUE=");
        VERIFY_IS_TRUE(valid.has_value());
        VERIFY_ARE_EQUAL(size_t{ 192 }, valid->record.title->size());
        VERIFY_ARE_EQUAL(size_t{ 2048 }, valid->record.message->size());
        VERIFY_IS_FALSE(PS::Parse(L"state=working:msg=" + message + L"QUFB").has_value());
        std::wstring unicode;
        for (size_t i = 0; i < 682; ++i)
        {
            unicode += L"5L2g";
        }
        VERIFY_IS_FALSE(PS::Parse(L"state=working:msg=" + unicode + L"QUFB").has_value());
        const auto unicodeLimit = PS::Parse(L"state=working:msg=" + unicode + L"QUE=");
        VERIFY_IS_TRUE(unicodeLimit.has_value());
        VERIFY_ARE_EQUAL(size_t{ 684 }, unicodeLimit->record.message->size());
    }

    TEST_METHOD(StrictBase64Utf8AndQueries)
    {
        const auto text = PS::Parse(L"state=done:title=8J+agA:msg=5L2g5aW9");
        VERIFY_IS_TRUE(text.has_value());
        VERIFY_IS_TRUE(text->record.title == L"\U0001f680");
        VERIFY_IS_TRUE(text->record.message == L"\u4f60\u597d");
        for (const auto value : { L"AA==", L"fw==", L"woA=", L"wK8=", L"7aCA", L"9JCAgA==", L"/w==", L"Y===", L"YQ=", L"YQ===", L"YQ==x", L"Y", L"!!!!", L"YR==", L"_w==" })
        {
            VERIFY_IS_FALSE(PS::Parse(std::wstring{ L"state=clear:msg=" } + value).has_value());
        }
        PS::Store store;
        const auto query = PS::Parse(L"?");
        VERIFY_IS_TRUE(query.has_value());
        VERIFY_IS_TRUE(query->operation == PS::Command::Operation::Query);
        VERIFY_IS_FALSE(store.Apply(*query));
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, store.GetSnapshot().revision);
        VERIFY_IS_FALSE(store.GetSnapshot().observed);
        VERIFY_IS_FALSE(PS::Parse(L"?:states=idle,working").has_value());
    }

    TEST_METHOD(MsgWireFieldAndUnknownKeys)
    {
        PS::Store store;
        apply(store, L"state=working:msg=Zmlyc3Q=:msg=bGF0ZXI=:message=AA==");
        VERIFY_IS_TRUE(store.GetSnapshot().records[0].message == L"later");
        const auto revision = store.GetSnapshot().revision;
        VERIFY_IS_FALSE(PS::Parse(L"state=clear:msg=AA==:msg=b2s=").has_value());
        VERIFY_ARE_EQUAL(revision, store.GetSnapshot().revision);
        apply(store, L"state=working:msg=");
        VERIFY_IS_TRUE(store.GetSnapshot().records[0].message == L"");
        apply(store, L"state=idle:message=AA==");
        VERIFY_IS_FALSE(store.GetSnapshot().records[0].message.has_value());
    }

    TEST_METHOD(ReplacementHierarchyAndRecency)
    {
        PS::Store store;
        apply(store, L"state=working:app=root");
        apply(store, L"state=blocked:id=a:kind=permission:progress=25:app=parent:title=b2s=:msg=b2s=");
        apply(store, L"state=working:id=a/b");
        auto snapshot = store.GetSnapshot();
        VERIFY_IS_TRUE(snapshot.records[2].effectiveApp == "parent");
        apply(store, L"state=idle:id=a");
        snapshot = store.GetSnapshot();
        VERIFY_IS_FALSE(snapshot.records[1].kind.has_value());
        VERIFY_IS_FALSE(snapshot.records[1].progress.has_value());
        VERIFY_IS_FALSE(snapshot.records[1].title.has_value());
        VERIFY_IS_FALSE(snapshot.records[1].message.has_value());
        VERIFY_IS_TRUE(snapshot.records[2].effectiveApp == "root");
        apply(store, L"state=working:id=ab");
        apply(store, L"state=clear:id=a");
        snapshot = store.GetSnapshot();
        VERIFY_ARE_EQUAL(size_t{ 2 }, snapshot.records.size());
        VERIFY_IS_TRUE(snapshot.records[1].id == "ab");
        apply(store, L"state=clear");
        apply(store, L"state=working:id=old");
        apply(store, L"state=working:id=old/child");
        for (size_t i = 0; i < 254; ++i)
        {
            apply(store, L"state=idle:id=n" + std::to_wstring(i));
        }
        const auto revision = store.GetSnapshot().revision;
        VERIFY_IS_FALSE(PS::Parse(L"state=working:id=old:title=AA==").has_value());
        apply(store, L"state=working:id=old/child");
        VERIFY_ARE_EQUAL(revision, store.GetSnapshot().revision);
        apply(store, L"state=working:id=new");
        snapshot = store.GetSnapshot();
        VERIFY_ARE_EQUAL(size_t{ 256 }, snapshot.records.size());
        VERIFY_IS_TRUE(std::none_of(snapshot.records.begin(), snapshot.records.end(), [](const auto& r) { return r.id == "old"; }));
        VERIFY_IS_TRUE(std::any_of(snapshot.records.begin(), snapshot.records.end(), [](const auto& r) { return r.id == "old/child"; }));
    }

    TEST_METHOD(ResultRevisionsCleanupAndReset)
    {
        PS::Store store;
        apply(store, L"state=done:id=a:msg=b2s=");
        const auto doneRevision = store.GetSnapshot().revision;
        apply(store, L"state=idle:id=a");
        VERIFY_IS_TRUE(store.GetSnapshot().results[0].unread);
        VERIFY_IS_FALSE(store.Acknowledge(doneRevision - 1));
        VERIFY_IS_TRUE(store.Acknowledge(doneRevision));
        apply(store, L"state=done:id=a:msg=b2s=");
        const auto nextResult = store.GetSnapshot().revision;
        VERIFY_IS_TRUE(nextResult > doneRevision);
        VERIFY_IS_TRUE(store.GetSnapshot().results[0].unread);
        VERIFY_IS_TRUE(store.Acknowledge(nextResult));
        const auto acknowledged = store.GetSnapshot().revision;
        apply(store, L"state=done:id=a:msg=b2s=");
        VERIFY_ARE_EQUAL(acknowledged, store.GetSnapshot().revision);
        VERIFY_IS_FALSE(store.GetSnapshot().results[0].unread);
        apply(store, L"state=done:id=a:msg=b2s=:app=renamed");
        VERIFY_IS_FALSE(store.GetSnapshot().results[0].unread);
        VERIFY_ARE_EQUAL(nextResult, store.GetSnapshot().results[0].revision);
        apply(store, L"state=done:id=a:msg=bmV3");
        VERIFY_IS_TRUE(store.GetSnapshot().results[0].unread);
        apply(store, L"state=working:id=b");
        apply(store, L"state=blocked:id=c");
        apply(store, L"state=idle:id=d");
        VERIFY_IS_TRUE(store.ProcessExited());
        VERIFY_ARE_EQUAL(size_t{ 1 }, store.GetSnapshot().records.size());
        VERIFY_IS_TRUE(store.GetSnapshot().results[0].unread);
        apply(store, L"state=idle:id=a");
        VERIFY_IS_TRUE(store.ProcessExited());
        VERIFY_IS_TRUE(store.GetSnapshot().records.empty());
        VERIFY_ARE_EQUAL(size_t{ 1 }, store.GetSnapshot().results.size());
        apply(store, L"state=clear:id=a");
        VERIFY_IS_TRUE(store.GetSnapshot().results.empty());
        VERIFY_IS_TRUE(store.GetSnapshot().observed);
        const auto revision = store.GetSnapshot().revision;
        VERIFY_IS_TRUE(store.Reset());
        apply(store, L"state=error");
        VERIFY_IS_TRUE(store.GetSnapshot().revision > revision);
        VERIFY_IS_FALSE(store.Acknowledge(revision));
        VERIFY_IS_TRUE(store.GetSnapshot().results[0].unread);
        apply(store, L"state=done:id=a");
        apply(store, L"state=done:id=a/child");
        apply(store, L"state=done:id=ab");
        apply(store, L"state=clear:id=a");
        VERIFY_ARE_EQUAL(size_t{ 2 }, store.GetSnapshot().results.size()); // Root and ab only.
    }

    TEST_METHOD(ResultHistoryRemainsBounded)
    {
        PS::Store store;
        for (size_t i = 0; i < 300; ++i)
        {
            const auto id = L":id=n" + std::to_wstring(i);
            apply(store, L"state=done" + id);
            apply(store, L"state=idle" + id);
            store.ProcessExited();
        }
        const auto snapshot = store.GetSnapshot();
        VERIFY_IS_TRUE(snapshot.records.empty());
        VERIFY_ARE_EQUAL(size_t{ 256 }, snapshot.results.size());
        apply(store, L"state=clear");
        VERIFY_IS_TRUE(store.GetSnapshot().results.empty());
    }
};
