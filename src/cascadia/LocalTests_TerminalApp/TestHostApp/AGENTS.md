# TestHostApp

One packaged Windows app hosts TerminalApp local tests and real production UI
for isolated accessibility checks. It does not replace full-app integration tests.

## Launch modes

| Argument | Purpose |
| --- | --- |
| No accessibility argument | Default test-client path for TAEF local tests. |
| `--accessibility-page=fre` | Real FRE overlay, initialized with default settings. |
| `--accessibility-page=agents` | Real Agents page reached through Frame navigation with the production view model and navigation arguments. |

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
