// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Fuzzing harness for wtcli pure functions.
// Targets: BuildSendEventJson, MatchesEventFilter.
//
// Built under the Fuzzing MSBuild configuration with LibFuzzer
// instrumentation; submittable to OneFuzz via the CI pipeline.

#include "precomp.h"
#include "wtcli_functions.h"

// Core fuzzing logic — called by both LibFuzzer and the manual main().
static int FuzzOneInput(const uint8_t* data, size_t size)
{
    if (size == 0)
    {
        return 0;
    }

    const std::string input(reinterpret_cast<const char*>(data), size);

    // ── Target 1: BuildSendEventJson ──
    // Feed fuzzed data as the paramsJson argument to exercise JSON parsing
    // and envelope construction.
    {
        Json::Value evt;
        wtcli::BuildSendEventJson("test.event", input, "42", evt);
    }

    // ── Target 2: MatchesEventFilter ──
    // Exercise both directions: fuzzed data as the event JSON (with fixed
    // filters) and fuzzed data as the filter pattern (with valid event JSON).
    {
        wtcli::MatchesEventFilter(input, "42", "agent.*");

        static const std::string validEvent =
            R"({"params":{"session_id":"1","event":"agent.task.started"}})";
        wtcli::MatchesEventFilter(validEvent, "", input);
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
