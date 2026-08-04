---
author: Kaitao @vanzue
created on: 2026-08-04
last updated: 2026-08-04
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
| Independent file-link setting | Planned |
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
| F8 | A user can select an editor and open `path:line:column`. | Planned |
| F9 | A user can define a regex and map capture groups into a clickable target. | Planned |
| F10 | A terminal-control consumer can provide matchers and click handlers. | Future |

## Related issues

This design directly or incrementally addresses the following upstream
requests:

| Issue | Request | Relationship to this design |
|---|---|---|
| [#2671] | Link generation for files and other data types | The built-in file matcher directly addresses the local-file portion. |
| [#8395] | Open disk files and folders in their default application | Files are implemented. Bare directories are not yet supported. |
| [#8978] | Clickable file paths | Directly addressed for supported local Windows file forms. |
| [#9793] | Clickable disk paths | Files are implemented; folders remain planned. |
| [#17612] | Clickable file paths | Directly addressed for the supported grammar. |
| [#18391] | Clickable directory paths | Not yet addressed because bare directory detection has a higher false-positive rate. |
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

The current Intelligent Terminal implementation follows the first two points.
Editor selection and line/column navigation remain roadmap items.

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
- Paths split across wrapped terminal rows.
- Main and alternate screen buffers.
- Opening through the existing `file://` and `ShellExecute` path.

### Deliberate current exclusions

- `path:line:column`.
- Editor-specific command lines.
- Bare directories.
- Most extensionless files, such as `LICENSE`.
- Unix, WSL, Cygwin, MSYS, container, SSH, and other remote path namespaces.
- UNC paths and remote drives.
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

## Solution design

### Component model

```text
terminal output
    |
    v
TextBuffer rows
    |  row metadata: historical working-directory ID
    v
ClickableMatcher registry
    |-- built-in URL entry  ------> ClickAction::OpenUri
    `-- built-in file entry ------> ClickAction::OpenFile
              |
              v
       syntax match + path resolution
              |
              v
       interval tree for hover/render/navigation
              |
              v
       activation-time resolution
              |
              v
       existing OpenHyperlink/ShellExecute pipeline
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

### Historical working-directory binding

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
9;9. The implementation stores a 32-bit interned working-directory ID on the
current row whenever the shell reports it. To resolve a relative path, the
nearest preceding row carrying an ID defines the directory active when the path
was printed.

The metadata is preserved through:

- circular-buffer rotation;
- buffer reflow and resize;
- main-to-alternate buffer transitions; and
- alternate-to-main buffer transitions.

`TextBuffer` owns the ID-to-string and string-to-ID maps. Unused entries are
pruned, and at most 4096 live directory strings are retained. If the limit
cannot be reduced by pruning, new metadata is not recorded rather than allowing
unbounded growth.

During pattern scanning, the effective working directory is computed once for
each row in the scan range. Candidates on that row reuse the result instead of
independently walking backward through scrollback.

### File-path resolution

An `OpenFile` candidate is processed as follows:

1. Parse the matched text as a `std::filesystem::path`.
2. If relative, combine it with the historical working directory.
3. Apply lexical normalization.
4. Reject UNC paths.
5. Reject paths rooted on a `DRIVE_REMOTE` volume.
6. Walk every component with `GetFileAttributesW`.
7. Reject missing components and any reparse-point component.
8. Convert the validated path to a `file://` URI with
   `UrlCreateFromPathW`.

The component walk verifies existence and prevents a seemingly local path from
redirecting through a junction or symbolic link to another location. The
tradeoff is that files under OneDrive placeholders, junction-based development
trees, or other legitimate reparse points are not linked. Reparse-tag-aware
policy is a future refinement.

### Detection cache

Filesystem validation is substantially more expensive than regex matching and
originally repeated on every pattern refresh. TerminalCore now maintains a
bounded cache keyed by the normalized absolute native path.

| Property | Value |
|---|---|
| Maximum entries | 512 |
| Positive TTL | 5 seconds |
| Negative TTL | 1 second |
| Eviction | Least recently used generation |
| Container | `std::map` |

The cache contains the normalized path key, resolved URI, expiry time, and LRU
generation. It stores both positive and negative results so repeated build
output does not repeatedly query the filesystem.

The entry limit bounds the number of records, not their total bytes. Typical
usage is expected to consume hundreds of kilobytes to approximately one
megabyte. A future hardening step may add a total string-byte budget for
pathological long paths.

TTL caching creates a short presentation-consistency window:

- a deleted file may remain underlined for up to five seconds; and
- a newly created file may remain unlinked for up to one second.

This does not create an activation security window. Detection and activation
use different resolution modes:

- `Detect` may accept a cached result;
- `Activate` always bypasses the cache, repeats all existence, drive, and
  reparse checks, and then refreshes the cache.

Consequently, a stale underline cannot open a deleted or newly unsafe target.

### Settings behavior

The prototype currently uses the existing `DetectURLs` setting as the gate for
both URL and file matchers. This is transitional and should not become the
final public contract.

The next settings step should introduce an independent file-path setting while
preserving the existing URL behavior:

```jsonc
{
    "detectURLs": true,
    "detectFilePaths": true
}
```

Before public user-defined matchers are exposed, the settings schema should be
reconciled with the trigger design. Introducing a temporary public `matchers`
schema and later replacing it with `triggers` would create avoidable
compatibility debt.

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
- No background matcher execution architecture.

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
- Only local drive paths are accepted.
- UNC and remote-drive paths are rejected before conversion to `file://`.
- Every component must exist.
- Reparse points are rejected.
- Activation repeats validation and never trusts the detection cache.
- The target is routed through the existing application hyperlink handler.
- Existing executable-extension warning and URI-scheme policy remain in force.

### Remaining risks

- `ShellExecute` uses file associations. Some file types may execute rather
  than open in an editor.
- `PATHEXT`-based warning does not model every potentially dangerous file
  association.
- The blanket reparse rejection is safe but overly restrictive.
- Future custom command targets would create a materially larger attack
  surface and must require explicit user configuration and confirmation.

For these reasons, editor commands and generic trigger actions are not part of
the initial built-in matcher.

## Reliability and compatibility

- OSC 8 hyperlinks retain precedence over automatic matching.
- Existing URL text and click targets remain unchanged.
- URL matches retain priority over overlapping file matches.
- If no historical working directory is available, relative paths are not
  linked rather than being resolved against an unrelated process directory.
- Failure to stat or convert a path produces no link.
- Cache failure cannot bypass activation-time validation.
- Working-directory metadata is bounded and survives buffer lifecycle events.
- Disabling URL detection currently also disables file detection; this
  compatibility limitation will be removed with the independent setting.

## Accessibility

The implementation reuses existing automatic hyperlink rendering and
interaction, including keyboard hyperlink selection. No new inaccessible
mouse-only surface is introduced.

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

### Residual issue

The first occurrence of each uncached file candidate still performs synchronous
filesystem calls while the Terminal write lock is held. A burst containing
many distinct path-like tokens can therefore delay output and rendering.
Rejecting remote paths reduces but does not eliminate local disk, removable
media, filter-driver, or antivirus latency.

The target architecture is asynchronous validation:

1. Under the Terminal lock, snapshot candidate text, coordinates, historical
   working directory, buffer identity, and generation.
2. Validate paths on a bounded worker queue.
3. Reacquire the lock.
4. Discard results if the buffer identity or generation no longer matches.
5. Commit valid intervals and invalidate only affected render regions.
6. Continue to revalidate on activation.

The worker queue must be bounded and coalesce duplicate normalized paths.
Otherwise, malicious output could create unbounded work even if memory remains
bounded.

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

Future coverage should add:

- cache eviction at 512 entries;
- TTL expiry with an injectable clock;
- long-path byte-budget behavior;
- high-volume matcher benchmarks;
- junction, symbolic-link, and cloud-placeholder policy;
- independent settings and profile inheritance;
- user regex compilation errors and capture substitution; and
- stale asynchronous-result rejection.

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

### Phase 2: Productize the built-in feature

- [ ] Add `detectFilePaths` independently from `detectURLs`.
- [ ] Add schema, defaults, settings UI, and documentation.
- [ ] Decide whether bare directories and extensionless files can meet the
      false-positive bar.
- [ ] Define reparse-tag policy for OneDrive and common development junctions.
- [ ] Add telemetry or local diagnostics for matcher latency and rejection
      reasons without recording terminal contents or paths.
- [ ] Move filesystem validation off the Terminal write lock.
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

### Phase 4: Editor-aware file links

- [ ] Add editor selection similar to Warp.
- [ ] Parse common `path:line:column` forms without including punctuation in
      the clickable path.
- [ ] Define editor adapters or URI formats for line/column navigation.
- [ ] Resolve Windows versus WSL/Cygwin/MSYS path namespaces using explicit
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
Asynchronous detection is the preferred eventual compromise.

### Store a full CWD string on every row

This is simple but duplicates large strings across scrollback. Interned IDs
preserve historical accuracy with bounded storage.

### Permit UNC and reparse paths

This improves compatibility but can turn hover-time detection into network or
provider I/O. The initial policy favors predictable local behavior. A future
policy can selectively permit known-safe reparse tags.

## Open questions

- Should bare directories be linked only when explicitly prefixed with `.\`,
  `..\`, or a drive root?
- Should extensionless files require filesystem confirmation plus a known
  filename allowlist?
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
| CWD ingestion and buffer switching | `src/cascadia/TerminalCore/TerminalApi.cpp` |
| Per-row CWD ID | `src/buffer/out/Row.hpp`, `Row.cpp` |
| CWD interning, pruning, rotation, reflow | `src/buffer/out/textBuffer.hpp`, `textBuffer.cpp` |
| Behavioral tests | `src/cascadia/UnitTests_TerminalCore/TerminalBufferTests.cpp` |

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
