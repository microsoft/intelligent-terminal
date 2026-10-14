// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <cstddef>
#include <cstdint>
#include <limits>
#include <optional>
#include <ranges>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include <til.h>

namespace TerminalApp
{
    class TabListViewModel
    {
    public:
        struct ChildFields
        {
            std::wstring title;
            std::wstring metadataText;
            bool metadataVisible = false;
        };

        struct OwnerEntry
        {
            const void* identity;
            uint32_t canonicalIndex;
            bool isPinned;
            bool isAgent;
            std::wstring title;
            std::wstring headerMetadataText;
            bool headerMetadataVisible = false;
            bool childrenVisible = false;
            std::vector<ChildFields> children;
        };

        struct Criteria
        {
            bool verticalLayout = false;
            bool railVisible = false;
            bool railCollapsed = false;
            bool agentsOnlySelected = false;
            bool searchActive = false;
            std::wstring query;
        };

        struct DerivedPolicy
        {
            bool searchEffective;
            bool navigationAgentScopeEffective;
            bool displayAgentScopeEffective;
            bool positionOperationsBlocked;
        };

        struct OwnerState
        {
            bool agentScopeMatched;
            bool searchMatched;
            bool displayed;
            std::optional<uint32_t> navigationOrdinal;
            bool canMovePrevious;
            bool canMoveNext;
        };

        struct Target
        {
            const void* identity;
            uint32_t canonicalIndex;
        };

        struct SwitchResult
        {
            Target target;
            bool isLastFallback;
        };

        struct BindingCandidate
        {
            std::wstring text;
            uint32_t switchIndex;
            bool useFilterNavigation;
        };

        enum class MoveDirection
        {
            Previous,
            Next,
        };

        // Borrows immutable canonical-order metadata with the canonical pinned prefix.
        // The backing storage must outlive this object; identity tokens are
        // compared, never dereferenced, even when an explicit span is supplied.
        explicit TabListViewModel(std::span<const OwnerEntry> owners, Criteria criteria) :
            _owners{ owners },
            _criteria{ std::move(criteria) }
        {
        }

        template<typename Owners>
            requires(std::ranges::contiguous_range<Owners> && !std::ranges::borrowed_range<Owners>)
        TabListViewModel(Owners&&, Criteria) = delete;

        [[nodiscard]] DerivedPolicy Policy() const noexcept
        {
            const auto search = _criteria.verticalLayout && _criteria.railVisible && !_criteria.railCollapsed &&
                                (_criteria.searchActive || !_criteria.query.empty());
            const auto navigationScope = _criteria.verticalLayout && _criteria.agentsOnlySelected;
            const auto displayScope = navigationScope && _criteria.query.empty();
            return { search, navigationScope, displayScope, displayScope || search };
        }

        [[nodiscard]] std::optional<OwnerState> StateFor(const void* identity) const noexcept
        {
            const auto policy = Policy();
            // Compatibility for the Page's null-owner projection wrapper, not a canonical entry.
            if (!identity)
            {
                return OwnerState{ false, !policy.searchEffective, !policy.searchEffective && !policy.displayAgentScopeEffective, std::nullopt, false, false };
            }
            uint32_t ordinal = 0;
            for (const auto& owner : _owners)
            {
                const auto navigable = !policy.navigationAgentScopeEffective || owner.isAgent;
                if (owner.identity == identity)
                {
                    const auto searchMatched = _MatchesSearch(owner);
                    return OwnerState{
                        owner.isAgent,
                        searchMatched,
                        (!policy.displayAgentScopeEffective || owner.isAgent) && searchMatched,
                        navigable ? std::optional{ ordinal } : std::nullopt,
                        MoveTarget(identity, MoveDirection::Previous).has_value(),
                        MoveTarget(identity, MoveDirection::Next).has_value()
                    };
                }
                if (navigable)
                {
                    ++ordinal;
                }
            }
            return std::nullopt;
        }

        [[nodiscard]] std::optional<SwitchResult> SwitchTarget(uint32_t index, bool useFilterNavigation) const noexcept
        {
            std::optional<Target> last;
            const auto agentScope = useFilterNavigation && Policy().navigationAgentScopeEffective;
            for (const auto& owner : _owners)
            {
                if (!agentScope || owner.isAgent)
                {
                    last = Target{ owner.identity, owner.canonicalIndex };
                    if (index == 0)
                    {
                        return SwitchResult{ *last, false };
                    }
                    if (index != std::numeric_limits<uint32_t>::max())
                    {
                        --index;
                    }
                }
            }
            return last ? std::optional{ SwitchResult{ *last, true } } : std::nullopt;
        }

        [[nodiscard]] std::optional<Target> MoveTarget(const void* identity, MoveDirection direction) const noexcept
        {
            const auto policy = Policy();
            if (policy.searchEffective || !identity)
            {
                return std::nullopt;
            }
            for (std::size_t i = 0; i < _owners.size(); ++i)
            {
                const auto& owner = _owners[i];
                if (owner.identity != identity)
                {
                    continue;
                }
                if (policy.navigationAgentScopeEffective && !owner.isAgent)
                {
                    return std::nullopt;
                }
                if (direction == MoveDirection::Previous)
                {
                    while (i > 0)
                    {
                        const auto& neighbor = _owners[--i];
                        if (neighbor.isPinned != owner.isPinned)
                        {
                            break;
                        }
                        if (!policy.navigationAgentScopeEffective || neighbor.isAgent)
                        {
                            return Target{ neighbor.identity, neighbor.canonicalIndex };
                        }
                    }
                }
                else
                {
                    while (++i < _owners.size())
                    {
                        const auto& neighbor = _owners[i];
                        if (neighbor.isPinned != owner.isPinned)
                        {
                            break;
                        }
                        if (!policy.navigationAgentScopeEffective || neighbor.isAgent)
                        {
                            return Target{ neighbor.identity, neighbor.canonicalIndex };
                        }
                    }
                }
                return std::nullopt;
            }
            return std::nullopt;
        }

        [[nodiscard]] std::optional<std::size_t> ChooseHint(const void* identity, std::wstring_view canonicalText, std::span<const BindingCandidate> candidates) const noexcept
        {
            const auto state = StateFor(identity);
            if (!identity || !state)
            {
                return std::nullopt;
            }
            std::optional<std::size_t> chosen;
            int bestRank = 4;
            for (std::size_t i = 0; i < candidates.size(); ++i)
            {
                const auto& candidate = candidates[i];
                const auto result = SwitchTarget(candidate.switchIndex, candidate.useFilterNavigation);
                if (!result || result->target.identity != identity)
                {
                    continue;
                }
                const auto rank = result->isLastFallback                                                             ? 3 :
                                  candidate.text == canonicalText                                                    ? 0 :
                                  candidate.useFilterNavigation && state->navigationOrdinal == candidate.switchIndex ? 1 :
                                                                                                                       2;
                if (rank < bestRank || (rank == bestRank && chosen && candidate.text < candidates[*chosen].text))
                {
                    chosen = i;
                    bestRank = rank;
                }
            }
            return chosen;
        }

    private:
        [[nodiscard]] bool _MatchesSearch(const OwnerEntry& owner) const noexcept
        {
            if (!Policy().searchEffective || _criteria.query.empty())
            {
                return true;
            }
            const std::wstring_view query{ _criteria.query.data(), _criteria.query.size() };
            const auto matches = [&](std::wstring_view value) {
                if (query.size() > value.size())
                {
                    return false;
                }
                for (std::size_t offset = 0; offset + query.size() <= value.size(); ++offset)
                {
                    if (til::compare_ordinal_insensitive(value.substr(offset, query.size()), query) == 0)
                    {
                        return true;
                    }
                }
                return false;
            };
            if (matches(owner.title) || (owner.headerMetadataVisible && matches(owner.headerMetadataText)))
            {
                return true;
            }
            if (owner.childrenVisible)
            {
                for (const auto& child : owner.children)
                {
                    if (matches(child.title) || (child.metadataVisible && matches(child.metadataText)))
                    {
                        return true;
                    }
                }
            }
            return false;
        }

        std::span<const OwnerEntry> _owners;
        Criteria _criteria;
    };
}
