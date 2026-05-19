// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Fuzzing harness for wtcli CLI utility functions.
// Targets: TranslateKeys, BuildSendEventJson, MatchesEventFilter.
//
// Built under the Fuzzing MSBuild configuration with LibFuzzer
// instrumentation; submittable to OneFuzz via the CI pipeline.

#include "precomp.h"
#include "wtcli_functions.h"

// Split fuzz input into N segments using null bytes as delimiters.
// If fewer than N null bytes exist, remaining segments are empty.
static std::vector<std::string> SplitInput(const uint8_t* data, size_t size, size_t n)
{
    std::vector<std::string> segments(n);
    size_t seg = 0;
    size_t start = 0;
    for (size_t i = 0; i < size && seg < n - 1; ++i)
    {
        if (data[i] == '\0')
        {
            segments[seg].assign(reinterpret_cast<const char*>(data + start), i - start);
            start = i + 1;
            ++seg;
        }
    }
    // Remainder goes into the last segment.
    if (start < size)
    {
        segments[seg].assign(reinterpret_cast<const char*>(data + start), size - start);
    }
    return segments;
}

// Core fuzzing logic — called by both LibFuzzer and the manual main().
static int FuzzOneInput(const uint8_t* data, size_t size)
{
    if (size == 0)
    {
        return 0;
    }

    // Split the entire input into segments for use across all targets.
    // We need at least 5 segments: [keys...] [eventType] [paramsJson]
    // [paneId] [eventTypeFilter]
    auto parts = SplitInput(data, size, 5);

    // ── Target 1: TranslateKeys ──
    // Split the first segment on tab bytes to create multiple key entries,
    // exercising the key-name matching and multi-key path.
    {
        std::vector<std::string> keys;
        const auto& keyData = parts[0];
        size_t start = 0;
        for (size_t i = 0; i < keyData.size(); ++i)
        {
            if (keyData[i] == '\t')
            {
                keys.emplace_back(keyData.data() + start, i - start);
                start = i + 1;
            }
        }
        if (start < keyData.size())
        {
            keys.emplace_back(keyData.data() + start, keyData.size() - start);
        }
        if (keys.empty())
        {
            keys.push_back(keyData);
        }

        wtcli::TranslateKeys(keys);
    }

    // ── Target 2: BuildSendEventJson ──
    // Fuzz all three input parameters: eventType, paramsJson, and paneId.
    {
        Json::Value evt;
        wtcli::BuildSendEventJson(parts[1], parts[2], parts[3], evt);
    }

    // ── Target 3: MatchesEventFilter ──
    // Construct semi-valid JSON from fuzzed fields so the parser succeeds
    // and the deep matching logic (pane_id, wildcard) is actually reached.
    {
        // 3a: Fuzzed event structure with fuzzed filter.
        const auto& fuzzedPaneId = parts[3];
        const auto& fuzzedEvent = parts[1];
        const auto& fuzzedFilter = parts[4];

        std::string syntheticJson =
            R"({"params":{"pane_id":")" + fuzzedPaneId +
            R"(","event":")" + fuzzedEvent +
            R"("}})";

        wtcli::MatchesEventFilter(syntheticJson, fuzzedPaneId, fuzzedFilter);

        // 3b: Mismatched pane_id — exercises the rejection path.
        wtcli::MatchesEventFilter(syntheticJson, "999", fuzzedFilter);

        // 3c: Raw fuzzed bytes as event JSON — exercises parse-failure path.
        const std::string raw(reinterpret_cast<const char*>(data), size);
        wtcli::MatchesEventFilter(raw, fuzzedPaneId, fuzzedFilter);
    }

    return 0;
}

#ifdef FUZZING_BUILD
extern "C" __declspec(dllexport) int LLVMFuzzerInitialize(int* /*argc*/, char*** /*argv*/)
{
    return 0;
}
#else
int main(int argc, char** argv)
{
    if (argc < 2)
    {
        fprintf(stderr, "Usage: WtcliFuzzer <input-file>\n");
        return 1;
    }
    std::ifstream file(argv[1], std::ios::binary);
    std::string data((std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
    return FuzzOneInput(reinterpret_cast<const uint8_t*>(data.data()), data.size());
}
#endif

extern "C" __declspec(dllexport) int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size)
{
    return FuzzOneInput(data, size);
}
