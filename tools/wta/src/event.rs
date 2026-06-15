use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::time::{self, Duration, MissedTickBehavior};

use crate::app::AppEvent;

/// Pure translation of a crossterm input `Event` into the `AppEvent` the TUI
/// consumes, or `None` for events we deliberately drop.
///
/// Load-bearing branch: only `KeyEventKind::Press` becomes an `AppEvent::Key`
/// — key *release* / *repeat* events (which conpty/Windows can deliver) must
/// be dropped; otherwise every keystroke would fire twice. Paste, Mouse, and
/// any other variant are dropped (we never enable mouse capture; the emulator
/// translates wheel into arrow keys in alt-screen mode).
fn map_crossterm_event(event: Event) -> Option<AppEvent> {
    match event {
        Event::Key(key) if key.kind == crossterm::event::KeyEventKind::Press => {
            Some(AppEvent::Key(key))
        }
        Event::Resize(w, h) => Some(AppEvent::Resize(w, h)),
        // WT/conpty forwards xterm focus-in/out (CSI I / CSI O) to the child
        // when the hosting TermControl gains/loses XAML focus — one event per
        // pane. Used to hide the input cursor when the agent pane is unfocused.
        Event::FocusGained => Some(AppEvent::FocusChanged(true)),
        Event::FocusLost => Some(AppEvent::FocusChanged(false)),
        _ => None,
    }
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
                let app_event = match map_crossterm_event(event) {
                    Some(ev) => ev,
                    // We do not enable mouse capture (see main.rs run_acp_tui_mode).
                    // The terminal emulator translates wheel into Up/Down arrow
                    // keystrokes in alt-screen mode, so we never observe raw
                    // Event::Mouse here. Drop anything else (Paste, key release, etc.).
                    None => continue,
                };
                if let AppEvent::Key(key) = &app_event {
                    tracing::trace!(
                        target: "input",
                        code = ?key.code,
                        mods = ?key.modifiers,
                        "key press received",
                    );
                }
                if tx.send(app_event).is_err() {
                    tracing::info!(target: "input", "crossterm reader exiting: AppEvent channel closed");
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    #[test]
    fn key_press_maps_to_key_event() {
        let press = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        // KeyEvent::new defaults kind to Press.
        assert!(matches!(
            map_crossterm_event(Event::Key(press)),
            Some(AppEvent::Key(_))
        ));
    }

    #[test]
    fn key_release_and_repeat_are_dropped() {
        // Only Press maps; release/repeat must be dropped to avoid double-fire.
        let mut release = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert!(map_crossterm_event(Event::Key(release)).is_none());

        let mut repeat = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        repeat.kind = KeyEventKind::Repeat;
        assert!(map_crossterm_event(Event::Key(repeat)).is_none());
    }

    #[test]
    fn resize_maps_with_dimensions() {
        assert!(matches!(
            map_crossterm_event(Event::Resize(120, 40)),
            Some(AppEvent::Resize(120, 40))
        ));
    }

    #[test]
    fn focus_in_out_map_to_focus_changed() {
        assert!(matches!(
            map_crossterm_event(Event::FocusGained),
            Some(AppEvent::FocusChanged(true))
        ));
        assert!(matches!(
            map_crossterm_event(Event::FocusLost),
            Some(AppEvent::FocusChanged(false))
        ));
    }

    #[test]
    fn paste_and_other_events_are_dropped() {
        assert!(map_crossterm_event(Event::Paste("text".to_string())).is_none());
    }
}
