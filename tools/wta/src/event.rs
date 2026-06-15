use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::time::{self, Duration, MissedTickBehavior};

use crate::app::AppEvent;

/// Maximum wait between bytes of a single CSI escape sequence. Real CSI
/// sequences arrive sub-millisecond; user keystrokes (Esc, then later
/// typing `[`) are tens of ms apart. 30ms cleanly disambiguates without
/// adding perceptible latency to a bare Esc press.
const CSI_TIMEOUT: Duration = Duration::from_millis(30);

/// Partial-sequence collector. When conpty hands us a raw VT escape
/// sequence as separate `Esc` + `Char('[')` + `Char(final)` events
/// instead of a parsed `KeyCode::Left`/etc., we hold the partial in
/// this state and combine on arrival of the final byte (or flush on
/// timeout / unexpected event).
#[derive(Debug)]
enum CsiState {
    /// No partial sequence in flight.
    Idle,
    /// Saw an Esc, waiting for `[` (or timeout → emit real Esc).
    Esc { since: Instant },
    /// Saw `Esc [`, waiting for the final byte (or timeout → emit Esc
    /// then `[` as separate keys).
    Bracket { since: Instant },
}

impl CsiState {
    fn pending_since(&self) -> Option<Instant> {
        match self {
            CsiState::Idle => None,
            CsiState::Esc { since } | CsiState::Bracket { since } => Some(*since),
        }
    }
}

/// Decode a CSI final byte into the corresponding `KeyCode`. Returns
/// `None` for unsupported sequences; callers flush the partial as raw
/// keys in that case so behavior degrades to "type the chars" rather
/// than swallow input.
fn decode_csi_final(c: char) -> Option<KeyCode> {
    match c {
        'A' => Some(KeyCode::Up),
        'B' => Some(KeyCode::Down),
        'C' => Some(KeyCode::Right),
        'D' => Some(KeyCode::Left),
        'H' => Some(KeyCode::Home),
        'F' => Some(KeyCode::End),
        _ => None,
    }
}

fn make_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

pub async fn read_crossterm_events(tx: mpsc::UnboundedSender<AppEvent>) {
    let mut reader = EventStream::new();
    let mut ticker = time::interval(Duration::from_millis(120));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // Separate, higher-frequency ticker (~30fps) that drives only the
    // typewriter reveal animation (`AppEvent::RevealTick`). Kept distinct from
    // the 120ms spinner `Tick` so the reveal can run smoothly without
    // quadrupling spinner full-frame flushes — a `RevealTick` only forces a
    // redraw when there is unrevealed pending text (see
    // `App::event_requires_redraw` / `has_reveal_backlog`).
    let mut reveal_ticker = time::interval(Duration::from_millis(33));
    reveal_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    tracing::info!(target: "input", "crossterm reader task starting");
    let mut consecutive_errors = 0usize;
    let mut csi = CsiState::Idle;

    // Helper: emit a key event to the app channel; returns false if the
    // channel is closed so the caller can break the loop.
    let send_key = |tx: &mpsc::UnboundedSender<AppEvent>, key: KeyEvent| -> bool {
        tracing::trace!(
            target: "input",
            code = ?key.code,
            mods = ?key.modifiers,
            "key dispatched",
        );
        tx.send(AppEvent::Key(key)).is_ok()
    };

    loop {
        // Compute the CSI flush deadline. Only Some when we have a
        // partial sequence in flight; otherwise the select! branch is
        // disabled and we wait normally.
        let csi_deadline = csi
            .pending_since()
            .map(|since| since + CSI_TIMEOUT);

        tokio::select! {
            _ = ticker.tick() => {
                if tx.send(AppEvent::Tick).is_err() {
                    tracing::info!(target: "input", "crossterm reader exiting: AppEvent channel closed");
                    break;
                }
            }
            _ = reveal_ticker.tick() => {
                if tx.send(AppEvent::RevealTick).is_err() {
                    tracing::info!(target: "input", "crossterm reader exiting: AppEvent channel closed");
                    break;
                }
            }
            _ = async {
                if let Some(deadline) = csi_deadline {
                    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
                } else {
                    std::future::pending::<()>().await;
                }
            }, if csi_deadline.is_some() => {
                // Partial-sequence timed out — user pressed bare Esc (and
                // maybe `[` afterwards as a real keystroke). Flush.
                match std::mem::replace(&mut csi, CsiState::Idle) {
                    CsiState::Esc { .. } => {
                        if !send_key(&tx, make_key(KeyCode::Esc)) { break; }
                    }
                    CsiState::Bracket { .. } => {
                        if !send_key(&tx, make_key(KeyCode::Esc)) { break; }
                        if !send_key(&tx, make_key(KeyCode::Char('['))) { break; }
                    }
                    CsiState::Idle => {}
                }
            }
            maybe_event = reader.next() => {
                let event = match maybe_event {
                    Some(Ok(e)) => {
                        consecutive_errors = 0;
                        e
                    }
                    Some(Err(e)) => {
                        // ConPTY can return transient read errors when the
                        // hosting pane is hidden/restored, when the OS swaps
                        // the underlying pseudo-console buffer, or under
                        // resource pressure. Historically we used to break
                        // out of the loop on the very first error — that
                        // killed both the ticker and the keyboard reader,
                        // so the TUI kept rendering on WT-pipe events but
                        // never saw another keypress (Up/Down/Ctrl+Shift+/ all dead).
                        // Instead, log and keep going. If we ever see a
                        // sustained burst of errors, drop the EventStream
                        // and rebuild it; that resyncs against the current
                        // input handle if Windows recycled it.
                        consecutive_errors += 1;
                        tracing::warn!(
                            target: "input",
                            error = %e,
                            consecutive = consecutive_errors,
                            "crossterm read error, continuing",
                        );
                        if consecutive_errors >= 8 {
                            tracing::warn!(
                                target: "input",
                                "rebuilding EventStream after sustained read errors",
                            );
                            reader = EventStream::new();
                            consecutive_errors = 0;
                        }
                        continue;
                    }
                    None => {
                        // Real EOF on stdin — only legitimate exit path.
                        tracing::info!(target: "input", "crossterm reader EOF, exiting");
                        break;
                    }
                };

                // ─── Normalization layer ─────────────────────────────────
                // conpty in this build (both shipped IntelligentTerminal
                // and the dev sideload) hands us raw VT bytes for keys
                // crossterm should parse natively: Backspace as
                // `Char('\u{7f}')`, arrow keys as `Esc` + `Char('[')` +
                // `Char('A'|'B'|'C'|'D')`, etc. We rewrite those at the
                // boundary so every downstream handler sees the normal
                // crossterm KeyCodes (Backspace, Up, Down, Left, Right,
                // Home, End) without each remembering the quirk.
                //
                // Backspace + Ctrl+H normalization is unconditional (no
                // state). CSI sequences need a 3-event state machine
                // because `Esc` arrives one event before `[` which
                // arrives one event before the final byte. CSI_TIMEOUT
                // bounds how long we hold a partial — a real bare Esc
                // press flushes after 30ms, which is below user
                // perception while well above the inter-byte gap of a
                // genuine conpty CSI write.

                let key_event = if let Event::Key(mut key) = event {
                    if key.kind != crossterm::event::KeyEventKind::Press {
                        continue;
                    }
                    // Static byte rewrites (no state).
                    match (key.code, key.modifiers) {
                        (KeyCode::Char('\u{7f}'), _) | (KeyCode::Char('\u{8}'), _) => {
                            key.code = KeyCode::Backspace;
                        }
                        _ => {}
                    }
                    key
                } else {
                    // Non-key events flush any partial CSI as raw, then
                    // emit the event itself. Resize/focus changes are
                    // rare enough that the flush isn't user-visible.
                    match std::mem::replace(&mut csi, CsiState::Idle) {
                        CsiState::Esc { .. } => {
                            if !send_key(&tx, make_key(KeyCode::Esc)) { break; }
                        }
                        CsiState::Bracket { .. } => {
                            if !send_key(&tx, make_key(KeyCode::Esc)) { break; }
                            if !send_key(&tx, make_key(KeyCode::Char('['))) { break; }
                        }
                        CsiState::Idle => {}
                    }
                    let app_event = match event {
                        Event::Resize(w, h) => AppEvent::Resize(w, h),
                        // WT/conpty forwards xterm focus-in/out (CSI I / CSI O)
                        // to the child unconditionally when the hosting TermControl
                        // gains/loses XAML focus — one event per pane, not per
                        // window. Used to hide the input cursor when the agent
                        // pane is not the focused pane.
                        Event::FocusGained => AppEvent::FocusChanged(true),
                        Event::FocusLost => AppEvent::FocusChanged(false),
                        _ => continue,
                    };
                    if tx.send(app_event).is_err() {
                        tracing::info!(target: "input", "crossterm reader exiting: AppEvent channel closed");
                        break;
                    }
                    continue;
                };

                // CSI state machine.
                let now = Instant::now();
                let next_csi = match (&csi, key_event.code, key_event.modifiers) {
                    // Start CSI: bare Esc (no modifiers) opens the window.
                    (CsiState::Idle, KeyCode::Esc, mods) if mods.is_empty() => {
                        csi = CsiState::Esc { since: now };
                        continue;
                    }
                    // Esc + `[` → expect CSI final byte.
                    (CsiState::Esc { .. }, KeyCode::Char('['), mods) if mods.is_empty() => {
                        csi = CsiState::Bracket { since: now };
                        continue;
                    }
                    // Esc + `[` + recognized final → emit the decoded key.
                    (CsiState::Bracket { .. }, KeyCode::Char(c), mods)
                        if mods.is_empty() && decode_csi_final(c).is_some() =>
                    {
                        let kc = decode_csi_final(c).unwrap();
                        csi = CsiState::Idle;
                        if !send_key(&tx, make_key(kc)) { break; }
                        continue;
                    }
                    // Esc + unexpected next key → flush Esc as a real Esc
                    // press, then fall through to process the new event
                    // normally (the user pressed Esc then immediately
                    // pressed something other than `[`).
                    (CsiState::Esc { .. }, _, _) => {
                        csi = CsiState::Idle;
                        if !send_key(&tx, make_key(KeyCode::Esc)) { break; }
                        Some(key_event)
                    }
                    // Esc + `[` + unrecognized → flush as raw, then
                    // process the new event normally.
                    (CsiState::Bracket { .. }, _, _) => {
                        csi = CsiState::Idle;
                        if !send_key(&tx, make_key(KeyCode::Esc)) { break; }
                        if !send_key(&tx, make_key(KeyCode::Char('['))) { break; }
                        Some(key_event)
                    }
                    _ => Some(key_event),
                };

                if let Some(key) = next_csi {
                    if !send_key(&tx, key) { break; }
                }
            }
        }
    }
}
