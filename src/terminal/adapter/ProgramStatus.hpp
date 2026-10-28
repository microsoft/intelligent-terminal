// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <cstddef>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

namespace Microsoft::Console::VirtualTerminal::ProgramStatus
{
    inline constexpr size_t MaximumBodyLength = 4087;
    inline constexpr size_t MaximumRecords = 256;
    inline constexpr std::wstring_view CapabilityResponse = L"7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth";

    enum class State
    {
        Idle,
        Working,
        Blocked,
        Done,
        Error,
    };

    enum class BlockKind
    {
        Permission,
        Question,
        Auth,
    };

    struct Record
    {
        std::string id; // Empty is the root; '/' separates optional child segments.
        State state = State::Idle;
        std::optional<BlockKind> kind;
        std::optional<uint32_t> progress;
        std::optional<std::string> app;
        std::optional<std::wstring> title;
        std::optional<std::wstring> message;
        // Computed when taking a snapshot, never copied into raw child records.
        std::optional<std::string> effectiveApp;
        uint64_t revision = 0;
    };

    struct Result
    {
        Record record;
        uint64_t revision = 0;
        bool unread = true;
    };

    struct Snapshot
    {
        // First valid report latches this until RIS or connection replacement.
        bool observed = false;
        // Monotonic even across reset, so an old acknowledgment cannot see a new result.
        uint64_t revision = 0;
        std::vector<Record> records;
        // Latest Done/Error per id, including results superseded by live activity.
        std::vector<Result> results;
    };

    struct Command
    {
        enum class Operation
        {
            Query,
            Replace,
            Clear,
        };

        Operation operation = Operation::Query;
        Record record;
    };

    [[nodiscard]] std::optional<Command> Parse(std::wstring_view body);

    // The owner serializes access in terminal stream order. None of these
    // methods calls UI code, performs I/O, or depends on the terminal buffer.
    class Store
    {
    public:
        [[nodiscard]] Snapshot GetSnapshot() const;
        [[nodiscard]] bool Observed() const noexcept { return _observed; }
        bool Apply(const Command& command);
        bool ProcessExited();
        bool Acknowledge(uint64_t observedRevision);
        bool Reset();

    private:
        struct Entry
        {
            Record record;
            uint64_t updated = 0;
        };

        std::optional<std::string> _EffectiveApp(std::string_view id) const;
        std::vector<Entry> _records;
        std::vector<Result> _results;
        uint64_t _revision = 0;
        uint64_t _clock = 0;
        bool _observed = false;
    };
}
