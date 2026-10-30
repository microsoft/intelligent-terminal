# TestHostApp

One packaged Windows app hosts TerminalApp local tests and real production UI
for isolated accessibility checks. It does not replace full-app integration tests.

## Launch modes

| Argument | Purpose |
| --- | --- |
| No accessibility argument | Default test-client path for TAEF local tests. |
| `--accessibility-page=fre` | Real FRE overlay, initialized with default settings. |
| `--accessibility-page=agents` | Real Agents page reached through Frame navigation with the production view model and navigation arguments. |
| `--visual-page=sidebar` | Real sidebar controls with deterministic tab, pane-group, and history data; named theme presets, active/inactive palette, RTL, and width controls. |

The harness surface `fre-settings` launches the FRE mode, invokes Next through
UI Automation, and scans the resulting settings controls. It is not a separate
app or launch argument. Agents mode activates the host window before creating
the production view model and navigates its Frame with `NavigateToPageArgs`.
This exercises the page's normal `OnNavigatedTo` initialization, not a
TestHost-only Agents constructor. No native window root is supplied because
this page's initialization does not require one.

Do not construct the full Settings MainPage in this AppContainer host.
MainPage eagerly loads Extensions; its filesystem discovery fails with access
denied in AppContainer and reaches the debug coroutine assertion. This was
confirmed with an owned-process debugger, not inferred from activation errors.
The fixture does not establish full Settings navigation, native-window integration
or persistence coverage.

## Build and test

Build from the repository root:

```powershell
cmd.exe /c "tools\razzle.cmd && cd src\cascadia\LocalTests_TerminalApp\TestHostApp && bx"
```

For TAEF, also build `TerminalApp.LocalTests.dll`; `runut` in a razzle environment
runs it from the TestHostApp output, not the LocalTests output.

For native scans, use `test\accessibility\Invoke-AxeWindowsTestHost.ps1` with
`-Surface` set to `fre`, `fre-settings`, or `agents`. Supply the built manifest,
Axe.Windows path, output directory, and exact source SHA. An interactive Windows
desktop and the app's framework dependencies are required. Setup and artifact
limitations are documented in `doc\ghaw\ghaw-pr-accessibility.md`.

## Extending coverage

Reuse production pages, view models, and resources; do not duplicate UI in mocks.
Add an explicit launch route in `UnitTestApp.xaml.cpp` and matching readiness/
UIA assertions in the scan harness. Preserve the default TAEF launch path.
Use the test package, not deployment over the user's Intelligent Terminal.

These scans cover selected states, not all Settings pages, terminal-window
integration, complete keyboard journeys, Narrator, contrast, or scaling.

## Sidebar visual iteration

The sidebar route loads the packaged production `TerminalApp\App.xaml` resource
dictionary and instantiates production controls through `TerminalApp.LocalTests.dll`.
Its TAEF runtime dependencies are packaged explicitly because this route runs
without TAEF provisioning. It does not start Terminal, WTA, shells, or agents.
Fixture labels and paths are synthetic; do not substitute real session data.

The four named presets come from `CascadiaSettings::LoadDefaults()`, not copied
color tables. Production `Tab::ThemeColor` applies their tab colors against a
synthetic black content background; legacy presets retain the native/default
tab brushes. The active/inactive palette switch exercises the preset's tab-row
colors with acrylic disabled. This is controlled palette coverage, not proof of
native window activation, acrylic, or every profile-background combination.

Build LocalTests and the full incremental TestHost payload after control changes,
then inspect the registered test package with `winapp ui inspect`, `get-property`,
`hover`, and `screenshot`. For parallel worktrees, give the TestHost manifest a
temporary private identity before building; do not replace another checkout's
registered layout. Keep these identity/version overrides out of commits.

UWP may host the window in `ApplicationFrameHost`, whose PID differs from
`TestHostApp.exe`. Resolve its HWND and verify that its UIA descendants contain
the newly activated TestHost PID before targeting it with `winapp -w`. Never
terminate the shared frame-host process.

Compare matching tab/search/hover states across themes, widths, and directions.
Retain raw captures and source/payload identity before adding annotations. A
passing TestHost inspection is control-level evidence, not proof of full Terminal
theme propagation, titlebar composition, session routing, or persistence; follow
it with focused inspection of the isolated application package.
