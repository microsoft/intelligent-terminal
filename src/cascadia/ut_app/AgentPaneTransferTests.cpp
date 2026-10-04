// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"
#include "../TerminalApp/AgentPaneDragStash.h"

#include <array>
#include <future>
#include <type_traits>

using namespace WEX::TestExecution;
using namespace winrt::TerminalApp::implementation;

namespace TerminalAppUnitTests
{
    static_assert(!std::is_copy_constructible_v<AgentPaneLifetime>);
    static_assert(!std::is_copy_assignable_v<AgentPaneLifetime>);
    static_assert(std::is_nothrow_move_constructible_v<AgentPaneLifetime>);
    static_assert(std::is_nothrow_move_assignable_v<AgentPaneLifetime>);
    static_assert(!std::is_copy_constructible_v<AgentPaneDragStash::Entry>);
    static_assert(!std::is_copy_assignable_v<AgentPaneDragStash::Entry>);
    static_assert(std::is_nothrow_move_constructible_v<AgentPaneDragStash::Entry>);

    class AgentPaneTransferTests
    {
        TEST_CLASS(AgentPaneTransferTests);

        TEST_METHOD(ClaimMovesMetadataExactlyOnce);
        TEST_METHOD(StaleClaimCannotTakeReplacement);
        TEST_METHOD(ExpiryCannotTakeClaimedOrReplacementTransfer);
        TEST_METHOD(DuplicateStoreDoesNotReplaceOwner);
        TEST_METHOD(RejectsInvalidTransferIdentity);
        TEST_METHOD(ClaimRequiresBothContentAndTransferIdentity);
        TEST_METHOD(ConcurrentReceiveAndExpiryHaveSingleClaimant);
        TEST_METHOD(ConcurrentDuplicateStoresPreserveWinningEntry);
    };

    namespace
    {
        AgentPaneDragStash::Entry Transfer(const uint64_t id)
        {
            AgentPaneDragStash::Entry entry;
            entry.transferId = id;
            entry.originalTabId = L"source-tab";
            entry.sourceProfileGuid = winrt::guid{ 0x12345678, 0xabcd, 0x1234, { 1, 2, 3, 4, 5, 6, 7, 8 } };
            entry.hidden = true;
            entry.sessionsView = true;
            entry.panePosition = L"right";
            entry.attachDisposition = AgentPaneDragStash::AttachDisposition::FirstPaneOfNewTab;
            return entry;
        }
    }

    void AgentPaneTransferTests::ClaimMovesMetadataExactlyOnce()
    {
        AgentPaneDragStash stash;
        VERIFY_ARE_EQUAL(uint64_t{ 11 }, stash.Store(7, Transfer(11)));
        auto claimed = stash.Take(7, 11);
        VERIFY_IS_TRUE(claimed.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"source-tab" }, claimed->originalTabId);
        VERIFY_ARE_EQUAL(std::wstring{ L"right" }, claimed->panePosition);
        VERIFY_IS_TRUE(claimed->sourceProfileGuid.has_value());
        VERIFY_IS_TRUE(claimed->sourceProfileGuid == Transfer(11).sourceProfileGuid);
        VERIFY_ARE_EQUAL(uint64_t{ 11 }, claimed->transferId);
        VERIFY_IS_TRUE(claimed->hidden);
        VERIFY_IS_TRUE(claimed->sessionsView);
        VERIFY_IS_TRUE(claimed->attachDisposition == AgentPaneDragStash::AttachDisposition::FirstPaneOfNewTab);
        VERIFY_IS_FALSE(stash.Take(7, 11).has_value());
    }

    void AgentPaneTransferTests::StaleClaimCannotTakeReplacement()
    {
        AgentPaneDragStash stash;
        stash.Store(7, Transfer(11));
        VERIFY_IS_TRUE(stash.Take(7, 11).has_value());
        stash.Store(7, Transfer(12));
        VERIFY_IS_FALSE(stash.Take(7, 11).has_value());
        VERIFY_IS_TRUE(stash.Take(7, 12).has_value());
    }

    void AgentPaneTransferTests::ExpiryCannotTakeClaimedOrReplacementTransfer()
    {
        AgentPaneDragStash stash;
        stash.Store(7, Transfer(11));
        auto receiving = stash.Take(7, 11);
        VERIFY_IS_TRUE(receiving.has_value());
        VERIFY_IS_FALSE(stash.Take(7, 11).has_value());
        stash.Store(7, Transfer(12));
        VERIFY_IS_FALSE(stash.Take(7, 11).has_value());
        auto abandoned = stash.Take(7, 12);
        VERIFY_IS_TRUE(abandoned.has_value());
        VERIFY_IS_FALSE(stash.Take(7, 12).has_value());
    }

    void AgentPaneTransferTests::DuplicateStoreDoesNotReplaceOwner()
    {
        AgentPaneDragStash stash;
        stash.Store(7, Transfer(11));
        VERIFY_THROWS(stash.Store(7, Transfer(11)), wil::ResultException);
        VERIFY_THROWS(stash.Store(7, Transfer(12)), wil::ResultException);
        VERIFY_IS_FALSE(stash.Take(7, 12).has_value());
        auto incumbent = stash.Take(7, 11);
        VERIFY_IS_TRUE(incumbent.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"source-tab" }, incumbent->originalTabId);
    }

    void AgentPaneTransferTests::RejectsInvalidTransferIdentity()
    {
        AgentPaneDragStash stash;
        VERIFY_THROWS(stash.Store(0, Transfer(11)), wil::ResultException);
        VERIFY_THROWS(stash.Store(7, Transfer(0)), wil::ResultException);
        VERIFY_IS_FALSE(stash.Take(0, 11).has_value());
        VERIFY_IS_FALSE(stash.Take(7, 0).has_value());
        VERIFY_ARE_EQUAL(uint64_t{ 11 }, stash.Store(7, Transfer(11)));
        VERIFY_IS_TRUE(stash.Take(7, 11).has_value());
    }

    void AgentPaneTransferTests::ClaimRequiresBothContentAndTransferIdentity()
    {
        AgentPaneDragStash stash;
        stash.Store(7, Transfer(11));
        stash.Store(8, Transfer(12));
        VERIFY_IS_FALSE(stash.Take(7, 12).has_value());
        VERIFY_IS_FALSE(stash.Take(8, 11).has_value());
        VERIFY_IS_FALSE(stash.Take(0, 11).has_value());
        VERIFY_IS_FALSE(stash.Take(7, 0).has_value());
        VERIFY_IS_TRUE(stash.Take(7, 11).has_value());
        VERIFY_IS_TRUE(stash.Take(8, 12).has_value());
    }

    void AgentPaneTransferTests::ConcurrentReceiveAndExpiryHaveSingleClaimant()
    {
        AgentPaneDragStash stash;
        stash.Store(7, Transfer(11));
        std::array<std::future<std::optional<AgentPaneDragStash::Entry>>, 2> claims;
        std::promise<void> start;
        const auto ready = start.get_future().share();
        // Release any launched worker even if launching the next one throws.
        auto releaseStart = wil::scope_exit([&]() noexcept { start.set_value(); });
        for (auto& claim : claims)
        {
            claim = std::async(std::launch::async, [&, ready]() {
                ready.wait();
                return stash.Take(7, 11);
            });
        }
        start.set_value();
        releaseStart.release();

        auto receiving = claims[0].get();
        auto expiring = claims[1].get();
        VERIFY_IS_TRUE(receiving.has_value() != expiring.has_value());
        VERIFY_IS_FALSE(stash.Take(7, 11).has_value());

        // Neither a late loser nor destruction of the old claimed metadata
        // may remove a replacement registered under the same content ID.
        stash.Store(7, Transfer(12));
        receiving.reset();
        expiring.reset();
        VERIFY_IS_FALSE(stash.Take(7, 11).has_value());
        VERIFY_IS_TRUE(stash.Take(7, 12).has_value());
    }

    void AgentPaneTransferTests::ConcurrentDuplicateStoresPreserveWinningEntry()
    {
        AgentPaneDragStash stash;
        std::array<std::future<HRESULT>, 2> stores;
        std::promise<void> start;
        const auto ready = start.get_future().share();
        auto releaseStart = wil::scope_exit([&]() noexcept { start.set_value(); });
        for (size_t i = 0; i < stores.size(); ++i)
        {
            stores[i] = std::async(std::launch::async, [&, i, ready]() -> HRESULT {
                ready.wait();
                try
                {
                    stash.Store(7, Transfer(11 + i));
                    return S_OK;
                }
                catch (const wil::ResultException& error)
                {
                    return error.GetErrorCode();
                }
            });
        }
        start.set_value();
        releaseStart.release();

        const auto first = stores[0].get();
        const auto second = stores[1].get();
        VERIFY_IS_TRUE((first == S_OK && second == E_ILLEGAL_METHOD_CALL) ||
                       (second == S_OK && first == E_ILLEGAL_METHOD_CALL));
        const uint64_t winner = first == S_OK ? 11 : 12;
        const uint64_t loser = first == S_OK ? 12 : 11;
        VERIFY_IS_FALSE(stash.Take(7, loser).has_value());
        auto claimed = stash.Take(7, winner);
        VERIFY_IS_TRUE(claimed.has_value());
        VERIFY_ARE_EQUAL(winner, claimed->transferId);
        VERIFY_IS_FALSE(stash.Take(7, winner).has_value());
    }

}
