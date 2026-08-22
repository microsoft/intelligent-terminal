---
author: Kaitao @vanzue
created on: 2026-08-04
last updated: 2026-08-22
issue id: 2671, 8849, 5916, 6969
---

# Clickable File Paths and the Clickable Matcher Foundation

## Abstract

Intelligent Terminal automatically recognizes local Windows file paths in
terminal output and presents them through the existing hyperlink experience.
Hovering a recognized path marks it as clickable, and activating it opens the
file with the Windows-associated application.

This feature is intentionally narrower than the generic "Triggers and Custom
Clickable Links" design proposed in [PR #15700]. The current implementation
first solves the concrete file-linking scenario from [#2671], while introducing
an internal `ClickableMatcher` and `ClickAction` foundation that can evolve
toward configurable clickable links and, later, generic triggers.

This document records:

- the user problem and related upstream issues;
- the behavior and architecture implemented today;
- security, correctness, and performance decisions;
- the relationship to the earlier trigger spec; and
- the roadmap from built-in file detection to configurable matchers and
  triggers.

## Status

| Area | Status |
|---|---|
| Built-in URL matcher | Implemented |
| Built-in local Windows file matcher | Implemented |
| Historical working-directory binding | Implemented |
| File existence and local-path validation | Implemented |
| Matcher result cache and activation-time revalidation | Implemented |
| Line/column file locations | Implemented |
| System-default or `$EDITOR` activation | Implemented |
| Historical shell/path translation context | Implemented |
| Hover-time bare directories and extensionless files | Implemented |
| Bounded asynchronous hover validation | Implemented |
| User-defined regex matchers | Planned |
| Capture substitution and editor selection | Planned |
| Generic trigger actions | Future |

The implementation is currently a prototype on the Intelligent Terminal branch.
It is not an implementation of the full trigger design.

## Motivation and user stories

Terminal output frequently contains file references:

```text
README.md
src\cascadia\TerminalCore\Terminal.cpp
C:\work\project\build.log
"release notes.txt"
```

The user should not need to copy a path, determine the directory in which it
was printed, switch applications, and paste it into an open dialog. A detected
path should behave like an automatically detected URL.

The initial user stories are:

| ID | User story | Status |
|---|---|---|
| F1 | A local file name printed by a shell can be activated with the mouse or keyboard. | Implemented |
| F2 | A relative path resolves against the working directory active when that row was printed. | Implemented |
| F3 | A missing file or unrelated dotted token is not presented as a link. | Implemented |
| F4 | An absolute Windows path can be opened with its associated application. | Implemented |
| F5 | A quoted path may contain spaces, while the quotes remain outside the clickable range. | Implemented |
| F6 | URL detection takes priority when a file-like substring appears inside a URL. | Implemented |
| F7 | A user can independently enable or disable URL and file detection. | Planned |
| F8 | A user can choose the system default app or `$EDITOR` and open `path:line:column`. | Implemented |
| F9 | A user can define a regex and map capture groups into a clickable target. | Planned |
| F10 | A terminal-control consumer can provide matchers and click handlers. | Future |
| F11 | Bare directory names, extensionless files, and unquoted paths with spaces become clickable only after a bounded hover-time existence check. | Implemented |

## Related issues

This design directly or incrementally addresses the following upstream
requests:

| Issue | Request | Relationship to this design |
|---|---|---|
| [#2671] | Link generation for files and other data types | The built-in file matcher directly addresses the local-file portion. |
| [#8395] | Open disk files and folders in their default application | Files and hover-confirmed bare directories are implemented. |
| [#8978] | Clickable file paths | Directly addressed for supported local Windows file forms. |
| [#9793] | Clickable disk paths | Files and hover-confirmed folders are implemented. |
| [#17612] | Clickable file paths | Directly addressed for the supported grammar. |
| [#18391] | Clickable directory paths | Addressed through existence-checked hover-time detection rather than viewport-wide word matching. |
| [#8849] | Arbitrary regex hyperlink matching and custom handlers | The internal matcher/action split is foundational work. Public regex configuration and custom handlers remain planned. |
| [#5916] | Triggers and actions over terminal text | Only the passive clickable-matcher subset is addressed. Active triggers are future work. |
| [#6969] | Consumer-provided recognizers and click handlers | The internal types prepare for this, but no public `TerminalControl` interface exists yet. |
| [#8294] | Configure normal and hover appearance of detected links | Not addressed. The feature reuses existing hyperlink rendering. |
| [#7562] | Additional URI schemes | Not expanded by this work. Existing URI safety policy remains authoritative. |
| [#11901] | Clickable IPv6 links | Not addressed by the file matcher. A future matcher framework could host a corrected URL matcher. |

The related [PR #15700] was closed without merging. Its draft remains useful as
the architectural direction for configurable matchers and triggers, but this
document does not treat it as an accepted or active specification.

## Prior art

Warp provides the closest product model for the initial feature:

- file-path recognition is built in rather than expressed as a user-authored
  regular expression;
- relative and absolute paths become clickable;
- the user can choose an editor for file links; and
- Warp supports common line and column suffixes.

The current Intelligent Terminal implementation follows all four points, with
editor selection intentionally limited to the system default application or
the user's `$EDITOR` command.

OSC 8 remains the preferred mechanism when the producing application already
knows that text is a link. Automatic matching exists for unmodified tools and
plain output that do not emit OSC 8.

## Scope

### Supported today

- Bare file names with an extension, such as `README.md`.
- Nested relative paths, such as `src\main.cpp`.
- Explicit relative paths beginning with `.\` or `..\`.
- Drive-qualified absolute Windows paths.
- Single- or double-quoted paths containing spaces.
- `path:line`, `path:line:column`, `path(line,column)`, and
  `path[line,column]`.
- Python traceback entries (`File "path", line N, in ...`), GitHub-style
  `path#Lline[:column]`, and `path:start-end`.
- POSIX paths translated from historical WSL, Cygwin, MSYS2, or MinGW profile
  context.
- Paths split across wrapped terminal rows.
- Main and alternate screen buffers.
- Opening through the system-associated application or a directly launched
  `$EDITOR` command.
- Hover-confirmed bare directories such as `Documents` and `Saved Games`.
- Hover-confirmed extensionless files such as `README` and `LICENSE`.
- Hover-confirmed unquoted paths containing spaces, including
  `OneDrive - Microsoft`.

### Deliberate current exclusions

- SSH, container, and other remote path namespaces.
- Arbitrary UNC paths and remote drives. A `\\wsl$\<distro>` path is allowed
  only when TerminalCore generated it from a validated historical
  `wsl:<distro>` identity.
- Paths containing reparse-point components.
- User-authored regular expressions.
- Automatically executing commands in response to output.

These exclusions avoid guessing an execution environment or introducing an
automatic command-execution surface before configuration and security semantics
are designed.

## UI and interaction

The feature reuses the existing automatic hyperlink UX:

1. Matching text is rendered as an automatically detected clickable region.
2. Hover uses the existing hyperlink underline and tooltip.
3. Mouse and keyboard activation use the existing hyperlink interaction.
4. The resolved `file://` target is sent through the existing
   `OpenHyperlink` event and application-level URI safety handling.
5. Windows opens the file with its associated application.

OSC 8 links embedded in cell attributes remain authoritative. Automatic
matchers are considered only when the cell does not already contain a
hyperlink.

Bare-path fallback is hover-only. It does not add ordinary words to the
viewport regex scan or hyperlink interval tree. Tab navigation therefore
continues to enumerate persistent OSC 8 and regex links; an ephemeral hover
link is activatable while it is current but need not participate in Tab
navigation.

## Solution design

### Component model

```text
terminal output
    |
    v
TextBuffer rows
    |  row metadata: historical working-directory and shell IDs
    v
ClickableMatcher registry
    |-- built-in URL entry  ------> ClickAction::OpenUri
    `-- built-in file entry ------> ClickAction::OpenFile
              |
              v
       location parse + context-aware path resolution
              |
              v
       interval tree for hover/render/navigation
              |
              v
       activation-time resolution
              |
              v
       OpenHyperlink -> Explorer, system association, or direct $EDITOR launch
```

The implementation currently uses three internal types:

```cpp
enum class ClickAction
{
    OpenUri,
    OpenFile,
};

struct ClickableMatcher
{
    size_t id;
    std::wstring_view pattern;
    ClickAction action;
};

struct ClickableMatch
{
    size_t matcherId;
    ClickAction action;
    std::wstring target;
};
```

`ClickableMatcher` describes what to recognize and how its text should be
resolved. `ClickableMatch` is the resolved result. This replaces hard-coded
numeric pattern branches with an explicit matcher/action boundary.

This is intentionally an internal C++ model. It is not yet a WinRT API or a
settings schema.

### Matching lifecycle

`UpdatePatternsUnderLock` scans the visible viewport plus one viewport of
context above and below. Extra rows allow wrapped links crossing the viewport
boundary to be recognized as one interval.

For each matcher, TerminalCore:

1. executes the ICU regular expression over a `UText` view of the text buffer;
2. converts each regex result to a half-open buffer-coordinate interval;
3. resolves or validates the candidate according to its `ClickAction`;
4. rejects candidates that do not resolve; and
5. inserts accepted intervals into the pattern interval tree.

Matchers are processed in registration order, which currently defines their
priority. A later match overlapping an earlier accepted interval is discarded.
The URL matcher currently runs before the file matcher, preventing `README.md` inside
`https://example.test/?file=README.md` from replacing part of the URL.

The interval tree continues to drive rendering, hover, keyboard hyperlink
selection, and viewport-relative hyperlink queries.

### Hover-time bare-path fallback

When the hovered cell has neither an OSC 8 hyperlink nor a regex interval,
`ControlCore` asks TerminalCore for a bounded immutable snapshot. TerminalCore
walks only the nearby soft-wrapped logical row, centered on the hovered cell,
and records:

- candidate text and exact half-open buffer intervals;
- the hovered viewport and buffer positions;
- historical CWD and shell identity for each candidate start row;
- the profile `PathTranslationStyle`;
- active-buffer mutation, path-context, and viewport generations; and
- extraction counters used to assert the scan bounds.

The snapshot contains no `TextBuffer` references. Candidate boundaries are
formed around the hovered fragment, single spaces, and path separators.
Aligned whitespace, control characters, quotes, and unsafe filename
punctuation terminate a candidate region. Combinations are ordered
longest-to-shortest, always contain the hovered point, and are capped at 64.
The text/cell neighborhood is capped at 4096.

One lazy worker exists per `ControlCore`. It validates at most one request at a
time and retains only the newest pending request. A blocked local/provider
probe delays that worker only; it does not block terminal output, rendering,
or create additional workers. Worker state is independent of `ControlCore`
lifetime, so shutdown can cancel queued work without waiting for an in-flight
filesystem call.

Completion is posted to the control dispatcher. TerminalCore applies it only
if the request still matches the hovered cell, active buffer mutation,
historical path-context generation, translation style, and viewport. It also
rechecks that no higher-precedence OSC 8 or regex link appeared. Applying a
result stores one ephemeral hover link; it does not modify buffer attributes
or the persistent regex interval tree.

Successful completion explicitly refreshes the renderer's hovered interval
and raises `HoveredHyperlinkChanged`, even though `_lastHoveredCell` has not
changed. Pointer exit, a new hover, output mutation, settings changes, resize,
buffer clearing, or newer request invalidates pending and resolved state.

Mouse and keyboard activation first snapshot the current ephemeral candidate
under the terminal lock, then release the lock and synchronously repeat strict
filesystem validation without the hover cache. Only that fresh result is sent
through `OpenHyperlink` with `IsAutoDetectedFilePath=true`.

### Historical working-directory and shell binding

Resolving a relative path against the current working directory at click time
is incorrect:

```text
C:\repo-a> dir README.md
C:\repo-a> cd C:\repo-b
C:\repo-b>
```

The earlier `README.md` must continue to resolve to
`C:\repo-a\README.md`.

Shell integration already reports the working directory through OSC 7 or OSC
9;9. The implementation stores 32-bit interned working-directory and shell IDs
on the current row whenever shell integration reports them. Line feed and
soft-wrap propagation eagerly carry those IDs onto newly produced rows. The
nearest preceding row remains a bounded fallback for sparse or restored
metadata. This prevents old WSL output from being reinterpreted after the pane
returns to PowerShell or another nested shell without making hover cost depend
on total scrollback length.

The metadata is preserved through:

- circular-buffer rotation;
- buffer reflow and resize;
- main-to-alternate buffer transitions; and
- alternate-to-main buffer transitions.

`TextBuffer` owns bounded ID-to-string maps for both metadata types. Unused
entries are pruned, and each map retains at most 4096 entries.

During pattern scanning, the effective working directory is computed once for
each row in the scan range. Candidates on that row reuse the result instead of
independently walking backward through scrollback.

### File-path resolution

An `OpenFile` candidate is processed as follows:

1. Separate and overflow-check an optional positive line and column.
2. Resolve relative paths using the historical working directory and shell.
3. Translate `/mnt/c`, `/cygdrive/c`, or `/c` according to WSL/Cygwin/MSYS2
   semantics. MinGW accepts its existing `C:/path` form and does not reinterpret
   `/c/path`.
4. Generate `\\wsl$\<distro>` only from a validated historical
   `wsl:<distro>` identity.
5. Apply lexical normalization.
6. Reject arbitrary UNC and remote-drive paths.
7. Walk every component with `GetFileAttributesW`.
8. Reject missing components and any reparse-point component.
9. Convert the validated path to a `file://` URI with
   `UrlCreateFromPathW`.
10. Append `#L<line>[:<column>]` after URI creation, so `#` characters in the
    actual path remain escaped as `%23`.

The component walk verifies existence and prevents a seemingly local path from
redirecting through a junction or symbolic link to another location. The
tradeoff is that files under OneDrive placeholders, junction-based development
trees, or other legitimate reparse points are not linked. Reparse-tag-aware
policy is a future refinement.

### Detection cache

Filesystem validation is substantially more expensive than regex matching and
originally repeated on every pattern refresh. TerminalCore now maintains a
bounded cache keyed by the normalized absolute native path. The hover worker
owns a separate cache because it validates outside the Terminal lock. Hover
keys additionally include historical CWD, shell identity, translation style,
trusted-provider state, and location suffix.

| Property | Value |
|---|---|
| Maximum entries | 512 |
| Positive TTL | 5 seconds |
| Negative TTL | 1 second |
| Eviction | Least recently used generation |
| Container | `std::map` |

Each cache contains the normalized context key, resolved URI, expiry time, and
LRU generation. Both store positive and negative results so repeated output or
pointer movement does not repeatedly query the filesystem. The worker cache is
accessed only by its single worker thread; the existing regex cache remains
owned under TerminalCore synchronization.

The entry limit bounds the number of records, not their total bytes. Typical
usage is expected to consume hundreds of kilobytes to approximately one
megabyte. A future hardening step may add a total string-byte budget for
pathological long paths.

TTL caching creates a short presentation-consistency window:

- a deleted file may remain underlined for up to five seconds; and
- a newly created file may remain unlinked for up to one second.

This does not create an activation security window. Detection and activation
use different resolution modes:

- regex and hover detection may accept their respective cached result;
- activation always bypasses the hover cache and repeats all existence, drive,
  directory/location, and reparse checks.

Consequently, a stale underline cannot open a deleted or newly unsafe target.

### Settings and activation behavior

The existing `DetectURLs` setting still gates both URL and file detection. The
global `filePathEditor` setting selects activation:

```jsonc
{
    "filePathEditor": "default" // or "environment"
}
```

`default` removes the internal location fragment and preserves the existing
Windows file-association behavior. `environment` reads `$EDITOR`, parses it
with `CommandLineToArgvW`, resolves and launches the executable directly with
`CreateProcessW`, preserves configured arguments, and adds editor-specific
location arguments for VS Code-family editors, Vim, Emacs, JetBrains
launchers, Zed, and Notepad++. Unknown editors receive only the path.
Directories always open in Explorer. Empty, invalid, or failing `$EDITOR`
commands show the standard open-link error and never silently fall back.

## Relationship to the trigger spec

The draft in [PR #15700] proposes a general system:

```text
regex matcher + capture groups + schedule/scope + arbitrary action
```

This design implements a smaller passive subset:

```text
built-in regex matcher + resolver + click action
```

### Implemented overlap

- Regex matching against terminal-buffer text.
- Clickable intervals rendered through the existing hyperlink UX.
- Deterministic overlap priority.
- Built-in `OpenUri`-like behavior.
- A distinct file-opening action with contextual resolution.
- A matcher identifier carried by interval-tree entries.

### Gaps from the draft

- No user-defined matcher settings.
- No capture-group storage or `${match[n]}` substitution.
- No `runOn` policy such as `everything`, `newline`, or `mark`.
- No `main`, `alt`, or `any` buffer scope per matcher.
- No anchor, offset, or line-count scope.
- No profile layering, IDs, unbinding, or fragments.
- No `clickableSendInput`.
- No active actions such as `sendInput`, `addMark`, or application commands.
- No `TerminalControl` consumer API.
- No custom action event across the Control/App boundary.
- No configurable tooltip, context menu, or appearance.
- No general background matcher execution architecture; only the dedicated
  bounded hover-path resolver runs asynchronously.

### Design direction

`ClickableMatcher` should remain the passive recognition layer. A future
`Trigger` can compose a matcher with a schedule and an action:

```text
ClickableMatcher
    pattern
    scope
    captures
    |
    +--> passive ClickAction
    |
    `--> TriggerSchedule + TriggerAction
```

This avoids making every clickable region an automatically executing trigger
while still allowing both systems to share pattern compilation, capture
substitution, scope, precedence, and buffer-coordinate handling.

## Security

### Threat model

Terminal output is untrusted. A remote process, build script, package, or
malicious file can print arbitrary text. Merely printing text must not cause a
command to execute or a remote resource to be contacted.

### Current controls

- Matching alone performs no action.
- Local drive paths and Terminal-generated WSL provider paths are accepted.
- Arbitrary UNC and remote-drive paths are rejected before conversion to
  `file://`.
- Every component must exist.
- Reparse points are rejected.
- Directories are accepted only when no line or column suffix is present.
- Hover candidates are bounded and filesystem work runs on one coalescing
  worker rather than the UI, renderer, or terminal-output thread.
- Activation repeats validation and never trusts the detection cache.
- The target is routed through the existing application hyperlink handler.
- `$EDITOR` is tokenized without shell interpretation, every argument is
  Windows-command-line quoted, and no `cmd.exe` or script shell is invoked.
- Existing executable-extension warning and URI-scheme policy remain in force.

### Remaining risks

- `ShellExecute` uses file associations. Some file types may execute rather
  than open in an editor.
- `PATHEXT`-based warning does not model every potentially dangerous file
  association.
- The blanket reparse rejection is safe but overly restrictive.
- `$EDITOR` is an explicit user choice but can still point at an unsafe or
  unexpected executable. Launch failure is surfaced rather than hidden.
- Future matcher-defined command targets would create a materially larger
  attack surface and must require separate configuration and confirmation.

## Reliability and compatibility

- OSC 8 hyperlinks retain precedence over automatic matching.
- Existing URL text and click targets remain unchanged.
- URL matches retain priority over overlapping file matches.
- If no historical working directory is available, relative paths are not
  linked rather than being resolved against an unrelated process directory.
- Failure to stat or convert a path produces no link.
- Cache failure cannot bypass activation-time validation.
- Working-directory and shell metadata are bounded and survive buffer lifecycle
  events.
- Stale hover completion is harmless after pointer movement, buffer mutation,
  settings or path-context changes, scrolling, and shutdown.
- Disabling URL detection currently also disables file detection; this
  compatibility limitation will be removed with the independent setting.

## Accessibility

Persistent OSC 8 and regex links reuse existing automatic hyperlink rendering
and keyboard selection. Hover-only bare paths reuse the same tooltip and
activation event once resolved, but are intentionally ephemeral and are not
added to Tab hyperlink navigation. Improving keyboard discovery for bare paths
remains an accessibility follow-up.

Before user-defined matchers ship, accessibility should be verified for:

- announcing the resolved target rather than only the matched text;
- differentiating passive clickable links from actions that send input; and
- exposing any future context-menu or editor-selection actions to keyboard and
  assistive technology.

## Performance, power, and efficiency

### Improvements already implemented

- Regex objects reuse the existing ICU regex interner.
- Scanning remains limited to the viewport and bounded surrounding context.
- Historical working directories are computed once per scanned row.
- Positive and negative filesystem results are cached.
- Cache size is capped at 512 entries.
- Bare-path extraction examines at most 4096 nearby cells/characters and 64
  candidate combinations.
- One coalescing hover worker retains only the newest pending request.

### Residual issue

The hover-only bare-path path is asynchronous and lock-safe. The original
viewport regex pipeline is intentionally unchanged: the first occurrence of
an uncached regex-shaped file candidate can still perform synchronous
filesystem calls while the Terminal write lock is held. A burst containing
many distinct dotted or separator-bearing tokens can therefore still delay
output and rendering. Moving that established prescan validation to a bounded
executor remains separate follow-up work because it affects persistent
interval creation, overlap precedence, and keyboard navigation.

## Testing

The TerminalCore tests currently cover:

- URL start/end boundaries.
- Bare relative file names.
- Absolute Windows paths.
- Quoted paths containing spaces.
- Historical CWD after directory changes.
- Missing files.
- Dotted-token false positives.
- URL/file overlap precedence.
- wrapped links.
- scrollback and viewport-relative intervals.
- circular-buffer CWD propagation.
- alternate/main buffer CWD propagation.
- positive cache reuse.
- negative cache reuse.
- deletion after positive detection.
- creation after negative detection.
- activation-time revalidation.
- every supported line/column grammar and overflow rejection.
- Windows drive-colon non-regression and escaped `#` paths.
- historical shell changes and buffer shell-metadata propagation.
- WSL drive and distro-UNC mapping, MSYS2 and Cygwin mapping, MinGW semantics,
  and arbitrary-UNC rejection.
- `filePathEditor` default/JSON serialization and editor argument adapters.
- bare directories, extensionless files, unquoted spaces, longest valid
  candidate, nested paths, and soft wraps;
- current-point containment and PowerShell table-column exclusion;
- historical CWD and WSL shell snapshots plus MSYS2/Cygwin conversion;
- stale buffer/path-context generation rejection and pointer-exit clearing;
- bounded candidate, character, row, and long-scrollback extraction;
- positive/negative hover TTL behavior, 512-entry eviction, and activation
  revalidation; and
- worker coalescing and nonblocking destruction during a blocked probe.

Future coverage should add:

- long-path byte-budget behavior;
- high-volume matcher benchmarks;
- junction, symbolic-link, and cloud-placeholder policy;
- independent detection settings;
- user regex compilation errors and capture substitution; and
- live UI automation for tooltip placement and Ctrl+Click activation.

## Roadmap

### Phase 0: Built-in file links - complete

- [x] Detect common local Windows file paths.
- [x] Reuse hover, tooltip, keyboard navigation, and click handling.
- [x] Resolve relative paths against historical CWD.
- [x] Validate existence and reject remote/reparse paths.
- [x] Preserve metadata through rotation, reflow, and alternate buffers.
- [x] Add false-positive and lifecycle tests.

### Phase 1: Internal matcher foundation and performance - complete

- [x] Introduce `ClickableMatcher`, `ClickableMatch`, and `ClickAction`.
- [x] Migrate URL and file recognition to built-in matchers.
- [x] Define matcher-order overlap priority.
- [x] Add bounded positive/negative caching.
- [x] Separate detection from activation-time resolution.
- [x] Batch historical-CWD lookup.

### Phase 2: Productize the built-in feature - in progress

- [ ] Add `detectFilePaths` independently from `detectURLs`.
- [x] Add the `filePathEditor` schema, defaults, settings UI, and documentation.
- [x] Add line/column parsing and editor-aware activation.
- [x] Add historical WSL/Cygwin/MSYS2/MinGW path translation.
- [x] Add existence-checked hover-time bare directories, extensionless files,
      and unquoted paths with spaces without expanding viewport prescan.
- [ ] Define reparse-tag policy for OneDrive and common development junctions.
- [ ] Add telemetry or local diagnostics for matcher latency and rejection
      reasons without recording terminal contents or paths.
- [x] Move hover-only bare-path validation off the Terminal write lock with one
      coalescing worker.
- [ ] Move established regex-prescan filesystem validation off the Terminal
      write lock.
- [ ] Build, deploy, and run manual performance tests against large compiler
      output and directory listings.

### Phase 3: Configurable passive clickable matchers

- [ ] Finalize a public schema aligned with [PR #15700].
- [ ] Support stable matcher IDs, profile layering, fragments, and unbinding.
- [ ] Add regex capture storage and `${match[n]}` substitution.
- [ ] Add `OpenUri` targets.
- [ ] Add an explicit `OpenFile` action rather than treating every target as a
      shell command.
- [ ] Add configurable buffer and prompt/output scope.
- [ ] Expose matcher diagnostics for invalid regexes and unsafe targets.
- [ ] Provide a `TerminalControl` consumer API for [#6969].

### Phase 4: Editor-aware file links - complete

- [x] Add system-default or `$EDITOR` selection similar to Warp.
- [x] Parse common `path:line:column` forms without including unrelated punctuation in
      the clickable path.
- [x] Define editor adapters and an internal `file://...#Lline:column` format.
- [x] Resolve Windows versus WSL/Cygwin/MSYS path namespaces using explicit
      profile or shell context rather than regex guessing.
- [ ] Add context-menu choices such as default application, configured editor,
      copy absolute path, and reveal in Explorer.

### Phase 5: Generic triggers

- [ ] Add `runOn` scheduling (`newline`, `mark`, and carefully gated
      `everything`).
- [ ] Add active actions only after permission and reentrancy design.
- [ ] Support `clickableSendInput`, `sendInput`, `addMark`, and custom
      application actions.
- [ ] Prevent trigger loops, repeated firing, and automatic execution from
      untrusted output.
- [ ] Integrate with shell marks and Quick Fix.

Phase 5 is the continuation of the generic trigger proposal, not a prerequisite
for shipping built-in file links.

## Alternatives considered

### Implement the entire trigger spec first

This would maximize generality but delay the concrete file-link scenario behind
settings layering, action execution, scheduling, WinRT API design, and security
work. The chosen approach establishes a passive matcher foundation without
prematurely enabling active triggers.

### Treat matched files as ordinary URL text

The existing URL path assumes that matched cells contain the final target.
Relative files need historical CWD and filesystem validation, so a distinct
resolver/action is required.

### Resolve against the current CWD

This produces incorrect links for scrollback after `cd`, tab reuse, or shell
navigation. Per-row historical metadata is required.

### Validate only when clicked

This avoids scan-time I/O but underlines every syntactic candidate, including
nonexistent files and unrelated dotted tokens. The current design validates
during detection for UX quality and revalidates during activation for safety.
Bare words use asynchronous hover-time validation so prose is never
pre-underlined and does not add viewport-wide filesystem work.

### Store a full CWD string on every row

This is simple but duplicates large strings across scrollback. Interned IDs
preserve historical accuracy with bounded storage, while eager ID inheritance
makes current-row lookup constant time.

### Permit UNC and reparse paths

This improves compatibility but can turn hover-time detection into network or
provider I/O. The initial policy favors predictable local behavior. A future
policy can selectively permit known-safe reparse tags.

## Open questions

- Which reparse tags are safe enough to validate during detection?
- Should built-in URL and file matchers be represented as implicit trigger IDs
  once public trigger settings exist?
- Should a configured editor receive an absolute Windows path, a file URI, or
  an environment-specific path?
- How should profile identity distinguish Windows, WSL, Cygwin, MSYS, SSH, and
  container path semantics?
- What confirmation model is required before a user-defined matcher may launch
  an executable or send input?
- Should cached entries also have a total byte budget in addition to the
  512-entry limit?

## Implementation map

| Area | File |
|---|---|
| Matcher/action types | `src/cascadia/TerminalCore/ClickableMatcher.hpp` |
| Pattern scanning, resolution, cache | `src/cascadia/TerminalCore/Terminal.cpp` |
| Terminal matcher state | `src/cascadia/TerminalCore/Terminal.hpp` |
| Hover worker, stale-result application, tooltip refresh | `src/cascadia/TerminalControl/ControlCore.h`, `ControlCore.cpp` |
| Activation-time hover revalidation | `src/cascadia/TerminalControl/ControlInteractivity.cpp`, `ControlCore.cpp` |
| CWD ingestion and buffer switching | `src/cascadia/TerminalCore/TerminalApi.cpp` |
| Per-row CWD and shell IDs | `src/buffer/out/Row.hpp`, `Row.cpp` |
| Metadata inheritance, interning, pruning, rotation, reflow | `src/terminal/adapter/adaptDispatch.cpp`, `src/buffer/out/textBuffer.hpp`, `textBuffer.cpp` |
| Global editor setting and schema | `TerminalSettingsModel`, `TerminalSettingsEditor`, `doc/cascadia/profiles.schema.json` |
| File activation and editor adapters | `src/cascadia/TerminalApp/TerminalPage.cpp`, `FilePathEditorHelpers.h` |
| Behavioral and worker tests | `src/cascadia/UnitTests_TerminalCore/TerminalBufferTests.cpp`, `src/cascadia/UnitTests_Control/ControlCoreTests.cpp` |

## Resources

- [Warp: Files, Links, and Scripts]
- [PR #15700: Spec for Triggers and Custom Clickable Links]
- [#2671: Link generation for files and other data types]
- [#5916: Triggers and actions]
- [#6969: Consumer-provided pattern recognizers and handlers]
- [#8849: Arbitrary hyperlink patterns and custom handlers]
- [#8294: Configure detected-link appearance]
- [OSC 8 hyperlinks]

[Warp: Files, Links, and Scripts]: https://docs.warp.dev/terminal/more-features/files-and-links
[PR #15700: Spec for Triggers and Custom Clickable Links]: https://github.com/microsoft/terminal/pull/15700
[#2671]: https://github.com/microsoft/terminal/issues/2671
[#5916]: https://github.com/microsoft/terminal/issues/5916
[#6969]: https://github.com/microsoft/terminal/issues/6969
[#7562]: https://github.com/microsoft/terminal/issues/7562
[#8294]: https://github.com/microsoft/terminal/issues/8294
[#8395]: https://github.com/microsoft/terminal/issues/8395
[#8849]: https://github.com/microsoft/terminal/issues/8849
[#8978]: https://github.com/microsoft/terminal/issues/8978
[#9793]: https://github.com/microsoft/terminal/issues/9793
[#11901]: https://github.com/microsoft/terminal/issues/11901
[#17612]: https://github.com/microsoft/terminal/issues/17612
[#18391]: https://github.com/microsoft/terminal/issues/18391
[OSC 8 hyperlinks]: https://github.com/Alhadis/OSC8-Adoption
