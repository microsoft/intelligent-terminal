# Pane progress render evidence

`Feature.PaneProgress.Tests.ps1` has four deterministic release cases
(C368-C371) and a separate literal one-shot, idle-prompt case. The latter sends
`$ESC=[char]27;$BEL=[char]7;[Console]::Write("$ESC]9;4;3;0$BEL")`
once, without a child command or sleeping fixture. It requires the final
PowerShell prompt and pre-action progress before the exact right-click route.
The lifecycle case separately uses a gated, persistent shell fixture.

Render assertions capture the actual desktop compositor with
`System.Drawing.Graphics.CopyFromScreen`, scoped to the foreground owned window
and freshly queried UIA ring/icon rectangles. Six ring crops must differ;
nonuniform sampling delays avoid aliasing the one-second rotation.
`PrintWindow` is not a substitute: it can return stale XAML icon pixels while
the compositor already shows the correct Copilot glyph. The Default input
desktop and foreground ownership are prerequisites; their absence skips rather
than fabricates render proof. No raw terminal-buffer export is written, but full-window
PNG artifacts retain visible terminal text. Capture only fixture-owned windows
with non-sensitive contents, and review screenshots before sharing them.

Tab IDs are positional indices, not stable identities. After each menu move,
the suite verifies the original pane session ID at the expected active index
and refreshes the index before querying its two panes. Exact current parent-tab
text, rather than a prefix or child-pane match, selects the right-click target.

Run through `Invoke-ItE2EReport.ps1` with explicit `ITE2E_PACKAGE=Dev`,
`ITE2E_EXPECTED_APP_SHA256`, `ITE2E_EXPECTED_WTA_SHA256`, and a unique
`ITE2E_ARTIFACT_ROOT`. Obtain hashes from the intended source build receipt,
not arbitrary installed binaries. The selected package must be inactive.
Settings/state are backed up and restored by ItE2E. Case tags
`PaneProgressLiteral`, `PaneProgressLifecycle`, `PaneProgressMenu`,
`PaneProgressNative`, and `PaneProgressGroup` allow independent runs through
the same report driver. C371 checks that expanded/collapsed groups use only the
chevron in the normal icon slot and align their top-level titles with singleton
tabs, while pane identity remains independent.

C370 uses real packaged native-hook transport and a fixture-owned Copilot
session, not model output or a real Copilot invocation. Working/Ended snapshots,
scoped hook events, exit status and profile-icon rasters are retained. A real
authenticated `copilot -p` smoke is separate evidence and must not be inferred
from this fixture. Neither UIA nor `pane-status` exposes raw Core taskbar state;
if progress is absent before moving, record that limitation and do not attribute
it to rendering or replace the input with the persistent fixture.

The shared indeterminate view owns its animation clock. It starts only while
active, loaded, and visible through its attached visual ancestry, stops on
inactivity, determinate presentation, ancestor collapse, or unload, and observes
the new ancestry after reattachment. No progress-model lifecycle state, timer,
or per-frame callback is involved. `IndeterminateProgressStopsHiddenClocks`
checks compositor animation attachment, including old-parent isolation; compositor
frame variation alone does not establish that hidden clocks have stopped.
`RenderTargetBitmap` is not an oracle for compositor transforms; unit lifecycle
checks inspect the controller's presence, while live rendered-motion checks use desktop crops.
