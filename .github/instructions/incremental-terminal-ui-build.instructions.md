---
description: 'Correct incremental MSBuild and payload freshness for TerminalApp UI changes'
applyTo: 'src/cascadia/TerminalApp/**/*.cpp, src/cascadia/TerminalApp/**/*.h, src/cascadia/TerminalApp/**/*.idl, src/cascadia/TerminalApp/**/*.xaml, src/cascadia/TerminalApp/**/*.vcxproj, src/cascadia/LocalTests_TerminalApp/**/*.cpp, src/cascadia/LocalTests_TerminalApp/**/*.vcxproj, src/cascadia/CascadiaPackage/**/*.wapproj, src/cascadia/CascadiaPackage/**/*.appxmanifest'
---

# Incremental Terminal UI builds

Use the existing MSBuild dependency graph. Optimize the changed project, not
the solution, and preserve coherent generated code, resources, and runtime
payloads. Follow [AGENTS.md](../../AGENTS.md) and
[the contributor build guide](../../doc/building.md) for environment setup and
general build/deployment commands; this file covers UI-specific correctness.

## Select the smallest correct build

- Pin the configuration, architecture, branding, and active `SolutionDir`.
  Do not reuse another worktree's output or mix branding outputs.
- Use `/t:Build`, not `/t:Rebuild` or a clean solution build, for ordinary
  iteration. Preserve dependency tracking and PCH reuse.
- Set `BuildProjectReferences=false` only after the referenced projects'
  generated headers, libraries, metadata, and resources exist and are up to date
  for the pinned configuration, architecture, branding, and referenced sources
  in this worktree. Build a missing or stale dependency rather than copying
  another tree's output.
- For a `.cpp`-only TerminalApp change, build AppLib, then link App DLL.
  Reuse unchanged WTA output; do not rebuild Rust for C++-only changes.
- For IDL changes, allow MIDL, metadata merging, cppwinrt generation, and all
  affected native compilation. Do not constrain the build to one source file.
- For XAML changes, regenerate bindings, compiled XAML, and resource indexes.
  Copying source XAML is not an equivalent build.
- Do not use bare `ClCompile;Link` as an AppLib pipeline. Do not adopt
  selected-file or packaging-target shortcuts without measuring them and
  verifying complete outputs.

### Warmed Debug x64 Dev example

Replace `<worktree>` with the active worktree's absolute path:

```cmd
cd /d "<worktree>"
call tools\razzle.cmd
set "P=/p:Configuration=Debug /p:Platform=x64 /p:WindowsTerminalBranding=Dev /p:BuildProjectReferences=false /v:minimal /nologo"
msbuild src\cascadia\TerminalApp\TerminalAppLib.vcxproj /t:Build %P% "/p:SolutionDir=<worktree>\\" && msbuild src\cascadia\TerminalApp\dll\TerminalApp.vcxproj /t:Build %P% "/p:SolutionDir=<worktree>\\"
```

Run these commands in one CMD session. Record the actual compiled files,
elapsed time, exit code, and output hashes; do not claim an unmeasured speedup.
Use the explicit worktree path when combining `cd` and a build in `cmd /c`;
`%CD%` expands before `cd` executes in a compound command.

## Focused native tests

- Build `TerminalApp.LocalTests.vcxproj` incrementally, then perform a full
  incremental `TestHostApp.vcxproj` Build to regenerate its aggregate resources
  and deployment recipe. `AfterBuild` alone is not a freshness guarantee.
- If dependency-free LocalTests reports missing source-tree
  `Generated Files\*.xaml` during copying, `/p:AppxPackage=true` disables the
  legacy loose-XAML copy path. This does not create missing referenced PRIs;
  keep that override local to the LocalTests command. Leave TestHost's
  `AppxPackage` property at its project default; propagating a global override
  changes referenced projects' expected PRI names. Build genuinely missing
  dependencies before generating the TestHost payload.
- Do not globally override `GeneratedFilesDir` or `XamlGeneratedOutputPath`.
  Such overrides propagate into unrelated referenced code-generation projects.
- Run TerminalApp native tests through TAEF from the TestHost output directory.
  Select the changed regression first, not the entire suite.

## Payload freshness

- Verify that recipes reference the active worktree, not a canonical or older
  worktree with the same project names.
- Match TestHost App DLL, WinMD, LocalTests DLL, generated XAML, and component
  resource inputs to the corresponding fresh project outputs.
- Preserve the TestHost's aggregate `resources.pri`; do not substitute a
  component PRI or an application package's aggregate PRI.
- For PRI duplicate-entry failures, inspect the actual merge list and resource
  values. Do not suppress the conflict or assume identical XBF bytes prove
  equivalent resource indexes.
- Build the application package for product UI verification after C++, IDL,
  XAML, or resource changes. A compiler result or TestHost result is not a
  deployed application result.
- Keep previous registered layouts immutable. Preserve current user settings;
  never uninstall Dev to work around deployment.
