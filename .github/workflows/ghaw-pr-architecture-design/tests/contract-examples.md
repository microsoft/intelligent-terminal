# Architecture review contract examples

Use the authoritative reviewer/skill against these bounded source fixtures.
Line numbers below refer to the displayed fixture files, not this Markdown.
Admission metadata is provided independently of the source excerpts; it tests
routing, not inferred patch size. These are synthetic sources, not product
regression claims.

## Competing state ownership: supported concern

Metadata: `src/SettingsView.cpp`, 100 added+deleted lines, one relevant file.
Before this change, `SettingsView` only called `SettingsModel::SetEnabled`;
the model was the single writer and notifier.

Reviewed `src/SettingsModel.cpp`:

```cpp
// 1
void SettingsModel::SetEnabled(bool value)
{
    _enabled = value;
    SaveSettings(_enabled);
    PropertyChanged(L"Enabled");
}
bool SettingsModel::Enabled() { return _enabled; }
```

Changed `src/SettingsView.cpp`:

```cpp
// 1
void SettingsView::Toggle()
{
    _enabled = !_enabled;
    _model.SetEnabled(_enabled);
}
bool SettingsView::Enabled() { return _enabled; }
```

Unchanged consumers:

```cpp
// src/ImportCommand.cpp:1
void ImportCommand::Execute(bool importedValue)
{
    _model.SetEnabled(importedValue);
}
```

```xml
<!-- src/SettingsView.xaml:1 -->
<ToggleSwitch IsOn="{x:Bind Enabled, Mode=OneWay}" />
```

Expected: trace ImportCommand to the model and the view binding to its new
independent field. A model update neither updates the view field nor provides
the view's notification. Report the changed owner and consumer consequence,
not merely "MVVM violation". The minimal alternative restores the existing
model-backed view accessor/toggle and forwards model notifications through the
view's established binding mechanism; it does not invent an event bus or
generic settings framework. Flag missing binding-forwarding implementation as
an evidence gap rather than pretending the replacement was validated.

## Intentional specialization: accept

Metadata: five relevant changed files, zero changed lines (rename-only fixture).
The snippets are moved unchanged. Review the move's consumers; resemblance
alone does not establish a DRY defect.

Reviewed `src/AgentDiscovery.cpp:1`:

```cpp
std::wstring FindAgentExecutable(std::wstring_view id);
```

Reviewed `src/NativeProfile.cpp:1`:

```cpp
Command NativeProfile::BuildCommand()
{
    return { FindAgentExecutable(_id), _userExtraArguments };
}
```

Reviewed `src/AgentPane.cpp:1`:

```cpp
Command AgentPane::BuildCommand()
{
    return { FindAgentExecutable(_id), PaneManagedArguments(_permissionPolicy) };
}
```

Contract: native profiles preserve user argument customization. Pane arguments
are owned by the pane's permission policy. Both intentionally share executable
discovery, not argument policy.

Expected: accept specialization with consumer/contract evidence, no generic
command-builder requirement, no claim of introduced debt from moved code.
State that the excerpts alone do not prove all rename consumers remain valid.

## Depth follows consequence, not volume

Use the same protocol change with two independent admission metadata inputs:

- Small: 104 relevant added+deleted lines across two files.
- Large: 200,000 relevant lines across 1,200 files. The inventory groups
  1,198 files as branding replacements and two as protocol changes. Available
  representative branding diffs change text only; the full group has not
  been inspected.

Both meet the lower gate. The transport contract requires compatibility with
already deployed peers; there is no version negotiation.

Before `src/MessageKind.h:1`:

```cpp
enum class MessageKind { Open = 1, Close = 2 };
```

Changed `src/MessageKind.h:1`:

```cpp
enum class MessageKind { Refresh = 1, Open, Close };
```

Unchanged `src/Encoder.cpp:1`:

```cpp
uint32_t Encode(MessageKind kind) { return static_cast<uint32_t>(kind); }
```

Deployed peer `src/Peer.cpp:1`:

```cpp
void Dispatch(uint32_t kind)
{
    if (kind == 1) Open();
    else if (kind == 2) Close();
}
```

Expected: prioritize and trace the changed enum through encoding to the
deployed peer in both sizes. An Open command now encodes as 2 and closes the
peer's target. Preserve existing wire identities and allocate a new unused
value; do not propose a protocol framework or bury this consequence beneath
branding commentary. For the large input, use the inventory to focus research
without claiming every branding file was verified or refusing solely on size.
Stop this trace once the compatibility consequence and alternative are
established rather than descending into unrelated implementation details.

Counter-evidence variant: keep `Refresh = 1, Open, Close`, but the actual
encoder uses a stable mapping:

```cpp
uint32_t Encode(MessageKind kind)
{
    switch (kind)
    {
    case MessageKind::Open: return 1;
    case MessageKind::Close: return 2;
    case MessageKind::Refresh: return 3;
    }
    throw UnsupportedKind{};
}
```

Expected: refute the wire-identity concern; enum ordering alone is not a
finding. Missing source for the new receiver remains a specific coverage gap,
not proof that existing peers receive Refresh. Do not retain the disproven
finding to meet a quota.

Missing-evidence variant: the encoder and deployed-peer implementation are
unavailable. Expected: identify those as the next consequential reads; do not
assert the direct-cast consequence without evidence. If access remains
unavailable, report the focused gap instead of exploring unrelated details
or issuing a whole-change clean assessment. Lead with the unresolved
compatibility assessment, not a no-findings headline.

## Below threshold: no review

Metadata: `src/SettingsView.cpp`, 99 added+deleted lines, one relevant file.
Expected: the deterministic gate returns `admitted: false`; activation, review,
detector inference and PR publication are skipped. Do not ask a reviewer to
infer whether this small change is architecturally safe, or post a no-findings
assessment. The Node tests execute this routing boundary.
