# Eternal Terminal — Per-tab Save / Restore — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `/save-tab <title>` and `/restore-tab` agent-pane commands that snapshot a single tab's layout + scrollback content and restore it later (focus the original tab if still open, else open a new tab), gated behind `experimental.eternalTerminal.enabled`.

**Architecture:** The Rust `wta` helper renders UI and issues four request/response COM calls via `wtcli`. C++ owns storage (`ApplicationState.SavedTabSessions`), serialization (`Tab::BuildStartupActions(Persist)`), snapshot buffers (a private `SettingsDirectory\SavedTabSessions\{id}\` folder), and the focus-vs-new-tab decision. Content restore reuses WT's existing `buffer_{sessionId}.txt` → `RestoreFromPath` path unchanged, by staging snapshot buffers into the settings root just before replaying the tab's startup actions with `ProcessStartupActions`.

**Tech Stack:** C++/WinRT (MSBuild), Rust (`tools/wta`, clap + ratatui + tokio), WT COM `IProtocolServer` + `wtcli` (CLI11), JsonCpp (`JsonUtils`).

**Design doc:** `doc/specs/eternal-terminal-tab-save-restore.md`

---

## Conventions (read before starting)

- **Worktree:** all work happens in `.worktree/eternal-terminal-save-restore` on branch `dev/yuazha/eternal-terminal-save-restore`.
- **Rust build/test** (this machine): run inside VS2022 Enterprise vcvars64, kill stale `wta.exe` first:
  ```powershell
  Get-Process wta -ErrorAction SilentlyContinue | Stop-Process -Force
  & $env:ComSpec /c 'call "C:\Program Files\Microsoft Visual Studio\2022\Enterprise\VC\Auxiliary\Build\vcvars64.bat" >nul && cargo test --manifest-path tools/wta/Cargo.toml'
  ```
  Build only: replace `cargo test` with `cargo build`.
- **C++ build** (from repo root of the worktree):
  ```powershell
  cmd.exe /c "tools\razzle.cmd && bcz no_clean"
  ```
- **Commit** at the end of each Task. Include the trailer:
  `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`
- **Do not** auto-push; @DDKinger verifies builds before pushing.

## File structure map

| File | Change | Responsibility |
|---|---|---|
| `src/cascadia/TerminalSettingsModel/MTSMSettings.h` | modify | Declare global bool `EternalTerminalEnabled`. |
| `src/cascadia/TerminalSettingsModel/GlobalAppSettings.idl` | modify | Project `EternalTerminalEnabled`. |
| `src/cascadia/TerminalApp/TerminalPage.cpp` | modify | Append `--eternal-terminal` to helper cmdline. |
| `src/cascadia/TerminalSettingsModel/ApplicationState.{idl,h,cpp}` | modify | New `SavedTabSession` type + `SavedTabSessions` store + upsert/remove. |
| `src/cascadia/TerminalApp/TerminalPage.{idl,h}` | modify | Declare 4 `*Protocol` coroutines + helpers. |
| `src/cascadia/TerminalApp/TerminalPage.Protocol.cpp` | modify | Save/List/Restore/Delete logic + snapshot buffer folder. |
| `src/cascadia/TerminalProtocol/TerminalProtocol.idl` | modify | 4 new `IProtocolServer` methods. |
| `src/cascadia/WindowsTerminal/TerminalProtocolComServer.{h,cpp}` | modify | COM impls delegating to the page. |
| `src/tools/wtcli/main.cpp` | modify | 4 new subcommands. |
| `tools/wta/src/main.rs` | modify | clap `--eternal-terminal` flag. |
| `tools/wta/src/app.rs` | modify | `eternal_terminal_enabled` field, dispatch, picker state/keys, AppEvents. |
| `tools/wta/src/commands.rs` | modify | `SaveTab`/`RestoreTab` command kinds + gating. |
| `tools/wta/src/shell/wt_channel/cli_channel.rs` | modify | 4 `spawn_wtcli_*` helpers returning results via AppEvent. |
| `tools/wta/src/ui/saved_tabs_view.rs` | create | Picker rendering. |
| `tools/wta/src/ui/mod.rs` | modify | `pub mod saved_tabs_view;`. |
| `tools/wta/locales/en-US.yml` (+ others) | modify | Command summaries + system messages. |
| `test/e2e/tests/Feature.EternalTerminal.Tests.ps1` | create | E2E coverage. |

---

# Phase A — Settings flag + command gating

Thin vertical slice: the setting exists, reaches the helper, and shows/hides the two (not-yet-functional) commands. Fully unit-testable in Rust.

### Task A1: Declare the global bool setting

**Files:**
- Modify: `src/cascadia/TerminalSettingsModel/MTSMSettings.h:76-84`
- Modify: `src/cascadia/TerminalSettingsModel/GlobalAppSettings.idl:118-143`

- [ ] **Step 1: Add the X-macro line**

In `MTSMSettings.h`, inside `MTSM_GLOBAL_SETTINGS(X)`, add after the `AgentPanePosition` line:

```cpp
    X(bool, EternalTerminalEnabled, "experimental.eternalTerminal.enabled", false)                                                                                     \
```

(Ensure the trailing backslash lines up; it must be the continuation of the macro. If it is the last entry, it must NOT have a trailing backslash — place it *before* the current last line to avoid backslash bookkeeping.)

- [ ] **Step 2: Project it in the IDL**

In `GlobalAppSettings.idl`, in the `INHERITABLE_SETTING` block (after `AgentPanePosition`):

```idl
        INHERITABLE_SETTING(Boolean, EternalTerminalEnabled);
```

- [ ] **Step 3: Build-verify the settings model**

Run: `cmd.exe /c "tools\razzle.cmd && bcz no_clean"`
Expected: build succeeds; `GlobalSettings().EternalTerminalEnabled()` is now available (generated by the macro).

- [ ] **Step 4: Commit**

```powershell
git add src/cascadia/TerminalSettingsModel/MTSMSettings.h src/cascadia/TerminalSettingsModel/GlobalAppSettings.idl
git commit -m "settings: add experimental.eternalTerminal.enabled global (default off)"
```

### Task A2: Plumb the flag to the wta helper

**Files:**
- Modify: `src/cascadia/TerminalApp/TerminalPage.cpp:1841-1845` (the `--no-autofix` block)

- [ ] **Step 1: Append `--eternal-terminal` when enabled**

Immediately after the existing block:

```cpp
        if (!globals.EffectiveAutoFixEnabled())
        {
            helperCmd.append(L" --no-autofix");
        }
```

add:

```cpp
        if (globals.EternalTerminalEnabled())
        {
            helperCmd.append(L" --eternal-terminal");
        }
```

- [ ] **Step 2: Build-verify**

Run: `cmd.exe /c "tools\razzle.cmd && bcz no_clean"`
Expected: build succeeds.

- [ ] **Step 3: Commit**

```powershell
git add src/cascadia/TerminalApp/TerminalPage.cpp
git commit -m "TerminalPage: pass --eternal-terminal to agent-pane helper when enabled"
```

### Task A3: Receive the flag in Rust (clap → App)

**Files:**
- Modify: `tools/wta/src/main.rs:176-183` (clap flags), `:2792-2793` (App::new call)
- Modify: `tools/wta/src/app.rs:1952` (field), `~2154-2167` (App::new signature + body)

- [ ] **Step 1: Add the clap flag**

In `main.rs`, next to `no_autofix`:

```rust
    /// Enable the experimental Eternal Terminal save/restore-tab commands
    #[arg(long)]
    eternal_terminal: bool,
```

- [ ] **Step 2: Add the App field**

In `app.rs`, next to `pub autofix_enabled: bool,`:

```rust
    /// Gates the experimental `/save-tab` and `/restore-tab` slash commands.
    /// Set from the `--eternal-terminal` CLI flag (mirrors `--no-autofix`).
    pub eternal_terminal_enabled: bool,
```

- [ ] **Step 3: Thread through `App::new`**

In `app.rs`, add a parameter to `App::new` right after `autofix_enabled: bool,`:

```rust
        eternal_terminal_enabled: bool,
```

and in the struct literal it returns, next to `autofix_enabled,`:

```rust
            eternal_terminal_enabled,
```

- [ ] **Step 4: Pass it at the call site**

In `main.rs`, update the `App::new(...)` call (line ~2793) to pass `cli.eternal_terminal` in the new position (immediately after `autofix_enabled`):

```rust
            let autofix_enabled = !cli.no_autofix;
            let mut app_state = app::App::new(prompt_tx, recommendation_tx, permission_tx, cancel_tx, new_session_tx, load_session_tx, drop_session_tx, rename_session_tx, restart_tx, master_ext_tx, debug_capture_enabled, wt_connected, autofix_enabled, cli.eternal_terminal, Arc::clone(&shell_mgr));
```

Search for every other `App::new(` call (tests, other entry points) and add `false` (or `true` for gating tests) in the new position so the code compiles.

- [ ] **Step 5: Build-verify**

Run (vcvars64): `cargo build --manifest-path tools/wta/Cargo.toml`
Expected: compiles. Fix any missed `App::new(` call sites (they will error with arg-count mismatch).

- [ ] **Step 6: Commit**

```powershell
git add tools/wta/src/main.rs tools/wta/src/app.rs
git commit -m "wta: receive --eternal-terminal flag into App.eternal_terminal_enabled"
```

### Task A4: Add gated command kinds (TDD)

**Files:**
- Modify: `tools/wta/src/commands.rs`
- Test: `tools/wta/src/commands.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write failing tests**

Add to the `tests` module in `commands.rs`:

```rust
    #[test]
    fn save_and_restore_tab_parse() {
        let s = parse("/save-tab my work").unwrap();
        assert_eq!(s.kind, CommandKind::SaveTab);
        assert_eq!(s.rest, "my work");
        assert!(lookup("save-tab").unwrap().takes_args);

        let r = parse("/restore-tab").unwrap();
        assert_eq!(r.kind, CommandKind::RestoreTab);
        assert!(!lookup("restore-tab").unwrap().takes_args);
    }

    #[test]
    fn eternal_commands_hidden_without_flag() {
        // Flag off → matches() must not surface the two eternal commands.
        let visible: Vec<&str> = matches_gated("", false).into_iter().map(|c| c.name).collect();
        assert!(!visible.contains(&"save-tab"));
        assert!(!visible.contains(&"restore-tab"));
        // Non-gated commands still present.
        assert!(visible.contains(&"help"));

        // Flag on → both present.
        let visible_on: Vec<&str> = matches_gated("", true).into_iter().map(|c| c.name).collect();
        assert!(visible_on.contains(&"save-tab"));
        assert!(visible_on.contains(&"restore-tab"));
    }

    #[test]
    fn gated_flag_marks_only_eternal_commands() {
        assert!(lookup("save-tab").unwrap().experimental_eternal);
        assert!(lookup("restore-tab").unwrap().experimental_eternal);
        assert!(!lookup("help").unwrap().experimental_eternal);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run (vcvars64): `cargo test --manifest-path tools/wta/Cargo.toml commands::`
Expected: FAIL (unknown `CommandKind::SaveTab`, `matches_gated`, `experimental_eternal`).

- [ ] **Step 3: Implement**

In `commands.rs`, add to the `CommandKind` enum:

```rust
    /// Snapshot the current tab (layout + scrollback) under a user title.
    /// Experimental — gated behind `experimental.eternalTerminal.enabled`.
    SaveTab,
    /// Open the saved-tab picker to restore a snapshot.
    /// Experimental — gated behind `experimental.eternalTerminal.enabled`.
    RestoreTab,
```

Add a field to `CommandSpec`:

```rust
    /// True for commands gated behind `experimental.eternalTerminal.enabled`.
    /// Such commands are hidden from the popup/help and rejected by the
    /// dispatcher unless the flag is on.
    pub experimental_eternal: bool,
```

Set `experimental_eternal: false` on every existing `REGISTRY` entry, and append two new entries:

```rust
    CommandSpec {
        name: "save-tab",
        summary_key: "commands.save_tab.summary",
        kind: CommandKind::SaveTab,
        takes_args: true, // `/save-tab <title>`
        experimental_eternal: true,
    },
    CommandSpec {
        name: "restore-tab",
        summary_key: "commands.restore_tab.summary",
        kind: CommandKind::RestoreTab,
        takes_args: false,
        experimental_eternal: true,
    },
```

Add the gated matcher next to `matches`:

```rust
/// Prefix-match like [`matches`], but hide experimental commands unless
/// `eternal_enabled` is true. Callers that render the popup / `/help` and
/// the dispatcher's Tab-completion use this so gated commands never appear
/// when the feature flag is off.
pub fn matches_gated(prefix: &str, eternal_enabled: bool) -> Vec<&'static CommandSpec> {
    matches(prefix)
        .into_iter()
        .filter(|spec| eternal_enabled || !spec.experimental_eternal)
        .collect()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run (vcvars64): `cargo test --manifest-path tools/wta/Cargo.toml commands::`
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add tools/wta/src/commands.rs
git commit -m "wta/commands: add gated /save-tab and /restore-tab kinds + matches_gated"
```

### Task A5: Wire gating into the popup + dispatcher

**Files:**
- Modify: `tools/wta/src/app.rs` (`handle_slash_command` at ~7396; the command-popup source; add stub `cmd_save_tab`/`cmd_restore_tab`)

- [ ] **Step 1: Route the two kinds (stub) in `handle_slash_command`**

In the `match cmd.kind` block (app.rs ~7419), add:

```rust
            CommandKind::SaveTab => self.cmd_save_tab(cmd.rest),
            CommandKind::RestoreTab => self.cmd_restore_tab(),
```

Immediately before the `match`, add the gating guard (after the `transport_lost` guard):

```rust
        // Experimental commands are invisible + inert unless the feature
        // flag is on. Defense-in-depth: the popup already hides them via
        // `matches_gated`, but a user could type `/save-tab` blind.
        if cmd.spec.experimental_eternal && !self.eternal_terminal_enabled {
            let tab = self.current_tab_mut();
            tab.messages.push(ChatMessage::System(
                t!("commands.eternal_disabled").into_owned(),
            ));
            tab.scroll_to_bottom();
            return;
        }
```

- [ ] **Step 2: Add stub command handlers**

Add near `cmd_sessions` (app.rs ~7616):

```rust
    /// `/save-tab <title>` — snapshot this tab. Implemented in Phase E.
    fn cmd_save_tab(&mut self, _title: String) {
        let tab = self.current_tab_mut();
        tab.messages.push(ChatMessage::System("save-tab: not yet implemented".to_string()));
        tab.scroll_to_bottom();
    }

    /// `/restore-tab` — open the saved-tab picker. Implemented in Phase E.
    fn cmd_restore_tab(&mut self) {
        let tab = self.current_tab_mut();
        tab.messages.push(ChatMessage::System("restore-tab: not yet implemented".to_string()));
        tab.scroll_to_bottom();
    }
```

- [ ] **Step 3: Make the command popup honor gating**

Find where the popup's suggestion list is built (grep `commands::matches(` in `app.rs` / `ui/command_popup.rs`). Replace the `commands::matches(prefix)` call that feeds the popup with:

```rust
        commands::matches_gated(prefix, self.eternal_terminal_enabled)
```

If `/help` renders the command list, update that call site the same way.

- [ ] **Step 4: Add locale strings**

In `tools/wta/locales/en-US.yml` (and the other locale files — copy the English text; translators handle it later, per repo convention) add:

```yaml
commands:
  save_tab:
    summary: "Save this tab (layout + output) to restore later"
  restore_tab:
    summary: "Restore a saved tab"
  eternal_disabled: "This command is disabled. Enable experimental.eternalTerminal.enabled in settings."
```

- [ ] **Step 5: Build + test**

Run (vcvars64): `cargo test --manifest-path tools/wta/Cargo.toml`
Expected: PASS (existing tests + Task A4). Fix compile errors from missed `App::new` sites if any remain.

- [ ] **Step 6: Commit**

```powershell
git add tools/wta/src/app.rs tools/wta/src/ui/command_popup.rs tools/wta/locales
git commit -m "wta: gate /save-tab + /restore-tab in popup/dispatcher; stub handlers"
```

---

# Phase B — C++ storage: `SavedTabSession` + `ApplicationState`

### Task B1: Declare `SavedTabSession` + store in the IDL

**Files:**
- Modify: `src/cascadia/TerminalSettingsModel/ApplicationState.idl`

- [ ] **Step 1: Add the runtimeclass + store + methods**

After the `WindowLayout` runtimeclass, add:

```idl
    runtimeclass SavedTabSession
    {
        SavedTabSession();

        static String ToJson(SavedTabSession session);
        static SavedTabSession FromJson(String json);

        String Id;
        String Title;
        String SourceStableId;
        String SavedAt; // decimal Unix epoch milliseconds, as a string
        Windows.Foundation.Collections.IVector<ActionAndArgs> TabActions;
        Windows.Foundation.Collections.IVector<String> BufferSessionIds;
    };
```

Inside `runtimeclass ApplicationState`, add the store + two mutators (near `AppendPersistedWindowLayout`):

```idl
        Windows.Foundation.Collections.IVector<SavedTabSession> SavedTabSessions;
        void UpsertSavedTabSession(SavedTabSession session);
        Boolean RemoveSavedTabSession(String id);
```

- [ ] **Step 2: Build-verify**

Run: `cmd.exe /c "tools\razzle.cmd && bcz no_clean"`
Expected: build fails at link/impl (methods not implemented yet) — that's fine; the IDL/projection must at least compile. If the projection step errors, fix the IDL syntax before proceeding.

### Task B2: Declare struct + field + methods in the header

**Files:**
- Modify: `src/cascadia/TerminalSettingsModel/ApplicationState.h:34-108`

- [ ] **Step 1: Add the store to the fields X-macro**

In `MTSM_APPLICATION_STATE_FIELDS(X)`, add (Local source, like `PersistedWindowLayouts`):

```cpp
    X(FileSource::Local, Windows::Foundation::Collections::IVector<Model::SavedTabSession>, SavedTabSessions, "savedTabSessions")                                          \
```

- [ ] **Step 2: Add the `SavedTabSession` implementation struct**

After the `WindowLayout` struct:

```cpp
    struct SavedTabSession : SavedTabSessionT<SavedTabSession>
    {
        static winrt::hstring ToJson(const Model::SavedTabSession& session);
        static Model::SavedTabSession FromJson(const winrt::hstring& json);

        WINRT_PROPERTY(winrt::hstring, Id);
        WINRT_PROPERTY(winrt::hstring, Title);
        WINRT_PROPERTY(winrt::hstring, SourceStableId);
        WINRT_PROPERTY(winrt::hstring, SavedAt);
        WINRT_PROPERTY(Windows::Foundation::Collections::IVector<Model::ActionAndArgs>, TabActions, nullptr);
        WINRT_PROPERTY(Windows::Foundation::Collections::IVector<winrt::hstring>, BufferSessionIds, nullptr);

        friend ::Microsoft::Terminal::Settings::Model::JsonUtils::ConversionTrait<Model::SavedTabSession>;
    };
```

- [ ] **Step 3: Declare the two mutators**

In `struct ApplicationState`, next to `void AppendPersistedWindowLayout(...)`:

```cpp
        void UpsertSavedTabSession(Model::SavedTabSession session);
        bool RemoveSavedTabSession(const hstring& id);
```

### Task B3: Implement conversion + mutators

**Files:**
- Modify: `src/cascadia/TerminalSettingsModel/ApplicationState.cpp:24-94` (trait), plus the `implementation` namespace (ToJson/FromJson + mutators)

- [ ] **Step 1: Add the `ConversionTrait<SavedTabSession>`**

After the `ConversionTrait<WindowLayout>` specialization, add (mirrors it exactly):

```cpp
    template<>
    struct ConversionTrait<SavedTabSession>
    {
        SavedTabSession FromJson(const Json::Value& json)
        {
            auto s = winrt::make_self<implementation::SavedTabSession>();
            GetValueForKey(json, "id", s->_Id);
            GetValueForKey(json, "title", s->_Title);
            GetValueForKey(json, "sourceStableId", s->_SourceStableId);
            GetValueForKey(json, "savedAt", s->_SavedAt);
            GetValueForKey(json, "tabActions", s->_TabActions);
            GetValueForKey(json, "bufferSessionIds", s->_BufferSessionIds);
            return *s;
        }

        bool CanConvert(const Json::Value& json) { return json.isObject(); }

        Json::Value ToJson(const SavedTabSession& val)
        {
            Json::Value json{ Json::objectValue };
            SetValueForKey(json, "id", val.Id());
            SetValueForKey(json, "title", val.Title());
            SetValueForKey(json, "sourceStableId", val.SourceStableId());
            SetValueForKey(json, "savedAt", val.SavedAt());
            SetValueForKey(json, "tabActions", val.TabActions());
            SetValueForKey(json, "bufferSessionIds", val.BufferSessionIds());
            return json;
        }

        std::string TypeDescription() const { return "SavedTabSession"; }
    };
```

- [ ] **Step 2: Add static `ToJson`/`FromJson` in the implementation namespace**

Mirror `WindowLayout::ToJson`/`FromJson` verbatim, substituting the type:

```cpp
    winrt::hstring SavedTabSession::ToJson(const Model::SavedTabSession& session)
    {
        JsonUtils::ConversionTrait<Model::SavedTabSession> trait;
        auto json = trait.ToJson(session);
        Json::StreamWriterBuilder wbuilder;
        const auto content = Json::writeString(wbuilder, json);
        return hstring{ til::u8u16(content) };
    }

    Model::SavedTabSession SavedTabSession::FromJson(const hstring& str)
    {
        auto data = til::u16u8(str);
        std::string errs;
        std::unique_ptr<Json::CharReader> reader{ Json::CharReaderBuilder{}.newCharReader() };
        Json::Value root;
        if (!reader->parse(data.data(), data.data() + data.size(), &root, &errs))
        {
            throw winrt::hresult_error(WEB_E_INVALID_JSON_STRING, winrt::to_hstring(errs));
        }
        JsonUtils::ConversionTrait<Model::SavedTabSession> trait;
        return trait.FromJson(root);
    }
```

- [ ] **Step 3: Implement `UpsertSavedTabSession` / `RemoveSavedTabSession`**

After `AppendPersistedWindowLayout`:

```cpp
    void ApplicationState::UpsertSavedTabSession(Model::SavedTabSession session)
    {
        {
            const auto state = _state.lock();
            if (!state->SavedTabSessions || !*state->SavedTabSessions)
            {
                state->SavedTabSessions = winrt::single_threaded_vector<Model::SavedTabSession>();
            }
            auto& vec = *state->SavedTabSessions;
            // Overwrite by SourceStableId: at most one snapshot per live tab.
            for (uint32_t i = 0; i < vec.Size(); ++i)
            {
                if (vec.GetAt(i).SourceStableId() == session.SourceStableId())
                {
                    vec.SetAt(i, std::move(session));
                    goto done;
                }
            }
            vec.Append(std::move(session));
        done:;
        }
        _throttler();
    }

    bool ApplicationState::RemoveSavedTabSession(const hstring& id)
    {
        bool removed = false;
        {
            const auto state = _state.lock();
            if (state->SavedTabSessions && *state->SavedTabSessions)
            {
                auto& vec = *state->SavedTabSessions;
                for (uint32_t i = 0; i < vec.Size(); ++i)
                {
                    if (vec.GetAt(i).Id() == id)
                    {
                        vec.RemoveAt(i);
                        removed = true;
                        break;
                    }
                }
            }
        }
        if (removed)
        {
            _throttler();
        }
        return removed;
    }
```

- [ ] **Step 4: Build-verify**

Run: `cmd.exe /c "tools\razzle.cmd && bcz no_clean"`
Expected: build succeeds.

- [ ] **Step 5: Commit**

```powershell
git add src/cascadia/TerminalSettingsModel/ApplicationState.idl src/cascadia/TerminalSettingsModel/ApplicationState.h src/cascadia/TerminalSettingsModel/ApplicationState.cpp
git commit -m "ApplicationState: add SavedTabSession type + SavedTabSessions store (upsert/remove)"
```

---

# Phase C — C++ save/list/restore/delete logic

All four operations are coroutines in `TerminalPage.Protocol.cpp` mirroring `CreateProtocolTab` (`get_strong()` + `co_await wil::resume_foreground(Dispatcher())`).

### Task C1: Declare the page methods + helpers

**Files:**
- Modify: `src/cascadia/TerminalApp/TerminalPage.idl` (add to the `TerminalPage` interface, near `CreateProtocolTab`)
- Modify: `src/cascadia/TerminalApp/TerminalPage.h`

- [ ] **Step 1: IDL declarations**

In `TerminalPage.idl`, alongside the existing `CreateProtocolTab` / `FocusProtocolPane`:

```idl
        Windows.Foundation.IAsyncOperation<String> SaveTabSessionProtocol(String tabStableId, String title);
        Windows.Foundation.IAsyncOperation<String> ListSavedTabSessionsProtocol();
        Windows.Foundation.IAsyncOperation<String> RestoreTabSessionProtocol(String id);
        Windows.Foundation.IAsyncOperation<Boolean> DeleteSavedTabSessionProtocol(String id);
```

- [ ] **Step 2: Header declarations**

In `TerminalPage.h` (public, near `CreateProtocolTab`):

```cpp
        winrt::Windows::Foundation::IAsyncOperation<winrt::hstring> SaveTabSessionProtocol(winrt::hstring tabStableId, winrt::hstring title);
        winrt::Windows::Foundation::IAsyncOperation<winrt::hstring> ListSavedTabSessionsProtocol();
        winrt::Windows::Foundation::IAsyncOperation<winrt::hstring> RestoreTabSessionProtocol(winrt::hstring id);
        winrt::Windows::Foundation::IAsyncOperation<bool> DeleteSavedTabSessionProtocol(winrt::hstring id);
```

Private helper (near other private helpers):

```cpp
        static std::filesystem::path _SavedTabSessionDir(const winrt::hstring& id);
```

- [ ] **Step 3: Build-verify (will link-fail until C2–C5)**

Run: `cmd.exe /c "tools\razzle.cmd && bcz no_clean"`
Expected: compiles the IDL/header; link may fail (unimplemented) — proceed to C2.

### Task C2: Implement `SaveTabSessionProtocol`

**Files:**
- Modify: `src/cascadia/TerminalApp/TerminalPage.Protocol.cpp` (add after `CreateProtocolTab`, ~line 596)

- [ ] **Step 1: Implement the folder helper + save**

```cpp
    std::filesystem::path TerminalPage::_SavedTabSessionDir(const winrt::hstring& id)
    {
        std::filesystem::path dir{ std::wstring_view{ CascadiaSettings::SettingsDirectory() } };
        dir /= L"SavedTabSessions";
        dir /= std::wstring_view{ id };
        return dir;
    }

    IAsyncOperation<winrt::hstring> TerminalPage::SaveTabSessionProtocol(winrt::hstring tabStableId, winrt::hstring title)
    {
        auto strong = get_strong();
        co_await wil::resume_foreground(Dispatcher());

        const auto tab = _FindTabByStableId(tabStableId);
        if (!tab)
        {
            co_return winrt::hstring{}; // empty = not found; COM layer maps to error
        }
        auto tabImpl = _GetTabImpl(tab);
        if (!tabImpl)
        {
            co_return winrt::hstring{};
        }

        // Reuse an existing snapshot id for this tab (overwrite), else new guid.
        winrt::hstring id;
        auto existing = ApplicationState::SharedInstance().SavedTabSessions();
        if (existing)
        {
            for (const auto& s : existing)
            {
                if (s.SourceStableId() == tabStableId)
                {
                    id = s.Id();
                    break;
                }
            }
        }
        if (id.empty())
        {
            id = winrt::to_hstring(winrt::guid{ ::Microsoft::Console::Utils::CreateGuid() });
        }

        // Fresh snapshot folder (clear any stale contents on overwrite).
        const auto dir = _SavedTabSessionDir(id);
        std::error_code ec;
        std::filesystem::remove_all(dir, ec);
        std::filesystem::create_directories(dir, ec);

        // Serialize layout (each pane's SessionId is embedded by Persist).
        auto actions = tabImpl->BuildStartupActions(BuildStartupKind::Persist);

        // Dump each pane's buffer into the snapshot folder, keyed by SessionId.
        std::vector<winrt::hstring> bufferIds;
        for (const auto& pane : tabImpl->GetRootPane()->_Panes()) // leaf panes; see note
        {
            const auto term = pane->GetContent().try_as<winrt::TerminalApp::ITerminalPaneContent>();
            if (!term) { continue; }
            const auto control = term.GetTermControl();
            if (!control) { continue; }
            const auto connection = control.Connection();
            if (!connection) { continue; }
            const auto sid = connection.SessionId();
            if (sid == winrt::guid{}) { continue; }
            const auto sidStr = winrt::to_hstring(sid);
            const auto path = dir / (std::wstring{ L"buffer_" } + std::wstring{ sidStr } + L".txt");
            try
            {
                if (wil::unique_hfile file{ CreateFileW(path.c_str(), GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_DELETE, nullptr, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr) })
                {
                    control.PersistTo(reinterpret_cast<int64_t>(file.get()));
                    bufferIds.push_back(sidStr);
                }
            }
            CATCH_LOG();
        }

        // Assemble + upsert the record.
        Model::SavedTabSession record;
        record.Id(id);
        record.Title(title);
        record.SourceStableId(tabStableId);
        const auto nowMs = std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::system_clock::now().time_since_epoch()).count();
        record.SavedAt(winrt::to_hstring(nowMs));
        record.TabActions(winrt::single_threaded_vector<Model::ActionAndArgs>(std::move(actions)));
        record.BufferSessionIds(winrt::single_threaded_vector<winrt::hstring>(std::move(bufferIds)));
        ApplicationState::SharedInstance().UpsertSavedTabSession(record);

        // Return {"id":..,"title":..} as JSON.
        Json::Value out{ Json::objectValue };
        out["id"] = winrt::to_string(id);
        out["title"] = winrt::to_string(title);
        Json::StreamWriterBuilder wb;
        co_return winrt::to_hstring(Json::writeString(wb, out));
    }
```

> **Note on iterating leaf panes:** use whatever accessor the codebase already
> exposes for a tab's leaf terminal panes. `WindowEmperor::_finalizeSessionPersistence`
> iterates `w->Logic().Panes()` then `pane.try_as<ITerminalPaneContent>()`
> (`WindowEmperor.cpp:1330-1345`). If `Tab`/`Pane` doesn't expose a public leaf
> enumerator, mirror the `Panes()` accessor used there (add a small
> `Tab::LeafPanes()` that walks `_rootPane->WalkTree(...)` collecting leaves).
> Verify the exact accessor before writing this loop; do not invent `_Panes()`.

- [ ] **Step 2: Build-verify**

Run: `cmd.exe /c "tools\razzle.cmd && bcz no_clean"`
Expected: compiles once the pane-enumeration accessor is correct. Fix per the note.

### Task C3: Implement `ListSavedTabSessionsProtocol`

**Files:**
- Modify: `src/cascadia/TerminalApp/TerminalPage.Protocol.cpp`

- [ ] **Step 1: Implement**

```cpp
    IAsyncOperation<winrt::hstring> TerminalPage::ListSavedTabSessionsProtocol()
    {
        auto strong = get_strong();
        co_await wil::resume_foreground(Dispatcher());

        Json::Value arr{ Json::arrayValue };
        const auto sessions = ApplicationState::SharedInstance().SavedTabSessions();
        if (sessions)
        {
            for (const auto& s : sessions)
            {
                Json::Value o{ Json::objectValue };
                o["id"] = winrt::to_string(s.Id());
                o["title"] = winrt::to_string(s.Title());
                o["sourceStableId"] = winrt::to_string(s.SourceStableId());
                o["savedAt"] = winrt::to_string(s.SavedAt());
                // Surface the first pane's starting directory as a hint for the
                // picker. Best-effort: read it back from the first action's args.
                o["isOpen"] = (_FindTabByStableId(s.SourceStableId()) != nullptr);
                arr.append(o);
            }
        }
        Json::StreamWriterBuilder wb;
        co_return winrt::to_hstring(Json::writeString(wb, arr));
    }
```

- [ ] **Step 2: Build-verify**

Run: `cmd.exe /c "tools\razzle.cmd && bcz no_clean"`
Expected: compiles.

### Task C4: Implement `RestoreTabSessionProtocol`

**Files:**
- Modify: `src/cascadia/TerminalApp/TerminalPage.Protocol.cpp`

- [ ] **Step 1: Implement focus-or-restore**

```cpp
    IAsyncOperation<winrt::hstring> TerminalPage::RestoreTabSessionProtocol(winrt::hstring id)
    {
        auto strong = get_strong();
        co_await wil::resume_foreground(Dispatcher());

        // Look up the record.
        Model::SavedTabSession record{ nullptr };
        const auto sessions = ApplicationState::SharedInstance().SavedTabSessions();
        if (sessions)
        {
            for (const auto& s : sessions)
            {
                if (s.Id() == id) { record = s; break; }
            }
        }
        if (!record)
        {
            co_return winrt::hstring{}; // empty = unknown id
        }

        // Focus-if-open: if the source tab is still live, switch to it.
        if (const auto live = _FindTabByStableId(record.SourceStableId()))
        {
            _SelectTab(live);
            Json::Value out{ Json::objectValue };
            out["outcome"] = "focused";
            Json::StreamWriterBuilder wb;
            co_return winrt::to_hstring(Json::writeString(wb, out));
        }

        // New-tab restore. Stage snapshot buffers into the settings root so the
        // unmodified restore path (_MakeTerminalPane -> RestoreFromPath) finds
        // them by SessionId, then replay the tab's startup actions.
        const auto dir = _SavedTabSessionDir(id);
        std::filesystem::path settingsRoot{ std::wstring_view{ CascadiaSettings::SettingsDirectory() } };
        if (const auto ids = record.BufferSessionIds())
        {
            for (const auto& sid : ids)
            {
                const std::wstring name = std::wstring{ L"buffer_" } + std::wstring{ sid } + L".txt";
                std::error_code ec;
                std::filesystem::copy_file(dir / name, settingsRoot / name,
                                           std::filesystem::copy_options::overwrite_existing, ec);
            }
        }

        auto actions = wil::to_vector(record.TabActions());
        ProcessStartupActions(std::move(actions), winrt::hstring{}, winrt::hstring{});

        Json::Value out{ Json::objectValue };
        out["outcome"] = "opened";
        Json::StreamWriterBuilder wb;
        co_return winrt::to_hstring(Json::writeString(wb, out));
    }
```

> `_SelectTab` takes an index in some call sites (`TerminalPage.cpp:6098`) and a
> tab elsewhere; use the overload that focuses a specific `Tab` (or compute its
> index via `_GetTabIndex`). If only cross-window focus is needed later, route
> through `WindowEmperor::FocusTabInAnyWindow`; for step 1 the same-window
> `_SelectTab` is sufficient because the picker runs in the tab's own window.

- [ ] **Step 2: Build-verify**

Run: `cmd.exe /c "tools\razzle.cmd && bcz no_clean"`
Expected: compiles.

### Task C5: Implement `DeleteSavedTabSessionProtocol`

**Files:**
- Modify: `src/cascadia/TerminalApp/TerminalPage.Protocol.cpp`

- [ ] **Step 1: Implement**

```cpp
    IAsyncOperation<bool> TerminalPage::DeleteSavedTabSessionProtocol(winrt::hstring id)
    {
        auto strong = get_strong();
        co_await wil::resume_foreground(Dispatcher());

        const bool removed = ApplicationState::SharedInstance().RemoveSavedTabSession(id);
        std::error_code ec;
        std::filesystem::remove_all(_SavedTabSessionDir(id), ec);
        co_return removed;
    }
```

- [ ] **Step 2: Build-verify**

Run: `cmd.exe /c "tools\razzle.cmd && bcz no_clean"`
Expected: build + link succeed (all four page methods now defined).

- [ ] **Step 3: Commit**

```powershell
git add src/cascadia/TerminalApp/TerminalPage.idl src/cascadia/TerminalApp/TerminalPage.h src/cascadia/TerminalApp/TerminalPage.Protocol.cpp
git commit -m "TerminalPage: implement save/list/restore/delete SavedTabSession protocol methods"
```

---

# Phase D — COM surface + wtcli

### Task D1: Add the four `IProtocolServer` methods (IDL)

**Files:**
- Modify: `src/cascadia/TerminalProtocol/TerminalProtocol.idl` (Mutations/Queries section of `interface IProtocolServer`)

- [ ] **Step 1: Add methods**

```idl
        // Eternal Terminal — per-tab save/restore
        String SaveTabSession(String tabStableId, String title);
        String ListSavedTabSessions();
        String RestoreTabSession(String id);
        void DeleteSavedTabSession(String id);
```

### Task D2: Implement in the COM server

**Files:**
- Modify: `src/cascadia/WindowsTerminal/TerminalProtocolComServer.h` (method decls — mirror `CreateTab`/`FocusPane`)
- Modify: `src/cascadia/WindowsTerminal/TerminalProtocolComServer.cpp`

- [ ] **Step 1: Header decls**

Add STDMETHOD decls mirroring the generated signatures (BSTR in/out for String returns, GUID/BSTR params). Match the exact ABI shape the midlrt-generated interface expects (String → BSTR; String return → `BSTR*`; void → no out-param).

- [ ] **Step 2: Implement (mirror `CreateTab` for target-page resolution)**

```cpp
STDMETHODIMP TerminalProtocolComServer::SaveTabSession(BSTR tabStableId, BSTR title, BSTR* json)
try
{
    RETURN_HR_IF_NULL(E_POINTER, json);
    *json = nullptr;
    RETURN_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    // The tab lives in whichever window owns it; iterate windows and ask each
    // page (the one that finds the tab returns non-empty).
    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;
        const auto result = page.SaveTabSessionProtocol(_hstr(tabStableId), _hstr(title)).get();
        if (!result.empty())
        {
            *json = _bstrFromHstring(result);
            return S_OK;
        }
    }
    return HRESULT_FROM_WIN32(ERROR_NOT_FOUND);
}
CATCH_RETURN()

STDMETHODIMP TerminalProtocolComServer::ListSavedTabSessions(BSTR* json)
try
{
    RETURN_HR_IF_NULL(E_POINTER, json);
    *json = nullptr;
    RETURN_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    // Storage is process-global (ApplicationState singleton); any page answers.
    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;
        *json = _bstrFromHstring(page.ListSavedTabSessionsProtocol().get());
        return S_OK;
    }
    return E_FAIL;
}
CATCH_RETURN()

STDMETHODIMP TerminalProtocolComServer::RestoreTabSession(BSTR id, BSTR* json)
try
{
    RETURN_HR_IF_NULL(E_POINTER, json);
    *json = nullptr;
    RETURN_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    const auto mostRecent = s_emperor->GetMostRecentWindow();
    const auto page = mostRecent ? _getPage(mostRecent) : nullptr;
    RETURN_HR_IF(E_FAIL, !page);
    const auto result = page.RestoreTabSessionProtocol(_hstr(id)).get();
    RETURN_HR_IF(HRESULT_FROM_WIN32(ERROR_NOT_FOUND), result.empty());
    *json = _bstrFromHstring(result);
    return S_OK;
}
CATCH_RETURN()

STDMETHODIMP TerminalProtocolComServer::DeleteSavedTabSession(BSTR id)
try
{
    RETURN_HR_IF(E_NOT_VALID_STATE, !s_emperor);
    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;
        (void)page.DeleteSavedTabSessionProtocol(_hstr(id)).get();
        return S_OK;
    }
    return E_FAIL;
}
CATCH_RETURN()
```

> `_bstrFromHstring` may not exist — the file already has `_bstrFromJson` and
> `_hstr`. Add a tiny helper `static BSTR _bstrFromHstring(const winrt::hstring& s)
> { return SysAllocString(s.c_str()); }` next to them, or reuse the existing
> string→BSTR helper. Verify the existing helper names before use.

- [ ] **Step 3: Build-verify**

Run: `cmd.exe /c "tools\razzle.cmd && bcz no_clean"`
Expected: build succeeds.

- [ ] **Step 4: Commit**

```powershell
git add src/cascadia/TerminalProtocol/TerminalProtocol.idl src/cascadia/WindowsTerminal/TerminalProtocolComServer.h src/cascadia/WindowsTerminal/TerminalProtocolComServer.cpp
git commit -m "COM: add SaveTabSession/ListSavedTabSessions/RestoreTabSession/DeleteSavedTabSession"
```

### Task D3: Add wtcli subcommands

**Files:**
- Modify: `src/tools/wtcli/main.cpp` (mirror the `new-tab` block at :457 and the `focus-pane` block at :569)

- [ ] **Step 1: Add the four subcommands**

```cpp
    // ── save-tab ──
    std::string saveTabId, saveTabTitle;
    auto* saveTabCmd = app.add_subcommand("save-tab", "Save a tab snapshot");
    saveTabCmd->add_option("-t,--tab", saveTabId, "Source tab StableId")->required();
    saveTabCmd->add_option("-n,--title", saveTabTitle, "Snapshot title")->required();
    saveTabCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        wil::unique_bstr tab{ Bstr(saveTabId) }, title{ Bstr(saveTabTitle) };
        Json::Value result;
        auto hr = CallJson([&](BSTR* j) { return server->SaveTabSession(tab.get(), title.get(), j); }, result);
        if (FAILED(hr)) { fprintf(stderr, "SaveTabSession failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        PrintJson(result);
    });

    // ── list-saved-tabs ──
    auto* listSavedCmd = app.add_subcommand("list-saved-tabs", "List saved tab snapshots");
    listSavedCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        Json::Value result;
        auto hr = CallJson([&](BSTR* j) { return server->ListSavedTabSessions(j); }, result);
        if (FAILED(hr)) { fprintf(stderr, "ListSavedTabSessions failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        PrintJson(result);
    });

    // ── restore-tab ──
    std::string restoreTabId;
    auto* restoreTabCmd = app.add_subcommand("restore-tab", "Restore a saved tab snapshot");
    restoreTabCmd->add_option("-i,--id", restoreTabId, "Snapshot id")->required();
    restoreTabCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        wil::unique_bstr sid{ Bstr(restoreTabId) };
        Json::Value result;
        auto hr = CallJson([&](BSTR* j) { return server->RestoreTabSession(sid.get(), j); }, result);
        if (FAILED(hr)) { fprintf(stderr, "RestoreTabSession failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        PrintJson(result);
    });

    // ── delete-saved-tab ──
    std::string deleteTabId;
    auto* deleteSavedCmd = app.add_subcommand("delete-saved-tab", "Delete a saved tab snapshot");
    deleteSavedCmd->add_option("-i,--id", deleteTabId, "Snapshot id")->required();
    deleteSavedCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        wil::unique_bstr sid{ Bstr(deleteTabId) };
        auto hr = server->DeleteSavedTabSession(sid.get());
        if (FAILED(hr)) { fprintf(stderr, "DeleteSavedTabSession failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
    });
```

- [ ] **Step 2: Build-verify**

Run: `cmd.exe /c "tools\razzle.cmd && bcz no_clean"`
Expected: build succeeds; `wtcli save-tab -t X -n Y`, `wtcli list-saved-tabs`, etc. exist.

- [ ] **Step 3: Commit**

```powershell
git add src/tools/wtcli/main.cpp
git commit -m "wtcli: add save-tab/list-saved-tabs/restore-tab/delete-saved-tab subcommands"
```

---

# Phase E — Rust helper: dispatch + picker

### Task E1: wtcli shell-out helpers (with AppEvent delivery)

**Files:**
- Modify: `tools/wta/src/shell/wt_channel/cli_channel.rs` (mirror `spawn_wtcli_split_then_focus_with_callback` at :217)
- Modify: `tools/wta/src/app.rs` (add `AppEvent` variants)

- [ ] **Step 1: Add AppEvent variants**

Find the `AppEvent` enum (grep `enum AppEvent` in `app.rs`) and add:

```rust
    /// Result of `wtcli save-tab`: Ok(title) or Err(message).
    SavedTabResult(Result<String, String>),
    /// Result of `wtcli list-saved-tabs`: the parsed rows (may be empty).
    SavedTabsListed(Vec<crate::app::SavedTabEntry>),
    /// Result of `wtcli restore-tab`: outcome "focused" | "opened", or Err.
    RestoredTabResult(Result<String, String>),
    /// A saved tab was deleted; carries the id so the picker can drop the row.
    SavedTabDeleted(String),
```

- [ ] **Step 2: Add the shell-out helpers**

In `cli_channel.rs` add (mirroring `spawn_wtcli_split_then_focus_with_callback`, which spawns on a thread, `.output()`, parses stdout JSON, invokes a callback):

```rust
/// Run `wtcli save-tab -t <tab> -n <title>` and deliver the result via the
/// supplied callback (invoked on a background thread).
pub fn spawn_wtcli_save_tab(tab_stable_id: &str, title: &str, on_done: Box<dyn FnOnce(Result<String, String>) + Send>) {
    let path = resolve_wtcli_path();
    let (tab, title) = (tab_stable_id.to_string(), title.to_string());
    std::thread::spawn(move || {
        let out = std::process::Command::new(&path)
            .args(["save-tab", "-t", &tab, "-n", &title])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output();
        let result = match out {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout);
                match serde_json::from_str::<serde_json::Value>(s.trim()) {
                    Ok(v) => Ok(v.get("title").and_then(|t| t.as_str()).unwrap_or(&title).to_string()),
                    Err(_) => Ok(title.clone()),
                }
            }
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => Err(e.to_string()),
        };
        on_done(result);
    });
}

/// Run `wtcli list-saved-tabs` and deliver parsed rows via the callback.
pub fn spawn_wtcli_list_saved_tabs(on_done: Box<dyn FnOnce(Vec<crate::app::SavedTabEntry>) + Send>) {
    let path = resolve_wtcli_path();
    std::thread::spawn(move || {
        let rows = std::process::Command::new(&path)
            .args(["list-saved-tabs"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| serde_json::from_slice::<Vec<crate::app::SavedTabEntry>>(&o.stdout).ok())
            .unwrap_or_default();
        on_done(rows);
    });
}

/// Run `wtcli restore-tab -i <id>` and deliver the outcome via the callback.
pub fn spawn_wtcli_restore_tab(id: &str, on_done: Box<dyn FnOnce(Result<String, String>) + Send>) {
    let path = resolve_wtcli_path();
    let id = id.to_string();
    std::thread::spawn(move || {
        let out = std::process::Command::new(&path)
            .args(["restore-tab", "-i", &id])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output();
        let result = match out {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout);
                Ok(serde_json::from_str::<serde_json::Value>(s.trim())
                    .ok()
                    .and_then(|v| v.get("outcome").and_then(|o| o.as_str()).map(str::to_string))
                    .unwrap_or_else(|| "opened".to_string()))
            }
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => Err(e.to_string()),
        };
        on_done(result);
    });
}

/// Run `wtcli delete-saved-tab -i <id>` (fire-and-forget; failures logged).
pub fn spawn_wtcli_delete_saved_tab(id: &str) {
    spawn_wtcli_async(&["delete-saved-tab".into(), "-i".into(), id.to_string()]);
}
```

- [ ] **Step 3: Build-verify**

Run (vcvars64): `cargo build --manifest-path tools/wta/Cargo.toml`
Expected: compiles once `SavedTabEntry` exists (Task E2 Step 1). If ordering causes an error, do E2 Step 1 first.

### Task E2: Picker state + dispatch (TDD)

**Files:**
- Modify: `tools/wta/src/app.rs`
- Test: `tools/wta/src/app.rs` (tests module)

- [ ] **Step 1: Define the entry type + view state**

Near the other view-state structs (e.g. `AgentsViewState`, app.rs ~2083):

```rust
#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
pub struct SavedTabEntry {
    pub id: String,
    pub title: String,
    #[serde(rename = "sourceStableId", default)]
    pub source_stable_id: String,
    #[serde(rename = "savedAt", default)]
    pub saved_at: String, // decimal epoch ms
    #[serde(rename = "isOpen", default)]
    pub is_open: bool,
}

#[derive(Debug, Default)]
pub struct SavedTabsViewState {
    pub entries: Vec<SavedTabEntry>,
    pub selected: usize,
    pub loading: bool,
}
```

Add to `struct App` (near `agents_view`):

```rust
    pub saved_tabs: Option<SavedTabsViewState>,
```

Initialize `saved_tabs: None` in `App::new`.

- [ ] **Step 2: Write failing tests**

```rust
    #[test]
    fn saved_tabs_navigation_clamps() {
        let mut app = App::new_for_test();
        app.eternal_terminal_enabled = true;
        app.saved_tabs = Some(SavedTabsViewState {
            entries: vec![
                SavedTabEntry { id: "a".into(), title: "A".into(), source_stable_id: "".into(), saved_at: "0".into(), is_open: false },
                SavedTabEntry { id: "b".into(), title: "B".into(), source_stable_id: "".into(), saved_at: "0".into(), is_open: false },
            ],
            selected: 0,
            loading: false,
        });
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.saved_tabs.as_ref().unwrap().selected, 1);
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)); // clamp
        assert_eq!(app.saved_tabs.as_ref().unwrap().selected, 1);
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.saved_tabs.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn saved_tabs_esc_closes() {
        let mut app = App::new_for_test();
        app.eternal_terminal_enabled = true;
        app.saved_tabs = Some(SavedTabsViewState::default());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.saved_tabs.is_none());
    }

    #[test]
    fn saved_tabs_enter_dispatches_restore() {
        let mut app = App::new_for_test();
        app.eternal_terminal_enabled = true;
        app.saved_tabs = Some(SavedTabsViewState {
            entries: vec![SavedTabEntry { id: "sid-1".into(), title: "T".into(), source_stable_id: "".into(), saved_at: "0".into(), is_open: false }],
            selected: 0,
            loading: false,
        });
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        // Enter records the dispatched restore id via the test hook.
        assert_eq!(app.last_restore_tab_id.as_deref(), Some("sid-1"));
    }
```

> Use whatever test constructor the suite already uses (grep `fn new_for_test`
> or how existing tests build `App`). If none exists, construct via `App::new`
> with the same args other tests pass, then set fields. Add a
> `#[cfg(test)] pub last_restore_tab_id: Option<String>` hook to `App` and set
> it in the Enter path (mirrors the existing `last_dispatched_command` hook at
> app.rs ~12206).

- [ ] **Step 3: Run tests to verify they fail**

Run (vcvars64): `cargo test --manifest-path tools/wta/Cargo.toml saved_tabs_`
Expected: FAIL (no key handling; hook missing).

- [ ] **Step 4: Implement key handling + dispatch**

Add a saved-tabs key handler and call it from `handle_key` **before** normal input handling when `self.saved_tabs.is_some()` (mirror how `agents_view` / setup intercept keys):

```rust
    fn handle_saved_tabs_key(&mut self, key: KeyEvent) {
        let Some(view) = self.saved_tabs.as_mut() else { return; };
        match key.code {
            KeyCode::Esc => { self.saved_tabs = None; }
            KeyCode::Up => { if view.selected > 0 { view.selected -= 1; } }
            KeyCode::Down => {
                let max = view.entries.len().saturating_sub(1);
                if view.selected < max { view.selected += 1; }
            }
            KeyCode::Enter => {
                if let Some(entry) = view.entries.get(view.selected).cloned() {
                    #[cfg(test)]
                    { self.last_restore_tab_id = Some(entry.id.clone()); }
                    self.dispatch_restore_tab(&entry.id);
                    self.saved_tabs = None;
                }
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                if let Some(entry) = view.entries.get(view.selected).cloned() {
                    crate::shell::wt_channel::cli_channel::spawn_wtcli_delete_saved_tab(&entry.id);
                    view.entries.remove(view.selected);
                    if view.selected >= view.entries.len() {
                        view.selected = view.entries.len().saturating_sub(1);
                    }
                }
            }
            _ => {}
        }
    }
```

Replace the Phase-A stub bodies of `cmd_save_tab` / `cmd_restore_tab`:

```rust
    fn cmd_save_tab(&mut self, title: String) {
        let title = title.trim().to_string();
        if title.is_empty() {
            let tab = self.current_tab_mut();
            tab.messages.push(ChatMessage::System(t!("commands.save_tab.needs_title").into_owned()));
            tab.scroll_to_bottom();
            return;
        }
        let Some(tab_id) = self.owner_tab_id.clone() else {
            let tab = self.current_tab_mut();
            tab.messages.push(ChatMessage::System(t!("commands.save_tab.no_tab").into_owned()));
            tab.scroll_to_bottom();
            return;
        };
        let tx = self.app_event_tx.clone();
        crate::shell::wt_channel::cli_channel::spawn_wtcli_save_tab(&tab_id, &title, Box::new(move |res| {
            let _ = tx.send(AppEvent::SavedTabResult(res));
        }));
    }

    fn cmd_restore_tab(&mut self) {
        self.saved_tabs = Some(SavedTabsViewState { loading: true, ..Default::default() });
        let tx = self.app_event_tx.clone();
        crate::shell::wt_channel::cli_channel::spawn_wtcli_list_saved_tabs(Box::new(move |rows| {
            let _ = tx.send(AppEvent::SavedTabsListed(rows));
        }));
    }

    fn dispatch_restore_tab(&mut self, id: &str) {
        let tx = self.app_event_tx.clone();
        crate::shell::wt_channel::cli_channel::spawn_wtcli_restore_tab(id, Box::new(move |res| {
            let _ = tx.send(AppEvent::RestoredTabResult(res));
        }));
    }
```

> `self.app_event_tx` — use the App's existing event sender. Grep for how other
> background callbacks post `AppEvent`s (e.g. the master-ext or restart channels)
> and reuse that exact sender/field name.

Handle the new AppEvents where AppEvents are processed (grep `AppEvent::` match arm):

```rust
            AppEvent::SavedTabResult(res) => {
                let msg = match res {
                    Ok(title) => t!("commands.save_tab.saved", title = title.as_str()).into_owned(),
                    Err(e) => t!("commands.save_tab.failed", error = e.as_str()).into_owned(),
                };
                let tab = self.current_tab_mut();
                tab.messages.push(ChatMessage::System(msg));
                tab.scroll_to_bottom();
            }
            AppEvent::SavedTabsListed(rows) => {
                if let Some(view) = self.saved_tabs.as_mut() {
                    view.entries = rows;
                    view.loading = false;
                    view.selected = 0;
                }
            }
            AppEvent::RestoredTabResult(res) => {
                let msg = match res {
                    Ok(outcome) if outcome == "focused" => t!("commands.restore_tab.focused").into_owned(),
                    Ok(_) => t!("commands.restore_tab.opened").into_owned(),
                    Err(e) => t!("commands.restore_tab.failed", error = e.as_str()).into_owned(),
                };
                let tab = self.current_tab_mut();
                tab.messages.push(ChatMessage::System(msg));
                tab.scroll_to_bottom();
            }
            AppEvent::SavedTabDeleted(_id) => { /* row already removed optimistically */ }
```

Add the intercept in `handle_key` (near the top, mirroring existing overlay intercepts):

```rust
        if self.saved_tabs.is_some() {
            self.handle_saved_tabs_key(key);
            return;
        }
```

Add the locale strings to `tools/wta/locales/en-US.yml` (+ other locales):

```yaml
commands:
  save_tab:
    needs_title: "Please provide a title: /save-tab <name>"
    no_tab: "Cannot determine this tab. Save is unavailable."
    saved: "Saved this tab as \"%{title}\". Restore it later with /restore-tab."
    failed: "Save failed: %{error}"
  restore_tab:
    focused: "Switched to the original tab."
    opened: "Restored in a new tab."
    failed: "Restore failed: %{error}"
    empty: "No saved tabs yet."
```

- [ ] **Step 5: Run tests to verify they pass**

Run (vcvars64): `cargo test --manifest-path tools/wta/Cargo.toml saved_tabs_`
Expected: PASS.

- [ ] **Step 6: Commit**

```powershell
git add tools/wta/src/app.rs tools/wta/src/shell/wt_channel/cli_channel.rs tools/wta/locales
git commit -m "wta: implement /save-tab + /restore-tab dispatch and picker state"
```

### Task E3: Render the picker

**Files:**
- Create: `tools/wta/src/ui/saved_tabs_view.rs`
- Modify: `tools/wta/src/ui/mod.rs` (add `pub mod saved_tabs_view;`)
- Modify: the main render dispatch (grep where `agents_view::render` is called in `ui/layout.rs`) to render `saved_tabs_view` when `app.saved_tabs.is_some()`.

- [ ] **Step 1: Write the render module**

```rust
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::app::SavedTabsViewState;

const ACCENT_CYAN: Color = Color::Cyan;
const MUTED: Color = Color::Rgb(0x8b, 0x8b, 0x8b);

/// Render the saved-tab picker: one row per snapshot, `>` marks the cursor.
pub fn render(f: &mut Frame, area: Rect, view: &SavedTabsViewState) {
    if view.loading {
        f.render_widget(Paragraph::new("Loading saved tabs…").style(Style::default().fg(MUTED)), area);
        return;
    }
    if view.entries.is_empty() {
        f.render_widget(Paragraph::new("No saved tabs yet.").style(Style::default().fg(MUTED)), area);
        return;
    }
    let items: Vec<ListItem> = view
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let marker = if i == view.selected { "> " } else { "  " };
            let open = if e.is_open { "  (open)" } else { "" };
            let mut style = Style::default();
            if i == view.selected {
                style = style.fg(ACCENT_CYAN).add_modifier(Modifier::BOLD);
            }
            ListItem::new(Line::from(vec![
                Span::styled(format!("{marker}{}", e.title), style),
                Span::styled(open, Style::default().fg(MUTED)),
            ]))
        })
        .collect();
    let mut state = ListState::default();
    state.select(Some(view.selected));
    f.render_stateful_widget(List::new(items), area, &mut state);

    // Footer hint on the bottom row.
    let hint = Rect { x: area.x, y: area.y + area.height.saturating_sub(1), width: area.width, height: 1 };
    f.render_widget(
        Paragraph::new("↑/↓ select · Enter restore · D delete · Esc close").style(Style::default().fg(MUTED)),
        hint,
    );
}
```

- [ ] **Step 2: Wire it into the layout**

In `ui/mod.rs` add `pub mod saved_tabs_view;`. In the render entry (`ui/layout.rs`, where `agents_view::render(...)` is dispatched based on view state), add a branch:

```rust
    if let Some(view) = app.saved_tabs.as_ref() {
        crate::ui::saved_tabs_view::render(f, main_area, view);
        return;
    }
```

(Place it with the other full-pane overlay branches so it replaces the chat area while open.)

- [ ] **Step 3: Build + test**

Run (vcvars64): `cargo test --manifest-path tools/wta/Cargo.toml`
Expected: PASS (whole suite).

- [ ] **Step 4: Commit**

```powershell
git add tools/wta/src/ui/saved_tabs_view.rs tools/wta/src/ui/mod.rs tools/wta/src/ui/layout.rs
git commit -m "wta/ui: render the saved-tab picker"
```

---

# Phase F — End-to-end test + manual verification

### Task F1: ItE2E coverage

**Files:**
- Create: `test/e2e/tests/Feature.EternalTerminal.Tests.ps1`

- [ ] **Step 1: Write the test**

Mirror an existing agent-pane feature test (e.g. `Feature.AgentPaneInteraction.Tests.ps1`). Gate on `winapp` availability (per repo convention). The test must:
1. `Set-WtSetting experimental.eternalTerminal.enabled $true` and start Terminal.
2. Open the agent pane; type `/` and assert `save-tab` appears in the popup (locale-robust regex via `Get-WtaLocalizedTextRegex 'commands.save_tab.summary'`).
3. Send `/save-tab e2e-snapshot` + Enter; assert the "Saved this tab as" system message (locale-robust).
4. `wtcli list-saved-tabs` (via `Get-WtCli`) returns a row with `title == "e2e-snapshot"`.
5. Send `/restore-tab`; assert the picker shows `e2e-snapshot`; press Enter; assert tab count increased OR focus outcome (per whether the source tab is still open).
6. Cleanup: `wtcli delete-saved-tab -i <id>`; assert `list-saved-tabs` no longer contains it.

```powershell
Describe 'Eternal Terminal — save/restore tab' {
    BeforeAll {
        if (-not (Get-Command winapp -ErrorAction SilentlyContinue)) { throw 'winapp required' }
        # ... Start-Terminal with experimental.eternalTerminal.enabled = $true
    }
    It 'saves a tab and lists it' {
        # send /save-tab, then assert list-saved-tabs contains the title
    }
    It 'restores a saved tab' {
        # open picker, Enter, assert outcome
    }
    AfterAll { # delete snapshot + Stop-Terminal }
}
```

- [ ] **Step 2: Run the E2E suite**

Run the ItE2E harness per repo docs for this single Describe.
Expected: PASS.

- [ ] **Step 3: Commit**

```powershell
git add test/e2e/tests/Feature.EternalTerminal.Tests.ps1
git commit -m "e2e: cover /save-tab + /restore-tab end to end"
```

### Task F2: Manual smoke (F5)

- [ ] Build wta (vcvars64) then C++ (`bcz no_clean`), F5 `CascadiaPackage`.
- [ ] With `experimental.eternalTerminal.enabled: true`, open agent pane, `/` shows `save-tab`/`restore-tab`; with it `false`, they are absent.
- [ ] `/save-tab hello`, run some commands to fill scrollback, close the tab, `/restore-tab` → Enter → new tab restores cwd + splits + scrollback content.
- [ ] Re-`/save-tab` on an open restored/original tab twice → only one snapshot per source tab in `wtcli list-saved-tabs`.
- [ ] `/restore-tab` on a snapshot whose source tab is still open → focuses it (no duplicate).
- [ ] Picker `D` removes the row and the `SavedTabSessions\{id}` folder.

---

## Self-review notes (author)

- **Spec coverage:** `/save-tab` (A5/E2), `/restore-tab` picker (E2/E3), title input inline (E2), overwrite-by-StableId (B3 `UpsertSavedTabSession`), content save (C2 `PersistTo` to folder) + restore (C4 stage-into-root), focus-if-open (C4 `_FindTabByStableId` + `_SelectTab`), gating (A1/A2/A4/A5), separate discoverable folder (C1 `_SavedTabSessionDir`), `D` delete (E2/C5). All covered.
- **Open verification items flagged inline (must confirm during impl, not invent):** the tab leaf-pane enumerator (C2 note), `_SelectTab` overload (C4 note), `_bstrFromHstring`/existing BSTR helper (D2 note), the App event-sender field name and `new_for_test` constructor (E2 note), the exact `commands::matches(` call site feeding the popup (A5). Each note says where to look.
- **Deferred (not in this plan, per design doc):** agent-pane restore, workspaces, crash-recovery integration, restored-tab rebinding.
