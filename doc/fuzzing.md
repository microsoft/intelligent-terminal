# Fuzzing

Intelligent Terminal uses [LibFuzzer](https://www.llvm.org/docs/LibFuzzer.html) with
Address Sanitizer (ASan) to fuzz-test parsing and input-handling code. Fuzzing
jobs run automatically in CI via the `fuzz.yml` pipeline and are submitted to
OneFuzz for continuous coverage.

## Fuzzer projects

| Project | Path | Targets |
|---------|------|---------|
| **WtcliFuzzer** | `src/tools/wtcli/ft_fuzzer/` | `BuildSendEventJson`, `MatchesEventFilter` |
| **ProtocolFuzzer** | `src/cascadia/TerminalProtocol/ft_fuzzer/` | `ClassifySendEvent`, `ParseSplitDirection`, `ClassifyPaneOutputSource` |
| **OpenConsoleFuzzer** | `src/host/ft_fuzzer/` | `WriteCharsLegacy` (original host fuzzer) |

Each project contains:
- `fuzzmain.cpp` — fuzzing harness with `LLVMFuzzerTestOneInput` entry point
- `*-OneFuzzConfig.json` — OneFuzz submission configuration
- `*.vcxproj` — MSBuild project with `Fuzzing` configuration support

## Building locally

Fuzzers are built using the **Fuzzing** MSBuild configuration, which enables
ASan, LibFuzzer instrumentation, and static CRT linkage.

### Prerequisites

- Visual Studio 2022 (17.10+) with C++ desktop workload
- Windows SDK 10.0.22621.0 or later

### Build steps

From a razzle environment (or directly with MSBuild):

```cmd
rem One-time setup
tools\razzle.cmd

rem Build all fuzzers (Fuzzing configuration)
bcz Fuzzing

rem Or build individual fuzzers
cd src\tools\wtcli\ft_fuzzer
bcx Fuzzing

cd src\cascadia\TerminalProtocol\ft_fuzzer
bcx Fuzzing
```

Output lands in `bin\x64\Fuzzing\`:
- `WtcliFuzzer.exe`, `.pdb`, `.lib`
- `ProtocolFuzzer.exe`, `.pdb`, `.lib`
- `clang_rt.asan_dynamic-x86_64.dll` (copied by PostBuildEvent)
- `WtcliFuzzer-OneFuzzConfig.json`, `ProtocolFuzzer-OneFuzzConfig.json`

### Running a fuzzer locally

```cmd
rem Run with no arguments to start fuzzing from scratch
bin\x64\Fuzzing\WtcliFuzzer.exe

rem Run with a corpus directory
bin\x64\Fuzzing\WtcliFuzzer.exe corpus\wtcli\

rem Run a single test case (useful for repro)
bin\x64\Fuzzing\WtcliFuzzer.exe crash-input-file
```

Each fuzzer also has a manual `main()` that accepts file paths as arguments,
so you can run individual test cases without the LibFuzzer driver in a
Debug build.

## Adding a new fuzzer

1. **Create the project folder** under the target module as `ft_fuzzer/`.

2. **Create `fuzzmain.cpp`** with an `LLVMFuzzerTestOneInput` entry point:
   ```cpp
   extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size)
   {
       // Call your target functions with the fuzz input
       return 0;
   }
   ```

3. **Create a `.vcxproj`** following the pattern in the existing fuzzers.
   Key requirements in the `Fuzzing` configuration:
   - `<EnableASAN>true</EnableASAN>` and `<EnableFuzzer>true</EnableFuzzer>`
   - Preprocessor defines: `_DISABLE_VECTOR_ANNOTATION;_DISABLE_STRING_ANNOTATION`
   - Link `clang_rt.fuzzer_MT-$(OCClangArchitectureName).lib`
   - PostBuildEvent to copy `clang_rt.asan_dynamic-x86_64.dll` to `$(OutDir)`

   The repo's `common.build.pre.props` already provides ASan flags, coverage
   instrumentation, and `FUZZING_BUILD` define for the `Fuzzing` configuration.

4. **Create an OneFuzzConfig JSON** (use a unique name like
   `<ProjectName>-OneFuzzConfig.json` to avoid collisions in the shared output
   directory):
   ```json
   {
     "configVersion": 3,
     "entries": [
       {
         "Fuzzer": {
           "$type": "libfuzzer",
           "FuzzingHarnessExecutableName": "YourFuzzer.exe",
           "FuzzingTargetBinaries": ["YourFuzzer.exe"],
           "FuzzingEntrypoint": "LLVMFuzzerTestOneInput"
         },
         "adoTemplate": {
           "org": "microsoft",
           "project": "Dart",
           "AssignedTo": "your-alias@microsoft.com",
           "AreaPath": "OS\\Windows Client and Services\\WinPD\\DFX-Developer Fundamentals and Experiences\\DEFT\\SALT",
           "IterationPath": "OS\\Future"
         },
         "jobNotificationEmail": "your-alias@microsoft.com",
         "skip": false,
         "rebootAfterSetup": false,
         "oneFuzzJobs": [
           {
             "projectName": "IntelligentTerminal.YourModule",
             "targetName": "YourModule_Fuzzer"
           }
         ],
         "jobDependencies": [
           "YourFuzzer.exe",
           "YourFuzzer.pdb",
           "YourFuzzer.lib",
           "clang_rt.asan_dynamic-x86_64.dll"
         ],
         "SdlWorkItemId": 62007365,
         "SdlWorkItemProjectUrl": "https://dev.azure.com/microsoft/OS"
       }
     ]
   }
   ```

5. **Add a `CopyFileToFolders`** item in the vcxproj for your config:
   ```xml
   <ItemGroup>
     <CopyFileToFolders Include="YourFuzzer-OneFuzzConfig.json" />
   </ItemGroup>
   ```

6. **Register in the solution** — add the new vcxproj to `OpenConsole.slnx`
   under the appropriate solution folder.

7. **Add a submission step** in `build/pipelines/fuzz.yml` if the pipeline
   does not auto-discover OneFuzzConfig files:
   ```yaml
   - bash: |
       onefuzz template libfuzzer basic ... OpenConsole $test_name ...
     displayName: Submit OneFuzz Job — YourFuzzer
     env:
       target_exe_path: $(Build.ArtifactStagingDirectory)/YourFuzzer.exe
       test_name: YourTargetName
   ```

## CI pipeline

The fuzzing pipeline is defined in `build/pipelines/fuzz.yml`:

1. **Build stage** — builds all projects with `Configuration=Fuzzing` on x64
2. **Submit stage** — downloads build artifacts and submits each fuzzer to
   OneFuzz with the configured notification and ADO work item settings

The pipeline triggers on pushes to `main` (excluding `docs/`, `samples/`,
`tools/`).

## OneFuzz configuration

Each fuzzer's OneFuzzConfig JSON controls:

| Field | Purpose |
|-------|---------|
| `FuzzingHarnessExecutableName` | The fuzzer executable name |
| `FuzzingTargetBinaries` | Binaries being fuzz-tested (for claims) |
| `FuzzingEntrypoint` | Entry function (`LLVMFuzzerTestOneInput`) |
| `SdlWorkItemId` | SDL task ID for linking bugs and claims |
| `SdlWorkItemProjectUrl` | ADO project URL for the SDL work item |
| `adoTemplate` | Where bugs are filed (org, project, area path) |
| `jobNotificationEmail` | Email for job completion notifications |
| `jobDependencies` | Files to include alongside the fuzzer |

## Resources

- [LibFuzzer documentation](https://www.llvm.org/docs/LibFuzzer.html)
- [OneFuzz GitHub](https://github.com/microsoft/onefuzz)
- [OSG Wiki — Fuzzing Service](https://www.osgwiki.com/wiki/Fuzzing_Service_-_Azure_Edge_and_Platform)
- [Address Sanitizer (MSVC)](https://learn.microsoft.com/en-us/cpp/sanitizers/asan)