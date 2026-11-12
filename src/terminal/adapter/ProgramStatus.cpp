// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "ProgramStatus.hpp"

#include <algorithm>

using namespace Microsoft::Console::VirtualTerminal::ProgramStatus;

namespace
{
    bool sameRecord(const Record& a, const Record& b) noexcept
    {
        return a.id == b.id && a.state == b.state && a.kind == b.kind && a.progress == b.progress &&
               a.app == b.app && a.title == b.title && a.message == b.message;
    }

    bool isResult(const State state) noexcept
    {
        return state == State::Done || state == State::Error;
    }

    bool inSubtree(const std::string_view id, const std::string_view root) noexcept
    {
        return root.empty() || id == root || (id.size() > root.size() && id.substr(0, root.size()) == root && id[root.size()] == '/');
    }
}

bool Store::Apply(const Command& command)
{
    if (command.operation == Command::Operation::Query)
    {
        return false;
    }
    bool changed = !_observed;
    _observed = true;
    if (command.operation == Command::Operation::Clear)
    {
        changed |= std::erase_if(_records, [&](const auto& entry) { return inSubtree(entry.record.id, command.record.id); }) != 0;
        changed |= std::erase_if(_results, [&](const auto& result) { return inSubtree(result.record.id, command.record.id); }) != 0;
        if (changed)
        {
            ++_revision;
        }
        return changed;
    }

    auto entry = std::find_if(_records.begin(), _records.end(), [&](const auto& item) { return item.record.id == command.record.id; });
    const auto newResult = isResult(command.record.state) &&
                           (entry == _records.end() || entry->record.state != command.record.state ||
                            entry->record.title != command.record.title || entry->record.message != command.record.message);
    if (entry != _records.end())
    {
        entry->updated = ++_clock;
        changed |= !sameRecord(entry->record, command.record);
        if (!changed)
        {
            return false;
        }
    }
    else
    {
        if (_records.size() == MaximumRecords)
        {
            entry = std::min_element(_records.begin(), _records.end(), [](const auto& a, const auto& b) {
                return a.updated == b.updated ? a.record.id < b.record.id : a.updated < b.updated;
            });
            std::erase_if(_results, [&](const auto& result) { return result.record.id == entry->record.id; });
        }
        else
        {
            _records.emplace_back();
            entry = _records.end() - 1;
        }
        entry->updated = ++_clock;
    }
    entry->record = command.record;
    entry->record.effectiveApp.reset();
    entry->record.revision = ++_revision;
    if (isResult(entry->record.state))
    {
        auto result = std::find_if(_results.begin(), _results.end(), [&](const auto& item) { return item.record.id == entry->record.id; });
        if (newResult)
        {
            if (result == _results.end())
            {
                if (_results.size() == MaximumRecords)
                {
                    result = std::min_element(_results.begin(), _results.end(), [](const auto& a, const auto& b) {
                        return a.revision == b.revision ? a.record.id < b.record.id : a.revision < b.revision;
                    });
                }
                else
                {
                    _results.emplace_back();
                    result = _results.end() - 1;
                }
            }
            *result = Result{ entry->record, _revision, true };
        }
        else if (result != _results.end())
        {
            result->record = entry->record;
        }
    }
    return true;
}

bool Store::ProcessExited()
{
    if (std::erase_if(_records, [](const auto& entry) { return !isResult(entry.record.state); }))
    {
        ++_revision;
        return true;
    }
    return false;
}

bool Store::Acknowledge(const uint64_t observedRevision)
{
    bool changed = false;
    for (auto& result : _results)
    {
        if (result.unread && result.revision <= observedRevision)
        {
            result.unread = false;
            changed = true;
        }
    }
    if (changed)
    {
        ++_revision;
    }
    return changed;
}

bool Store::Reset()
{
    if (!_observed && _records.empty() && _results.empty())
    {
        return false;
    }
    _observed = false;
    _records.clear();
    _results.clear();
    // Revisions deliberately survive RIS/connection replacement, so a queued
    // acknowledgment can never mark a newer connection's result seen.
    ++_revision;
    return true;
}

std::optional<std::string> Store::_EffectiveApp(std::string_view id) const
{
    for (;;)
    {
        const auto entry = std::find_if(_records.begin(), _records.end(), [&](const auto& item) { return item.record.id == id; });
        if (entry != _records.end() && entry->record.app)
        {
            return entry->record.app;
        }
        if (id.empty())
        {
            return std::nullopt;
        }
        const auto slash = id.rfind('/');
        id = slash == std::string_view::npos ? std::string_view{} : id.substr(0, slash);
    }
}

Snapshot Store::GetSnapshot() const
{
    Snapshot snapshot{ _observed, _revision, {}, _results };
    snapshot.records.reserve(_records.size());
    for (const auto& entry : _records)
    {
        auto& record = snapshot.records.emplace_back(entry.record);
        record.effectiveApp = _EffectiveApp(record.id);
    }
    for (auto& result : snapshot.results)
    {
        result.record.effectiveApp = result.record.app ? result.record.app : _EffectiveApp(result.record.id);
    }
    const auto order = [](const auto& a, const auto& b) { return a.id < b.id; };
    std::sort(snapshot.records.begin(), snapshot.records.end(), order);
    std::sort(snapshot.results.begin(), snapshot.results.end(), [&](const auto& a, const auto& b) { return order(a.record, b.record); });
    return snapshot;
}
