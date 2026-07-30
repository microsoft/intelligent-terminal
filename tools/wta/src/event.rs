use crossterm::event::{Event, EventStream, MouseButton, MouseEventKind};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::time::{self, Duration, MissedTickBehavior};

use crate::app_contracts::AppEvent;

/// Pure translation of a crossterm input `Event` into the `AppEvent` the TUI
/// consumes, or `None` for events we deliberately drop.
///
/// Load-bearing branch: only `KeyEventKind::Press` becomes an `AppEvent::Key`
/// — key *release* / *repeat* events (which conpty/Windows can deliver) must
/// be dropped; otherwise every keystroke would fire twice. Mouse events are
/// forwarded so the app can scroll its virtual chat viewport without stealing
/// Up/Down from prompt history. Paste and unsupported variants are dropped.
fn map_crossterm_event(event: Event) -> Option<AppEvent> {
    match event {
        Event::Key(key) if key.kind == crossterm::event::KeyEventKind::Press => {
            Some(AppEvent::Key(key))
        }
        Event::Mouse(mouse)
            if matches!(
                mouse.kind,
                MouseEventKind::ScrollUp
                    | MouseEventKind::ScrollDown
                    | MouseEventKind::Down(MouseButton::Left)
                    | MouseEventKind::Drag(MouseButton::Left)
                    | MouseEventKind::Up(MouseButton::Left)
            ) =>
        {
            Some(AppEvent::Mouse(mouse))
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
                    // Drop paste, key release/repeat, and any other event the
                    // TUI does not consume.
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
    use crossterm::event::{
        KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
    };

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
    fn mouse_events_are_forwarded() {
        let mouse = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 4,
            row: 7,
            modifiers: KeyModifiers::NONE,
        };
        assert!(matches!(
            map_crossterm_event(Event::Mouse(mouse)),
            Some(AppEvent::Mouse(mapped)) if mapped == mouse
        ));
    }

    #[test]
    fn left_button_selection_events_are_forwarded() {
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            let mouse = MouseEvent {
                kind,
                column: 4,
                row: 7,
                modifiers: KeyModifiers::NONE,
            };
            assert!(matches!(
                map_crossterm_event(Event::Mouse(mouse)),
                Some(AppEvent::Mouse(mapped)) if mapped == mouse
            ));
        }
    }

    #[test]
    fn unsupported_mouse_and_paste_events_are_dropped() {
        let mouse = MouseEvent {
            kind: MouseEventKind::Moved,
            column: 4,
            row: 7,
            modifiers: KeyModifiers::NONE,
        };
        assert!(map_crossterm_event(Event::Mouse(mouse)).is_none());
        assert!(map_crossterm_event(Event::Paste("text".to_string())).is_none());
    }
}
