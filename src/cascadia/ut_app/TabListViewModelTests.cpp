// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../TerminalApp/TabListViewModel.h"

#include <array>
#include <limits>
#include <type_traits>
#include <vector>

using namespace WEX::TestExecution;
using ViewModel = TerminalApp::TabListViewModel;
using Entry = ViewModel::OwnerEntry;
using Criteria = ViewModel::Criteria;
using Candidate = ViewModel::BindingCandidate;
using Direction = ViewModel::MoveDirection;

static_assert(std::is_constructible_v<ViewModel, std::array<Entry, 4>&, Criteria>);
static_assert(std::is_constructible_v<ViewModel, const std::array<Entry, 4>&, Criteria>);
static_assert(!std::is_constructible_v<ViewModel, std::array<Entry, 4>&&, Criteria>);
static_assert(!std::is_constructible_v<ViewModel, const std::array<Entry, 4>&&, Criteria>);
static_assert(std::is_constructible_v<ViewModel, std::vector<Entry>&, Criteria>);
static_assert(std::is_constructible_v<ViewModel, const std::vector<Entry>&, Criteria>);
static_assert(!std::is_constructible_v<ViewModel, std::vector<Entry>&&, Criteria>);
static_assert(!std::is_constructible_v<ViewModel, const std::vector<Entry>&&, Criteria>);
static_assert(std::is_constructible_v<ViewModel, std::span<const Entry>, Criteria>);
static_assert(std::is_constructible_v<ViewModel, std::span<Entry>, Criteria>);

namespace TerminalAppUnitTests
{
    class TabListViewModelTests
    {
        TEST_CLASS(TabListViewModelTests);

        TEST_METHOD(AllOwnersAndFilteredOwners);
        TEST_METHOD(EmptyAndNobodyEligible);
        TEST_METHOD(OrdinalBoundsAndExplicitLast);
        TEST_METHOD(HiddenNeighborsAndMissingOwners);
        TEST_METHOD(PinPartitions);
        TEST_METHOD(FreshSnapshots);
        TEST_METHOD(MetadataAndIdentityRemainUnchanged);
        TEST_METHOD(StableFilterNumbersDuringSearch);
        TEST_METHOD(SearchDisablesMoveWithoutChangingNeighbors);
        TEST_METHOD(MatchingUsesDisplayedFields);
        TEST_METHOD(CriteriaPreservesChromeSemantics);
        TEST_METHOD(HintTargetsRemainStable);
        TEST_METHOD(DisplayedCycleWrapsAndSkipsHiddenOwners);
        TEST_METHOD(DisplayedCycleFollowsSearchProjection);
        TEST_METHOD(DisplayedCycleHandlesEmptySingleAndMissingSelection);
    };

    void TabListViewModelTests::DisplayedCycleWrapsAndSkipsHiddenOwners()
    {
        const std::array<int, 5> identities{};
        std::array<Entry, 5> entries{};
        for (uint32_t index = 0; index < entries.size(); ++index)
        {
            entries[index] = { &identities[index], index, index < 2, index == 1 || index >= 3 };
        }
        const ViewModel all{ entries, Criteria{ true, true } };
        for (uint32_t index = 0; index < entries.size(); ++index)
        {
            VERIFY_ARE_EQUAL((index + 1) % 5, all.CycleDisplayedTarget(&identities[index], true)->canonicalIndex);
            VERIFY_ARE_EQUAL((index + 4) % 5, all.CycleDisplayedTarget(&identities[index], false)->canonicalIndex);
        }
        const ViewModel agents{ entries, Criteria{ true, true, false, true } };
        VERIFY_ARE_EQUAL(3u, agents.CycleDisplayedTarget(&identities[1], true)->canonicalIndex);
        VERIFY_ARE_EQUAL(4u, agents.CycleDisplayedTarget(&identities[3], true)->canonicalIndex);
        VERIFY_ARE_EQUAL(1u, agents.CycleDisplayedTarget(&identities[4], true)->canonicalIndex);
        VERIFY_ARE_EQUAL(4u, agents.CycleDisplayedTarget(&identities[1], false)->canonicalIndex);
        VERIFY_ARE_EQUAL(1u, agents.CycleDisplayedTarget(&identities[3], false)->canonicalIndex);
        VERIFY_ARE_EQUAL(3u, agents.CycleDisplayedTarget(&identities[2], true)->canonicalIndex);
        VERIFY_ARE_EQUAL(1u, agents.CycleDisplayedTarget(&identities[2], false)->canonicalIndex);
        VERIFY_IS_FALSE(agents.MoveTarget(&identities[1], Direction::Next).has_value(), L"Navigation crosses pin partitions without changing the reordering policy.");
        for (uint32_t index = 0; index < entries.size(); ++index)
        {
            VERIFY_IS_TRUE(entries[index].identity == &identities[index]);
            VERIFY_ARE_EQUAL(index, entries[index].canonicalIndex);
            VERIFY_ARE_EQUAL(index < 2, entries[index].isPinned);
        }
    }

    void TabListViewModelTests::DisplayedCycleFollowsSearchProjection()
    {
        const std::array<int, 4> identities{};
        std::array entries{
            Entry{ &identities[0], 0, false, false, L"ordinary result" },
            Entry{ &identities[1], 1, false, true, L"agent first" },
            Entry{ &identities[2], 2, false, true, L"agent hidden" },
            Entry{ &identities[3], 3, false, true, L"agent last", L"result", true }
        };
        const Criteria search{ true, true, false, true, true, L"result" };
        const ViewModel filtered{ entries, search };
        VERIFY_ARE_EQUAL(3u, filtered.CycleDisplayedTarget(&identities[0], true)->canonicalIndex);
        VERIFY_ARE_EQUAL(0u, filtered.CycleDisplayedTarget(&identities[3], true)->canonicalIndex);
        VERIFY_ARE_EQUAL(3u, filtered.CycleDisplayedTarget(&identities[0], false)->canonicalIndex);
        VERIFY_ARE_EQUAL(0u, filtered.CycleDisplayedTarget(&identities[3], false)->canonicalIndex);
        VERIFY_ARE_EQUAL(1u, filtered.SwitchTarget(0, true)->target.canonicalIndex, L"Numeric targets remain independent of search.");
        VERIFY_ARE_EQUAL(2u, filtered.SwitchTarget(1, true)->target.canonicalIndex);

        entries[3].headerMetadataVisible = false;
        entries[2].children = { { L"result", L"", false } };
        entries[2].childrenVisible = true;
        const ViewModel refreshed{ entries, search };
        VERIFY_ARE_EQUAL(2u, refreshed.CycleDisplayedTarget(&identities[0], true)->canonicalIndex);
        VERIFY_ARE_EQUAL(0u, refreshed.CycleDisplayedTarget(&identities[2], true)->canonicalIndex);
        entries[2].childrenVisible = false;
        const ViewModel collapsedChild{ entries, search };
        VERIFY_ARE_EQUAL(0u, collapsedChild.CycleDisplayedTarget(&identities[0], true)->canonicalIndex);

        auto collapsedCriteria = search;
        collapsedCriteria.railCollapsed = true;
        const ViewModel collapsed{ entries, collapsedCriteria };
        VERIFY_IS_FALSE(collapsed.Policy().searchEffective);
        VERIFY_ARE_EQUAL(1u, collapsed.CycleDisplayedTarget(&identities[0], true)->canonicalIndex);
        VERIFY_ARE_EQUAL(3u, collapsed.CycleDisplayedTarget(&identities[0], false)->canonicalIndex);
    }

    void TabListViewModelTests::DisplayedCycleHandlesEmptySingleAndMissingSelection()
    {
        const int first{}, only{}, last{}, missing{};
        const std::array entries{
            Entry{ &first, 0, false, false }, Entry{ &only, 1, true, true }, Entry{ &last, 2, false, false }
        };
        const Criteria agents{ true, true, false, true };
        const ViewModel empty{ std::span<const Entry>{}, agents };
        const ViewModel one{ entries, agents };
        const ViewModel nobody{ entries, Criteria{ true, true, false, true, true, L"no match" } };
        for (const auto forward : { false, true })
        {
            for (const auto* identity : { &first, &only, &last, &missing, static_cast<const int*>(nullptr) })
            {
                VERIFY_IS_FALSE(empty.CycleDisplayedTarget(identity, forward).has_value());
                VERIFY_IS_FALSE(nobody.CycleDisplayedTarget(identity, forward).has_value());
                const auto target = one.CycleDisplayedTarget(identity, forward);
                VERIFY_IS_TRUE(target.has_value());
                VERIFY_IS_TRUE(target->identity == &only);
                VERIFY_ARE_EQUAL(1u, target->canonicalIndex);
            }
        }
        const ViewModel all{ entries, Criteria{ true, true } };
        VERIFY_ARE_EQUAL(0u, all.CycleDisplayedTarget(&missing, true)->canonicalIndex);
        VERIFY_ARE_EQUAL(2u, all.CycleDisplayedTarget(nullptr, false)->canonicalIndex);
    }

    void TabListViewModelTests::AllOwnersAndFilteredOwners()
    {
        const std::array<int, 4> identities{};
        std::array<Entry, 4> entries{};
        for (uint32_t i = 0; i < entries.size(); ++i)
        {
            entries[i] = { &identities[i], i, false, i % 2 != 0 };
        }
        const ViewModel all{ entries, Criteria{} };
        for (uint32_t i = 0; i < entries.size(); ++i)
        {
            const auto target = all.SwitchTarget(i, true);
            VERIFY_IS_TRUE(target.has_value());
            VERIFY_IS_TRUE(target->target.identity == &identities[i]);
            VERIFY_ARE_EQUAL(i, target->target.canonicalIndex);
            VERIFY_IS_FALSE(target->isLastFallback);
            VERIFY_IS_TRUE(all.StateFor(&identities[i])->displayed);
            VERIFY_ARE_EQUAL(i, *all.StateFor(&identities[i])->navigationOrdinal);
        }
        const ViewModel filtered{ entries, Criteria{ true, true, false, true } };
        VERIFY_ARE_EQUAL(1u, filtered.SwitchTarget(0, true)->target.canonicalIndex);
        VERIFY_ARE_EQUAL(3u, filtered.SwitchTarget(1, true)->target.canonicalIndex);
        VERIFY_IS_FALSE(filtered.StateFor(&identities[0])->displayed);
        VERIFY_IS_FALSE(filtered.StateFor(&identities[0])->navigationOrdinal.has_value());
        VERIFY_IS_TRUE(filtered.StateFor(&identities[1])->agentScopeMatched);
        VERIFY_ARE_EQUAL(0u, *filtered.StateFor(&identities[1])->navigationOrdinal);
    }

    void TabListViewModelTests::EmptyAndNobodyEligible()
    {
        const int identity{};
        const std::array entries{ Entry{ &identity, 0, false, false, L"ordinary" } };
        const Criteria agents{ true, true, false, true };
        const ViewModel empty{ std::span<const Entry>{}, agents };
        const ViewModel nobody{ entries, agents };
        for (const auto* view : { &empty, &nobody })
        {
            for (const auto index : { 0u, 7u, std::numeric_limits<uint32_t>::max() })
            {
                VERIFY_IS_FALSE(view->SwitchTarget(index, true).has_value());
            }
            VERIFY_IS_FALSE(view->MoveTarget(&identity, Direction::Previous).has_value());
            VERIFY_IS_FALSE(view->MoveTarget(&identity, Direction::Next).has_value());
        }
        VERIFY_IS_FALSE(empty.StateFor(&identity).has_value());
        VERIFY_IS_FALSE(nobody.StateFor(&identity)->displayed);
        VERIFY_IS_TRUE(nobody.SwitchTarget(0, false)->target.identity == &identity);
        const ViewModel globalSearch{ entries, Criteria{ true, true, false, true, true, L"ordinary" } };
        VERIFY_IS_TRUE(globalSearch.StateFor(&identity)->displayed);
        VERIFY_IS_FALSE(globalSearch.StateFor(&identity)->navigationOrdinal.has_value());
        VERIFY_IS_FALSE(globalSearch.SwitchTarget(0, true).has_value());
        VERIFY_IS_TRUE(globalSearch.SwitchTarget(0, false).has_value());
    }

    void TabListViewModelTests::OrdinalBoundsAndExplicitLast()
    {
        const std::array<int, 12> identities{};
        std::array<Entry, 12> entries{};
        for (uint32_t i = 0; i < entries.size(); ++i)
        {
            entries[i] = { &identities[i], i, false, true };
        }
        const ViewModel view{ entries, Criteria{} };
        VERIFY_ARE_EQUAL(7u, view.SwitchTarget(7, true)->target.canonicalIndex);
        VERIFY_IS_FALSE(view.SwitchTarget(11, true)->isLastFallback);
        for (const auto index : { 12u, std::numeric_limits<uint32_t>::max() })
        {
            VERIFY_ARE_EQUAL(11u, view.SwitchTarget(index, true)->target.canonicalIndex);
            VERIFY_IS_TRUE(view.SwitchTarget(index, true)->isLastFallback);
        }
        const std::array candidates{ Candidate{ L"last", UINT32_MAX, true }, Candidate{ L"custom", 10, false } };
        VERIFY_ARE_EQUAL(size_t{ 0 }, *view.ChooseHint(&identities.back(), L"", candidates));
        VERIFY_ARE_EQUAL(size_t{ 1 }, *view.ChooseHint(&identities[10], L"", candidates));
        VERIFY_IS_FALSE(view.MoveTarget(&identities.front(), Direction::Previous).has_value());
        VERIFY_IS_FALSE(view.MoveTarget(&identities.back(), Direction::Next).has_value());
    }

    void TabListViewModelTests::HiddenNeighborsAndMissingOwners()
    {
        const int a{}, b{}, c{}, d{}, missing{};
        const std::array entries{
            Entry{ &a, 0, false, false }, Entry{ &b, 1, false, true }, Entry{ &c, 2, false, false }, Entry{ &d, 3, false, true }
        };
        const ViewModel view{ entries, Criteria{ true, true, false, true } };
        VERIFY_IS_TRUE(view.MoveTarget(&d, Direction::Previous)->identity == &b);
        VERIFY_IS_TRUE(view.MoveTarget(&b, Direction::Next)->identity == &d);
        VERIFY_IS_TRUE(view.StateFor(&b)->canMoveNext);
        VERIFY_IS_FALSE(view.StateFor(&b)->canMovePrevious);
        for (const auto* identity : { &a, &c, &missing })
        {
            VERIFY_IS_FALSE(view.MoveTarget(identity, Direction::Previous).has_value());
            VERIFY_IS_FALSE(view.MoveTarget(identity, Direction::Next).has_value());
        }
        VERIFY_IS_FALSE(view.StateFor(&missing).has_value());
        VERIFY_IS_FALSE(view.ChooseHint(&missing, L"", {}).has_value());
    }

    void TabListViewModelTests::PinPartitions()
    {
        const std::array<int, 8> identities{};
        std::array<Entry, 8> entries{};
        for (uint32_t i = 0; i < entries.size(); ++i)
        {
            entries[i] = { &identities[i], i, i < 4, i % 2 == 0 };
        }
        const Criteria agents{ true, true, false, true };
        const ViewModel view{ entries, agents };
        VERIFY_ARE_EQUAL(0u, view.MoveTarget(&identities[2], Direction::Previous)->canonicalIndex);
        VERIFY_ARE_EQUAL(2u, view.MoveTarget(&identities[0], Direction::Next)->canonicalIndex);
        VERIFY_IS_FALSE(view.MoveTarget(&identities[2], Direction::Next).has_value());
        VERIFY_IS_FALSE(view.MoveTarget(&identities[4], Direction::Previous).has_value());
        VERIFY_ARE_EQUAL(6u, view.MoveTarget(&identities[4], Direction::Next)->canonicalIndex);
        VERIFY_ARE_EQUAL(4u, view.SwitchTarget(2, true)->target.canonicalIndex);
        for (const bool pinned : { false, true })
        {
            auto samePartition = entries;
            for (auto& entry : samePartition)
            {
                entry.isPinned = pinned;
            }
            const ViewModel onePartition{ samePartition, agents };
            VERIFY_ARE_EQUAL(4u, onePartition.MoveTarget(&identities[2], Direction::Next)->canonicalIndex);
        }
        auto noPinnedAgents = entries;
        noPinnedAgents[0].isAgent = false;
        noPinnedAgents[2].isAgent = false;
        const ViewModel emptyPartition{ noPinnedAgents, agents };
        VERIFY_IS_FALSE(emptyPartition.MoveTarget(&identities[4], Direction::Previous).has_value());
    }

    void TabListViewModelTests::FreshSnapshots()
    {
        const int a{}, b{}, c{}, d{};
        const std::array original{
            Entry{ &a, 0, false, false }, Entry{ &b, 1, false, true }, Entry{ &c, 2, false, false }, Entry{ &d, 3, false, true }
        };
        const std::array reordered{
            Entry{ &a, 0, false, false }, Entry{ &d, 1, false, true }, Entry{ &b, 2, false, true }, Entry{ &c, 3, false, false }
        };
        const Criteria agents{ true, true, false, true };
        const ViewModel before{ original, agents };
        const ViewModel after{ reordered, agents };
        VERIFY_IS_TRUE(before.SwitchTarget(0, true)->target.identity == &b);
        VERIFY_IS_TRUE(after.SwitchTarget(0, true)->target.identity == &d);
        VERIFY_ARE_EQUAL(2u, after.MoveTarget(&d, Direction::Next)->canonicalIndex);
        auto changed = reordered;
        changed[1].isAgent = false;
        const ViewModel newFacts{ changed, agents };
        VERIFY_IS_TRUE(newFacts.SwitchTarget(0, true)->target.identity == &b);
        VERIFY_IS_FALSE(newFacts.StateFor(&d)->displayed);
        VERIFY_IS_TRUE(after.StateFor(&d)->displayed);
    }

    void TabListViewModelTests::MetadataAndIdentityRemainUnchanged()
    {
        const int mixedOwner = 42;
        std::wstring source = L"mixed";
        std::wstring query = L"MIX";
        const std::array entries{ Entry{ &mixedOwner, 17, true, true, source, L"header", true, true, { { L"ordinary sibling", L"session", true } } } };
        Criteria criteria{ true, true, false, true, true, query };
        const ViewModel view{ entries, criteria };
        source.assign(L"changed");
        query.clear();
        criteria.query.assign(L"not present");
        VERIFY_IS_TRUE(view.StateFor(&mixedOwner)->displayed);
        VERIFY_IS_TRUE(view.SwitchTarget(0, true)->target.identity == &mixedOwner);
        VERIFY_ARE_EQUAL(17u, view.SwitchTarget(0, true)->target.canonicalIndex);
        VERIFY_IS_FALSE(view.MoveTarget(&mixedOwner, Direction::Next).has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"mixed" }, entries[0].title);
        VERIFY_ARE_EQUAL(std::wstring{ L"session" }, entries[0].children[0].metadataText);
        VERIFY_IS_TRUE(entries[0].isPinned);
        VERIFY_IS_TRUE(entries[0].isAgent);
        VERIFY_ARE_EQUAL(17u, entries[0].canonicalIndex);
        VERIFY_ARE_EQUAL(42, mixedOwner);
    }

    void TabListViewModelTests::StableFilterNumbersDuringSearch()
    {
        const std::array<int, 4> identities{};
        const std::array entries{
            Entry{ &identities[0], 0, false, false, L"A" },
            Entry{ &identities[1], 1, false, true, L"B" },
            Entry{ &identities[2], 2, false, false, L"C" },
            Entry{ &identities[3], 3, false, true, L"D" }
        };
        for (const bool agents : { false, true })
        {
            for (const auto& query : { std::wstring{}, std::wstring{ L"C" }, std::wstring{ L"missing" } })
            {
                for (const bool active : { false, true })
                {
                    const ViewModel view{ entries, Criteria{ true, true, false, agents, active, query } };
                    VERIFY_ARE_EQUAL(agents ? 1u : 0u, view.SwitchTarget(0, true)->target.canonicalIndex);
                    VERIFY_ARE_EQUAL(agents ? 3u : 1u, view.SwitchTarget(1, true)->target.canonicalIndex);
                    VERIFY_ARE_EQUAL(3u, view.SwitchTarget(UINT32_MAX, true)->target.canonicalIndex);
                    VERIFY_ARE_EQUAL(2u, view.SwitchTarget(2, false)->target.canonicalIndex);
                    for (uint32_t i = 0; i < entries.size(); ++i)
                    {
                        const auto state = view.StateFor(&identities[i]);
                        const auto displayed = query.empty() ? (!agents || entries[i].isAgent) : query == entries[i].title;
                        VERIFY_ARE_EQUAL(displayed, state->displayed);
                        VERIFY_ARE_EQUAL(!agents || entries[i].isAgent, state->navigationOrdinal.has_value());
                    }
                }
            }
        }
    }

    void TabListViewModelTests::SearchDisablesMoveWithoutChangingNeighbors()
    {
        const int a{}, b{}, c{};
        const std::array entries{
            Entry{ &a, 0, false, true }, Entry{ &b, 1, false, false }, Entry{ &c, 2, false, true }
        };
        for (const bool agents : { false, true })
        {
            const Criteria base{ true, true, false, agents };
            const ViewModel before{ entries, base };
            VERIFY_ARE_EQUAL(agents ? 2u : 1u, before.MoveTarget(&a, Direction::Next)->canonicalIndex);
            for (const auto& query : { std::wstring{}, std::wstring{ L"missing" } })
            {
                auto search = base;
                search.searchActive = true;
                search.query = query;
                const ViewModel during{ entries, search };
                VERIFY_IS_TRUE(during.Policy().searchEffective);
                VERIFY_IS_TRUE(during.Policy().positionOperationsBlocked);
                VERIFY_IS_FALSE(during.StateFor(&a)->canMovePrevious);
                VERIFY_IS_FALSE(during.StateFor(&a)->canMoveNext);
                VERIFY_IS_FALSE(during.MoveTarget(&c, Direction::Previous).has_value());
                VERIFY_ARE_EQUAL(agents ? 2u : 1u, during.SwitchTarget(1, true)->target.canonicalIndex);
            }
            const ViewModel after{ entries, base };
            VERIFY_IS_TRUE(after.StateFor(&a)->canMoveNext);
            VERIFY_ARE_EQUAL(agents ? 2u : 1u, after.MoveTarget(&a, Direction::Next)->canonicalIndex);
        }
    }

    void TabListViewModelTests::MatchingUsesDisplayedFields()
    {
        const int identity{};
        const std::array original{ Entry{ &identity, 0, false, false, L"\u00c4 owner", std::wstring{ L"x\0Y", 3 }, true, true, { { L"child title", L"pane meta", true } } } };
        struct Case
        {
            std::wstring query;
            bool headerVisible;
            bool childrenVisible;
            bool childMetadataVisible;
            bool expected;
        };
        const std::array cases{
            Case{ L"\u00e4 OWNER", false, false, false, true },
            Case{ std::wstring{ L"\0y", 2 }, true, false, false, true },
            Case{ std::wstring{ L"\0y", 2 }, false, true, true, false },
            Case{ L"CHILD TITLE", false, true, false, true },
            Case{ L"child title", true, false, true, false },
            Case{ L"PANE META", false, true, true, true },
            Case{ L"pane meta", true, true, false, false },
            Case{ L"longer than any available searchable field", true, true, true, false }
        };
        for (const auto& test : cases)
        {
            auto entries = original;
            entries[0].headerMetadataVisible = test.headerVisible;
            entries[0].childrenVisible = test.childrenVisible;
            entries[0].children[0].metadataVisible = test.childMetadataVisible;
            const ViewModel view{ entries, Criteria{ true, true, false, false, true, test.query } };
            VERIFY_ARE_EQUAL(test.expected, view.StateFor(&identity)->searchMatched);
            VERIFY_ARE_EQUAL(test.expected, view.StateFor(&identity)->displayed);
        }
    }

    void TabListViewModelTests::CriteriaPreservesChromeSemantics()
    {
        const int ordinary{}, agent{};
        const std::array entries{ Entry{ &ordinary, 0, false, false, L"ordinary" }, Entry{ &agent, 1, false, true, L"agent" } };
        struct Case
        {
            Criteria criteria;
            bool search;
            bool navigationScope;
            bool displayScope;
        };
        const std::array cases{
            Case{ { false, true, false, true, true, L"missing" }, false, false, false },
            Case{ { true, false, false, true, true, L"missing" }, false, true, false },
            Case{ { true, true, true, true, true, L"missing" }, false, true, false },
            Case{ { true, false, false, true, true, L"" }, false, true, true },
            Case{ { true, true, true, true, true, L"" }, false, true, true },
            Case{ { true, true, false, true, true, L"" }, true, true, true },
            Case{ { true, true, false, true, false, L"missing" }, true, true, false },
            Case{ { true, true, false, false, false, L"" }, false, false, false }
        };
        for (const auto& test : cases)
        {
            const ViewModel view{ entries, test.criteria };
            const auto policy = view.Policy();
            VERIFY_ARE_EQUAL(test.search, policy.searchEffective);
            VERIFY_ARE_EQUAL(test.navigationScope, policy.navigationAgentScopeEffective);
            VERIFY_ARE_EQUAL(test.displayScope, policy.displayAgentScopeEffective);
            VERIFY_ARE_EQUAL(test.search || test.displayScope, policy.positionOperationsBlocked);
            VERIFY_ARE_EQUAL(test.navigationScope ? 1u : 0u, view.SwitchTarget(0, true)->target.canonicalIndex);
            VERIFY_ARE_EQUAL(!test.displayScope && !test.search, view.StateFor(nullptr)->displayed);
            VERIFY_IS_FALSE(view.StateFor(nullptr)->agentScopeMatched);
            VERIFY_IS_FALSE(view.StateFor(nullptr)->navigationOrdinal.has_value());
            VERIFY_ARE_EQUAL(!test.displayScope && !test.search, view.StateFor(&ordinary)->displayed);
        }
    }

    void TabListViewModelTests::HintTargetsRemainStable()
    {
        const int a{}, b{}, c{}, d{};
        const std::array entries{
            Entry{ &a, 0, false, false, L"A" }, Entry{ &b, 1, false, true, L"B" }, Entry{ &c, 2, false, false, L"C" }, Entry{ &d, 3, false, true, L"D" }
        };
        const std::array candidates{
            Candidate{ L"canonical-B", 1, false }, Candidate{ L"ctrl+alt+1", 0, true }, Candidate{ L"custom-C", 2, false }, Candidate{ L"ctrl+alt+2", 1, true }, Candidate{ L"last", UINT32_MAX, true }, Candidate{ L"overflow", 20, true }
        };
        for (const auto& query : { std::wstring{}, std::wstring{ L"C" }, std::wstring{ L"missing" } })
        {
            const ViewModel view{ entries, Criteria{ true, true, false, true, true, query } };
            VERIFY_ARE_EQUAL(size_t{ 0 }, *view.ChooseHint(&b, L"canonical-B", candidates));
            VERIFY_ARE_EQUAL(size_t{ 1 }, *view.ChooseHint(&b, L"", candidates));
            VERIFY_ARE_EQUAL(size_t{ 2 }, *view.ChooseHint(&c, L"", candidates));
            VERIFY_ARE_EQUAL(size_t{ 3 }, *view.ChooseHint(&d, L"last", candidates));
        }
        const ViewModel view{ entries, Criteria{ true, true, false, true } };
        const std::array ties{ Candidate{ L"z", 1, false }, Candidate{ L"a", 1, false }, Candidate{ L"a", 1, false } };
        VERIFY_ARE_EQUAL(size_t{ 1 }, *view.ChooseHint(&b, L"", ties));
        const std::array lastTies{ Candidate{ L"z", UINT32_MAX, true }, Candidate{ L"a", 20, true }, Candidate{ L"a", 20, true } };
        VERIFY_ARE_EQUAL(size_t{ 1 }, *view.ChooseHint(&d, L"z", lastTies));
        const std::array remapped{ Candidate{ L"ctrl+alt+1", 2, false } };
        VERIFY_ARE_EQUAL(size_t{ 0 }, *view.ChooseHint(&c, L"", remapped));
        VERIFY_IS_FALSE(view.ChooseHint(&b, L"", remapped).has_value());
        VERIFY_IS_FALSE(view.ChooseHint(&b, L"", {}).has_value());
        VERIFY_IS_FALSE(view.ChooseHint(nullptr, L"", candidates).has_value());
    }
}
