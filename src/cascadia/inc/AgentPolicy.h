// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

// AgentPolicy.h — Centralized GPO (Group Policy) reader for AI agent settings.
//
// Reads registry values under Software\Policies\Microsoft\IntelligentTerminal
// (HKLM first, then HKCU) and caches the results. All consumers query this
// module instead of reading the registry directly.
//
// Header-only: each consuming DLL gets its own inline-static cache. This avoids
// cross-DLL export issues while ensuring each module has a consistent view of
// the registry state after Reload().
//
// Policy values:
//   AllowedAgents      REG_MULTI_SZ  Allowlist of agent IDs. Absent = all allowed.
//   AllowCustomAgents  REG_DWORD     0 = blocked, 1 = allowed. Absent = allowed.
//   AllowAutoFix       REG_DWORD     0 = blocked, 1 = allowed. Absent = allowed.
//   AllowAgentSessionHooks REG_DWORD  0 = blocked, 1 = allowed. Absent = allowed.

#pragma once

#include <atomic>
#include <memory>
#include <set>
#include <optional>
#include <string>
#include <string_view>
#include <mutex>
#include <vector>

namespace Microsoft::Terminal::Settings::Model::AgentPolicy
{
    // Case-insensitive comparison for agent IDs (e.g. "Copilot" should match "copilot").
    // Uses CompareStringOrdinal to avoid per-comparison heap allocations.
    struct CaseInsensitiveLess
    {
        using is_transparent = void;
        bool operator()(std::wstring_view a, std::wstring_view b) const
        {
            return CompareStringOrdinal(
                       a.data(), static_cast<int>(a.size()),
                       b.data(), static_cast<int>(b.size()),
                       TRUE) == CSTR_LESS_THAN;
        }
    };

    // Whether a particular feature is allowed, blocked, or unset by policy.
    enum class PolicyState
    {
        NotConfigured, // IT admin has not set this policy — feature is allowed by default
        Allowed,       // Explicitly allowed
        Blocked        // Explicitly blocked
    };

    // Snapshot of all agent-related GPO values.
    struct PolicySnapshot
    {
        // nullopt = AllowedAgents not configured (all allowed).
        // empty set = configured but empty (none allowed).
        std::optional<std::set<std::wstring, CaseInsensitiveLess>> allowedAgents;

        PolicyState customAgents{ PolicyState::NotConfigured };
        PolicyState autoFix{ PolicyState::NotConfigured };
        PolicyState agentSessionHooks{ PolicyState::NotConfigured };
    };

    // ── Private implementation details ──────────────────────────────────

    inline constexpr wchar_t PolicyRegKey[] = LR"(Software\Policies\Microsoft\IntelligentTerminal)";

    // Per-DLL cached snapshot, protected by a mutex.
    // Reload() builds a new snapshot and swaps it under the lock.
    // _GetSnapshot() reads the shared_ptr under the same lock.
    inline std::mutex s_policyMutex;
    inline std::shared_ptr<const PolicySnapshot> s_snapshot;
    inline std::atomic_bool s_loaded{ false }; // true once Reload() has run in this DLL

    inline std::optional<DWORD> _ReadDwordPolicy(const wchar_t* valueName)
    {
        for (const auto key : { HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER })
        {
            DWORD value{};
            DWORD size = sizeof(value);
            if (RegGetValueW(key, PolicyRegKey, valueName, RRF_RT_REG_DWORD, nullptr, &value, &size) == ERROR_SUCCESS)
            {
                return value;
            }
        }
        return std::nullopt;
    }

    inline std::optional<std::set<std::wstring, CaseInsensitiveLess>> _ReadMultiSzPolicy(const wchar_t* valueName)
    {
        for (const auto key : { HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER })
        {
            DWORD bufferSize = 0;
            auto result = RegGetValueW(key, PolicyRegKey, valueName, RRF_RT_REG_MULTI_SZ, nullptr, nullptr, &bufferSize);
            if (result != ERROR_SUCCESS || bufferSize == 0)
            {
                continue;
            }

            std::vector<wchar_t> buffer(bufferSize / sizeof(wchar_t));
            result = RegGetValueW(key, PolicyRegKey, valueName, RRF_RT_REG_MULTI_SZ, nullptr, buffer.data(), &bufferSize);
            if (result != ERROR_SUCCESS)
            {
                continue;
            }

            std::set<std::wstring, CaseInsensitiveLess> values;
            for (auto p = buffer.data(); *p;)
            {
                const auto len = wcslen(p);
                values.emplace(p, len);
                p += len + 1;
            }
            return values;
        }
        return std::nullopt;
    }

    inline PolicyState _DwordToPolicyState(std::optional<DWORD> val)
    {
        if (!val.has_value())
        {
            return PolicyState::NotConfigured;
        }
        return *val != 0 ? PolicyState::Allowed : PolicyState::Blocked;
    }

    // ── Public API ──────────────────────────────────────────────────────

    // (Re-)read all agent GPO values from the registry and cache them.
    // Called once at startup and again on settings reload.
    inline void Reload()
    {
        auto snap = std::make_shared<PolicySnapshot>();
        snap->allowedAgents = _ReadMultiSzPolicy(L"AllowedAgents");
        snap->customAgents = _DwordToPolicyState(_ReadDwordPolicy(L"AllowCustomAgents"));
        snap->autoFix = _DwordToPolicyState(_ReadDwordPolicy(L"AllowAutoFix"));
        snap->agentSessionHooks = _DwordToPolicyState(_ReadDwordPolicy(L"AllowAgentSessionHooks"));

        {
            std::lock_guard lock{ s_policyMutex };
            s_snapshot = std::move(snap);
        }
        s_loaded.store(true, std::memory_order_release);
    }

    // Return a thread-safe, immutable view of the cached policy.
    // No deep copy — callers share the same const snapshot.
    inline std::shared_ptr<const PolicySnapshot> _GetSnapshot()
    {
        if (!s_loaded.load(std::memory_order_acquire))
        {
            // Lazy-init: this DLL's cache was never populated. Read the
            // registry now so callers always get real policy data even if
            // nobody called Reload() in this DLL's context.
            Reload();
        }
        std::lock_guard lock{ s_policyMutex };
        return s_snapshot;
    }

    // Query cached policy. Thread-safe — returns data from the last Reload().
    inline bool IsAgentAllowed(std::wstring_view agentId)
    {
        const auto snap = _GetSnapshot();
        if (!snap->allowedAgents.has_value())
        {
            return true; // Not configured — all allowed
        }
        return snap->allowedAgents->find(agentId) != snap->allowedAgents->end();
    }

    inline bool IsCustomAgentAllowed()
    {
        return _GetSnapshot()->customAgents != PolicyState::Blocked;
    }

    inline bool IsAutoFixAllowed()
    {
        return _GetSnapshot()->autoFix != PolicyState::Blocked;
    }

    inline bool IsAgentSessionHooksAllowed()
    {
        return _GetSnapshot()->agentSessionHooks != PolicyState::Blocked;
    }

    // Expose raw policy state for UI to distinguish "not configured" from "allowed".
    inline PolicyState GetCustomAgentPolicy()
    {
        return _GetSnapshot()->customAgents;
    }

    inline PolicyState GetAutoFixPolicy()
    {
        return _GetSnapshot()->autoFix;
    }

    inline PolicyState GetAgentSessionHooksPolicy()
    {
        return _GetSnapshot()->agentSessionHooks;
    }

    // Whether AllowedAgents policy is configured at all (for UI lock indicators).
    inline bool IsAllowedAgentsPolicyConfigured()
    {
        return _GetSnapshot()->allowedAgents.has_value();
    }

    // Return the current snapshot (for advanced consumers).
    inline std::shared_ptr<const PolicySnapshot> GetSnapshot()
    {
        return _GetSnapshot();
    }
}
