use crossterm::event::{Event, EventStream, MouseEventKind};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::time::{self, Duration, MissedTickBehavior};

use crate::app::AppEvent;

pub async fn read_crossterm_events(tx: mpsc::UnboundedSender<AppEvent>) {
    let mut reader = EventStream::new();
    let mut ticker = time::interval(Duration::from_millis(120));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

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
                        // never saw another keypress (Up/Down/F2 all dead).
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
                    Event::Mouse(mouse) => {
                        // Trace mouse activity so we can diagnose "frozen pane"
                        // reports — e.g. shift+drag in WT triggers native text
                        // selection (xterm convention: shift overrides app
                        // mouse capture so users can still copy text), and
                        // until that selection is dismissed (Esc / unmodified
                        // click) WT may swallow keystrokes before they reach
                        // crossterm. If you see drag events in the log but no
                        // subsequent key events, that's the selection-mode
                        // signature.
                        tracing::trace!(
                            target: "input",
                            kind = ?mouse.kind,
                            mods = ?mouse.modifiers,
                            row = mouse.row,
                            col = mouse.column,
                            "mouse event",
                        );
                        match mouse.kind {
                            MouseEventKind::ScrollUp => AppEvent::MouseScroll { delta: -3, row: mouse.row },
                            MouseEventKind::ScrollDown => AppEvent::MouseScroll { delta: 3, row: mouse.row },
                            _ => continue,
                        }
                    }
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
