use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::time::{self, Duration, MissedTickBehavior};

use crate::app::AppEvent;

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

    loop {
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
                let app_event = match event {
                    Event::Key(key) if key.kind == crossterm::event::KeyEventKind::Press => {
                        tracing::trace!(
                            target: "input",
                            code = ?key.code,
                            mods = ?key.modifiers,
                            "key press received",
                        );
                        AppEvent::Key(key)
                    }
                    Event::Resize(w, h) => AppEvent::Resize(w, h),
                    // WT/conpty forwards xterm focus-in/out (CSI I / CSI O)
                    // to the child unconditionally when the hosting TermControl
                    // gains/loses XAML focus — one event per pane, not per
                    // window. Used to hide the input cursor when the agent
                    // pane is not the focused pane.
                    Event::FocusGained => AppEvent::FocusChanged(true),
                    Event::FocusLost => AppEvent::FocusChanged(false),
                    // We do not enable mouse capture (see main.rs run_acp_tui_mode).
                    // The terminal emulator translates wheel into Up/Down arrow
                    // keystrokes in alt-screen mode, so we never observe raw
                    // Event::Mouse here. Drop anything else (Paste, etc.).
                    _ => continue,
                };
                if tx.send(app_event).is_err() {
                    tracing::info!(target: "input", "crossterm reader exiting: AppEvent channel closed");
                    break;
                }
            }
        }
    }
}
