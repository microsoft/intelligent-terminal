use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::app::AppEvent;

pub async fn read_crossterm_events(tx: mpsc::UnboundedSender<AppEvent>) {
    let mut reader = EventStream::new();
    while let Some(Ok(event)) = reader.next().await {
        let app_event = match event {
            Event::Key(key) if key.kind == crossterm::event::KeyEventKind::Press => {
                AppEvent::Key(key)
            }
            Event::Resize(w, h) => AppEvent::Resize(w, h),
            _ => continue,
        };
        if tx.send(app_event).is_err() {
            break;
        }
    }
}
