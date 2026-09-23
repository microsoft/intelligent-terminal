// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::Arc,
    thread::JoinHandle,
};
use tokio::sync::mpsc;
use windows_sys::Win32::{
    Foundation::{WAIT_FAILED, WAIT_OBJECT_0},
    System::{
        Console::{
            GetConsoleMode, ReadConsoleInputW, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_INPUT,
            ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT, FOCUS_EVENT, INPUT_RECORD,
            KEY_EVENT, KEY_EVENT_RECORD, KEY_EVENT_RECORD_0, LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED,
            RIGHT_ALT_PRESSED, RIGHT_CTRL_PRESSED, SHIFT_PRESSED, WINDOW_BUFFER_SIZE_EVENT,
        },
        Threading::{CreateEventW, SetEvent, WaitForMultipleObjects, INFINITE},
    },
};

/// Console-only input owner. Do not run Crossterm's Windows reader concurrently:
/// it consumes paste delimiters as keys and pairs surrogate key-up records.
pub(in crate::agent_center) struct ConsoleInput {
    console: Arc<File>,
    original_mode: u32,
    stop: OwnedHandle,
    worker: Option<JoinHandle<()>>,
    events: mpsc::UnboundedReceiver<io::Result<Event>>,
    keyboard_mode: Option<KeyboardMode>,
}

struct KeyboardMode {
    output: File,
    original_mode: u32,
}

impl KeyboardMode {
    fn enable() -> io::Result<Self> {
        let output = OpenOptions::new().read(true).write(true).open("CONOUT$")?;
        let mut original_mode = 0;
        unsafe {
            if GetConsoleMode(output.as_raw_handle(), &mut original_mode) == 0
                || SetConsoleMode(
                    output.as_raw_handle(),
                    original_mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
                ) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        let mut mode = Self {
            output,
            original_mode,
        };
        // VT input otherwise re-encodes physical Enter/Escape as ambiguous text,
        // losing modifiers. Request lossless INPUT_RECORD packets from ConPTY.
        mode.output.write_all(b"\x1b[?9001h")?;
        mode.output.flush()?;
        Ok(mode)
    }
}

impl Drop for KeyboardMode {
    fn drop(&mut self) {
        if let Err(error) = self
            .output
            .write_all(b"\x1b[?9001l")
            .and_then(|()| self.output.flush())
        {
            tracing::warn!(%error, "cannot disable Console Win32 keyboard mode");
        }
        unsafe {
            if SetConsoleMode(self.output.as_raw_handle(), self.original_mode) == 0 {
                tracing::warn!(error = %io::Error::last_os_error(), "cannot restore Console output mode");
            }
        }
    }
}

impl ConsoleInput {
    pub fn new() -> io::Result<Self> {
        let console = Arc::new(OpenOptions::new().read(true).write(true).open("CONIN$")?);
        let mut original_mode = 0;
        // The file and owned event handles outlive every native call and the reader thread.
        let stop = unsafe {
            if GetConsoleMode(console.as_raw_handle(), &mut original_mode) == 0 {
                return Err(io::Error::last_os_error());
            }
            let event = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
            if event.is_null() {
                return Err(io::Error::last_os_error());
            }
            OwnedHandle::from_raw_handle(event)
        };
        let worker_stop = stop.try_clone()?;
        let worker_console = console.clone();
        let (sender, events) = mpsc::unbounded_channel();
        unsafe {
            // Without VT input, ConPTY consumes CSI 200~/201~ before ReadConsoleInputW.
            if SetConsoleMode(
                console.as_raw_handle(),
                original_mode | ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_WINDOW_INPUT,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        let mut input = Self {
            console,
            original_mode,
            stop,
            worker: None,
            events,
            keyboard_mode: None,
        };
        input.keyboard_mode = Some(KeyboardMode::enable()?);
        input.worker = Some(
            std::thread::Builder::new()
                .name("center-input".into())
                .spawn(move || {
                    if let Err(error) = read_records(&worker_console, &worker_stop, &sender) {
                        let _ = sender.send(Err(error));
                    }
                })?,
        );
        Ok(input)
    }

    pub async fn next(&mut self) -> Option<io::Result<Event>> {
        self.events.recv().await
    }
}

impl Drop for ConsoleInput {
    fn drop(&mut self) {
        unsafe {
            if SetEvent(self.stop.as_raw_handle()) == 0 {
                tracing::error!(error = %io::Error::last_os_error(), "cannot stop Console input reader");
            }
        }
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                tracing::error!("Console input reader panicked");
            }
            self.keyboard_mode.take();
        }
        unsafe {
            if SetConsoleMode(self.console.as_raw_handle(), self.original_mode) == 0 {
                tracing::warn!(error = %io::Error::last_os_error(), "cannot restore Console input mode");
            }
        }
    }
}

fn read_records(
    console: &File,
    stop: &OwnedHandle,
    sender: &mpsc::UnboundedSender<io::Result<Event>>,
) -> io::Result<()> {
    let mut decoder = Decoder::default();
    let handles = [stop.as_raw_handle(), console.as_raw_handle()];
    loop {
        // This is the sole reader; a signaled console has records ready. The stop
        // event wakes an idle reader without injecting keys or cancelling another process.
        let record = unsafe {
            match WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) {
                WAIT_OBJECT_0 => return Ok(()),
                WAIT_FAILED => return Err(io::Error::last_os_error()),
                value if value == WAIT_OBJECT_0 + 1 => {}
                _ => return Err(io::Error::other("unexpected Console input wait result")),
            }
            let mut record: INPUT_RECORD = std::mem::zeroed();
            let mut count = 0;
            if ReadConsoleInputW(console.as_raw_handle(), &mut record, 1, &mut count) == 0 {
                return Err(io::Error::last_os_error());
            }
            if count == 0 {
                continue;
            }
            record
        };
        for event in decoder.record(&record)? {
            if sender.send(Ok(event)).is_err() {
                return Ok(());
            }
        }
    }
}

#[derive(Default)]
struct Decoder {
    high_surrogate: Option<u16>,
    packet_surrogate: Option<u16>,
    packet_escape: String,
    escape: String,
    paste: Option<String>,
}

impl Decoder {
    fn record(&mut self, record: &INPUT_RECORD) -> io::Result<Vec<Event>> {
        // EventType selects the initialized member of the Win32 tagged union.
        unsafe {
            match record.EventType as u32 {
                KEY_EVENT => self.key(&record.Event.KeyEvent, false),
                WINDOW_BUFFER_SIZE_EVENT => {
                    let size = record.Event.WindowBufferSizeEvent.dwSize;
                    Ok(vec![Event::Resize(
                        size.X.max(0) as u16,
                        size.Y.max(0) as u16,
                    )])
                }
                FOCUS_EVENT => Ok(vec![if record.Event.FocusEvent.bSetFocus != 0 {
                    Event::FocusGained
                } else {
                    Event::FocusLost
                }]),
                _ => Ok(vec![]),
            }
        }
    }

    fn key(&mut self, key: &KEY_EVENT_RECORD, packet: bool) -> io::Result<Vec<Event>> {
        let unit = unsafe { key.uChar.UnicodeChar };
        // Older ConPTY hosts and Windows Alt-code entry deliver text on the
        // Alt key release. Ordinary key-up records must still be discarded.
        let alt_code = key.wVirtualKeyCode == 0x12 && key.bKeyDown == 0 && unit != 0;
        if (key.bKeyDown == 0 && !alt_code) || key.wRepeatCount == 0 {
            return Ok(vec![]);
        }
        // ASCII packet framing must not disturb a surrogate buffered from a
        // decoded packet. Raw paste text has a separate UTF-16 stream.
        let high_surrogate = if packet {
            &mut self.packet_surrogate
        } else {
            &mut self.high_surrogate
        };
        let character = match unit {
            0xd800..=0xdbff => {
                if high_surrogate.replace(unit).is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unpaired Console high surrogate",
                    ));
                }
                return Ok(vec![]);
            }
            0xdc00..=0xdfff => {
                let high = high_surrogate.take().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "unpaired Console low surrogate")
                })?;
                char::from_u32(0x10000 + ((high as u32 - 0xd800) << 10) + unit as u32 - 0xdc00)
            }
            _ => {
                // Modifier key records may occur between UTF-16 halves.
                if unit != 0 && high_surrogate.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "incomplete Console surrogate pair",
                    ));
                }
                char::from_u32(unit as u32)
            }
        };
        let mut modifiers = KeyModifiers::empty();
        if key.dwControlKeyState & SHIFT_PRESSED != 0 {
            modifiers |= KeyModifiers::SHIFT;
        }
        if key.dwControlKeyState & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0 {
            modifiers |= KeyModifiers::CONTROL;
        }
        if key.dwControlKeyState & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0 {
            modifiers |= KeyModifiers::ALT;
        }
        if alt_code {
            modifiers.remove(KeyModifiers::ALT);
        }
        if (0x60..=0x69).contains(&key.wVirtualKeyCode)
            && modifiers.contains(KeyModifiers::ALT)
            && !modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL)
        {
            return Ok(vec![]);
        }
        let mut events = Vec::new();
        for _ in 0..key.wRepeatCount {
            if !packet && key.wVirtualKeyCode == 0 {
                if let Some(ch) = character.filter(|ch| *ch != '\0') {
                    self.wire(ch, &mut events)?;
                }
                continue;
            }
            // Vk=0 is text, including paste delimiters re-encoded by older
            // ConPTY hosts. A physical Escape has Vk=27 and is never buffered.
            if self.paste.is_some() || !self.escape.is_empty() || key.wVirtualKeyCode == 0 {
                if let Some(ch) = character.filter(|ch| *ch != '\0') {
                    self.raw(ch, &mut events)?;
                }
                continue;
            }
            let code = match key.wVirtualKeyCode {
                0x08 => KeyCode::Backspace,
                0x09 if modifiers.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
                0x09 => KeyCode::Tab,
                0x0d => KeyCode::Enter,
                0x1b => KeyCode::Esc,
                0x21 => KeyCode::PageUp,
                0x22 => KeyCode::PageDown,
                0x23 => KeyCode::End,
                0x24 => KeyCode::Home,
                0x25 => KeyCode::Left,
                0x26 => KeyCode::Up,
                0x27 => KeyCode::Right,
                0x28 => KeyCode::Down,
                0x2d => KeyCode::Insert,
                0x2e => KeyCode::Delete,
                0x70..=0x87 => KeyCode::F((key.wVirtualKeyCode - 0x6f) as u8),
                0x12 if alt_code => {
                    let Some(ch) = character else { continue };
                    KeyCode::Char(ch)
                }
                0x10..=0x12 => continue,
                _ => {
                    let Some(ch) = character.filter(|ch| *ch != '\0') else {
                        continue;
                    };
                    if modifiers.contains(KeyModifiers::CONTROL)
                        && ch.is_control()
                        && (0x41..=0x5a).contains(&key.wVirtualKeyCode)
                    {
                        KeyCode::Char(char::from(key.wVirtualKeyCode as u8).to_ascii_lowercase())
                    } else {
                        if key.dwControlKeyState & (RIGHT_ALT_PRESSED | LEFT_CTRL_PRESSED)
                            == (RIGHT_ALT_PRESSED | LEFT_CTRL_PRESSED)
                            && !ch.is_control()
                        {
                            modifiers.remove(KeyModifiers::ALT | KeyModifiers::CONTROL);
                        }
                        KeyCode::Char(ch)
                    }
                }
            };
            events.push(Event::Key(KeyEvent::new(code, modifiers)));
        }
        Ok(events)
    }

    fn wire(&mut self, ch: char, events: &mut Vec<Event>) -> io::Result<()> {
        if self.packet_escape.is_empty() {
            if ch == '\x1b' {
                self.packet_escape.push(ch);
                return Ok(());
            }
            return self.raw(ch, events);
        }
        self.packet_escape.push(ch);
        if self.packet_escape == "\x1b[" || self.packet_escape == "\x1bO" {
            return Ok(());
        }
        if self.packet_escape.len() > 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Console input packet too long",
            ));
        }
        if ('@'..='~').contains(&ch) {
            let sequence = std::mem::take(&mut self.packet_escape);
            if ch == '_' {
                events.extend(self.key(&win32_key(&sequence)?, true)?);
            } else {
                for ch in sequence.chars() {
                    self.raw(ch, events)?;
                }
            }
        }
        Ok(())
    }

    fn raw(&mut self, ch: char, events: &mut Vec<Event>) -> io::Result<()> {
        const PASTE_END: &str = "\x1b[201~";
        const MAX_PASTE: usize = 4 * 1024 * 1024;
        if let Some(paste) = self.paste.as_mut() {
            paste.push(ch);
            if paste.ends_with(PASTE_END) {
                paste.truncate(paste.len() - PASTE_END.len());
                events.push(Event::Paste(self.paste.take().unwrap_or_default()));
            } else if paste.len() > MAX_PASTE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Console paste exceeds 4 MiB",
                ));
            }
            return Ok(());
        }
        if ch == '\x1b' && self.escape.is_empty() {
            self.escape.push(ch);
            return Ok(());
        }
        if !self.escape.is_empty() {
            self.escape.push(ch);
            if self.escape == "\x1b[" || self.escape == "\x1bO" {
                return Ok(());
            }
            if self.escape == "\x1b[200~" {
                self.escape.clear();
                self.paste = Some(String::new());
            } else if self.escape.len() > 64 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Console input escape sequence too long",
                ));
            } else if ('@'..='~').contains(&ch) {
                let sequence = std::mem::take(&mut self.escape);
                if let Some(key) = vt_key(&sequence) {
                    events.push(Event::Key(key));
                } else {
                    tracing::debug!("ignoring unsupported Console input escape sequence");
                }
            }
            return Ok(());
        }
        events.push(Event::Key(text_key(ch, KeyModifiers::NONE)));
        Ok(())
    }
}

fn text_key(ch: char, mut modifiers: KeyModifiers) -> KeyEvent {
    let code = match ch {
        '\r' | '\n' => KeyCode::Enter,
        '\x1b' => KeyCode::Esc,
        '\t' => KeyCode::Tab,
        '\x08' | '\x7f' => KeyCode::Backspace,
        '\x01'..='\x1a' => {
            modifiers |= KeyModifiers::CONTROL;
            KeyCode::Char(char::from(ch as u8 + b'a' - 1))
        }
        value => KeyCode::Char(value),
    };
    KeyEvent::new(code, modifiers)
}

fn win32_key(sequence: &str) -> io::Result<KEY_EVENT_RECORD> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid Console Win32 input packet",
        )
    };
    let body = sequence
        .strip_prefix("\x1b[")
        .and_then(|value| value.strip_suffix('_'))
        .ok_or_else(invalid)?;
    let mut fields = [0u32, 0, 0, 0, 0, 1];
    for (index, value) in body.split(';').enumerate() {
        let field = fields.get_mut(index).ok_or_else(invalid)?;
        if !value.is_empty() {
            *field = value.parse().map_err(|_| invalid())?;
        }
    }
    if fields[3] > 1 {
        return Err(invalid());
    }
    Ok(KEY_EVENT_RECORD {
        wVirtualKeyCode: fields[0].try_into().map_err(|_| invalid())?,
        wVirtualScanCode: fields[1].try_into().map_err(|_| invalid())?,
        uChar: KEY_EVENT_RECORD_0 {
            UnicodeChar: fields[2].try_into().map_err(|_| invalid())?,
        },
        bKeyDown: fields[3] as i32,
        dwControlKeyState: fields[4],
        wRepeatCount: fields[5].try_into().map_err(|_| invalid())?,
    })
}

fn vt_key(sequence: &str) -> Option<KeyEvent> {
    let body = sequence
        .strip_prefix("\x1b[")
        .or_else(|| sequence.strip_prefix("\x1bO"))?;
    let last = body.chars().last()?;
    let mut params = body[..body.len() - 1].split(';');
    let first = params.next().unwrap_or("");
    let modifier = params
        .next()
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(1);
    let mut modifiers = KeyModifiers::empty();
    let mask = modifier.saturating_sub(1);
    if mask & 1 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if mask & 2 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if mask & 4 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    let code = match last {
        'A' => KeyCode::Up,
        'B' => KeyCode::Down,
        'C' => KeyCode::Right,
        'D' => KeyCode::Left,
        'H' => KeyCode::Home,
        'F' => KeyCode::End,
        'P' => KeyCode::F(1),
        'Q' => KeyCode::F(2),
        'R' => KeyCode::F(3),
        'S' => KeyCode::F(4),
        'Z' => {
            modifiers |= KeyModifiers::SHIFT;
            KeyCode::BackTab
        }
        '~' => match first {
            "1" | "7" => KeyCode::Home,
            "2" => KeyCode::Insert,
            "3" => KeyCode::Delete,
            "4" | "8" => KeyCode::End,
            "5" => KeyCode::PageUp,
            "6" => KeyCode::PageDown,
            "11" => KeyCode::F(1),
            "12" => KeyCode::F(2),
            "13" => KeyCode::F(3),
            "14" => KeyCode::F(4),
            "15" => KeyCode::F(5),
            "17" => KeyCode::F(6),
            "18" => KeyCode::F(7),
            "19" => KeyCode::F(8),
            "20" => KeyCode::F(9),
            "21" => KeyCode::F(10),
            "23" => KeyCode::F(11),
            "24" => KeyCode::F(12),
            _ => return None,
        },
        _ => return None,
    };
    Some(KeyEvent::new(code, modifiers))
}

#[cfg(test)]
mod tests {
    use super::super::{handle_key, handle_paste, JobKind, Operation, PendingConfirmation, State};
    use super::*;

    fn record(unit: u16, down: bool, vk: u16, repeat: u16, modifiers: u32) -> INPUT_RECORD {
        let mut record: INPUT_RECORD = unsafe { std::mem::zeroed() };
        record.EventType = KEY_EVENT as u16;
        record.Event.KeyEvent = KEY_EVENT_RECORD {
            bKeyDown: i32::from(down),
            wRepeatCount: repeat,
            wVirtualKeyCode: vk,
            wVirtualScanCode: 0,
            uChar: windows_sys::Win32::System::Console::KEY_EVENT_RECORD_0 { UnicodeChar: unit },
            dwControlKeyState: modifiers,
        };
        record
    }

    fn records(text: &str) -> Vec<INPUT_RECORD> {
        text.encode_utf16()
            .flat_map(|unit| [record(unit, true, 0, 1, 0), record(unit, false, 0, 1, 0)])
            .collect()
    }

    fn packet(record: &INPUT_RECORD) -> String {
        let key = unsafe { record.Event.KeyEvent };
        format!(
            "\x1b[{};{};{};{};{};{}_",
            key.wVirtualKeyCode,
            key.wVirtualScanCode,
            unsafe { key.uChar.UnicodeChar },
            key.bKeyDown,
            key.dwControlKeyState,
            key.wRepeatCount
        )
    }

    fn assert_ui_key_semantics(event: &Event) {
        let _locale = crate::test_support::lock_locale();
        let Event::Key(key) = event else {
            panic!("expected physical key")
        };
        let mut state = State::new();
        state.task_list = false;
        state.select(Some("work-a".into()));
        handle_paste(&mut state, "first");
        state.editor_view_mut().editor.cursor = 2;
        state.editor_view_mut().editor.anchor = Some(0);
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        if key.modifiers == KeyModifiers::SHIFT {
            handle_key(&mut state, key.code, key.modifiers, &jobs).unwrap();
            assert_eq!(state.editor_view().unwrap().draft, "\nrst");
            assert!(
                receiver.try_recv().is_err(),
                "Shift+Enter must not dispatch"
            );
            return;
        }
        if key.modifiers == KeyModifiers::CONTROL || key.code == KeyCode::Esc {
            let operation = Operation {
                method: "workspace.takeover".into(),
                params: serde_json::json!({"workspaceId":"workspace-a"}),
                if_match: vec![
                    serde_json::json!({"kind":"Workspace","id":"workspace-a","version":7}),
                ],
                mutation: true,
                confirmation: true,
                command_id: "frozen-round10".into(),
            };
            state.pending = Some(PendingConfirmation {
                operation: operation.clone(),
                preview: serde_json::json!({}),
                work: Some("work-a".into()),
                input: "first".into(),
                scroll: 0,
                target_label: "work-a".into(),
                origin: super::super::MutationOrigin::Action,
            });
            handle_key(&mut state, key.code, key.modifiers, &jobs).unwrap();
            assert_eq!(state.editor_view().unwrap().draft, "first");
            assert_eq!(state.editor_view().unwrap().editor.selection(), 0..2);
            if key.code == KeyCode::Esc {
                assert!(state.pending.is_none());
                assert!(receiver.try_recv().is_err(), "Escape must only cancel");
            } else {
                let job = receiver.try_recv().expect("Ctrl+Enter must dispatch");
                assert_eq!(job.work.as_deref(), Some("work-a"));
                let JobKind::Send(sent) = job.kind else {
                    panic!("expected frozen operation")
                };
                assert_eq!(sent.command_id, operation.command_id);
                assert_eq!(sent.params, operation.params);
                assert_eq!(sent.if_match, operation.if_match);
                assert!(receiver.try_recv().is_err(), "only one confirmation");
            }
        } else {
            state
                .editor_view_mut()
                .replace_draft("/round10-invalid".into());
            assert!(
                handle_key(&mut state, key.code, key.modifiers, &jobs).is_err(),
                "ordinary Enter still parses the command"
            );
        }
    }

    #[test]
    fn win32_packets_keep_keyboard_semantics_and_separate_paste_framing() {
        for (vk, unit, flags, code, modifiers) in [
            (13, 13, SHIFT_PRESSED, KeyCode::Enter, KeyModifiers::SHIFT),
            (
                13,
                13,
                LEFT_CTRL_PRESSED,
                KeyCode::Enter,
                KeyModifiers::CONTROL,
            ),
            (27, 27, 0, KeyCode::Esc, KeyModifiers::NONE),
            (13, 13, 0, KeyCode::Enter, KeyModifiers::NONE),
        ] {
            let mut decoder = Decoder::default();
            let mut events = Vec::new();
            for input in records(&packet(&record(unit, true, vk, 1, flags))) {
                events.extend(decoder.record(&input).unwrap());
            }
            assert_eq!(events, vec![Event::Key(KeyEvent::new(code, modifiers))]);
            assert_ui_key_semantics(&events[0]);
            for input in records(&packet(&record(unit, false, vk, 1, flags))) {
                assert!(decoder.record(&input).unwrap().is_empty());
            }
        }
        for framed in [false, true] {
            let mut decoder = Decoder::default();
            let payload = "a\u{1f469}\u{200d}\u{1f4bb}\r\nz";
            let original = records(&format!("\x1b[200~{payload}\x1b[201~"));
            let wire = if framed {
                original
                    .iter()
                    .flat_map(|item| records(&packet(item)))
                    .collect()
            } else {
                original
            };
            let mut events = Vec::new();
            for input in wire {
                events.extend(decoder.record(&input).unwrap());
            }
            assert_eq!(events, vec![Event::Paste(payload.into())]);
        }
    }

    #[test]
    fn win32_packets_validate_fields_and_retain_native_repeat_counts() {
        for value in [
            "\x1b[1;2;65536;1;0;1_",
            "\x1b[13;0;13;2;0;1_",
            "\x1b[1;2;3;1;0;1;7_",
            "\x1b[nope_",
        ] {
            assert!(win32_key(value).is_err());
        }
        let defaults = win32_key("\x1b[13;;13;1_").unwrap();
        assert_eq!(defaults.wRepeatCount, 1);
        assert_eq!(defaults.dwControlKeyState, 0);
        let mut decoder = Decoder::default();
        let text = packet(&record(b'x' as u16, true, 0x58, 4, 0));
        let events = records(&text)
            .iter()
            .flat_map(|item| decoder.record(item).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            events,
            vec![Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)); 4]
        );
    }

    #[test]
    fn native_utf16_key_up_does_not_consume_surrogates_and_repeats_are_retained() {
        let mut decoder = Decoder::default();
        let text = "a\u{1f469}\u{200d}\u{1f4bb}z";
        let events = records(text)
            .iter()
            .flat_map(|record| decoder.record(record).unwrap())
            .collect::<Vec<_>>();
        let actual = events
            .iter()
            .map(|event| {
                let Event::Key(KeyEvent {
                    code: KeyCode::Char(ch),
                    ..
                }) = event
                else {
                    panic!("expected character")
                };
                *ch
            })
            .collect::<String>();
        assert_eq!(actual, text);
        assert_eq!(
            decoder
                .record(&record(b'x' as u16, true, 0x58, 5, 0))
                .unwrap()
                .len(),
            5
        );
        assert!(decoder
            .record(&record(b'x' as u16, false, 0x58, 5, 0))
            .unwrap()
            .is_empty());
        assert!(decoder.record(&record(0xdc00, true, 0, 1, 0)).is_err());
    }

    #[test]
    fn fragmented_paste_is_one_event_and_never_dispatches_keys_or_commands() {
        let _locale = crate::test_support::lock_locale();
        let payload = "/round9-invalid-one\r\n/round9-invalid-two\u{1f469}";
        let mut decoder = Decoder::default();
        let mut state = State::new();
        state.task_list = false;
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        let mut events = Vec::new();
        for record in records(&format!("\x1b[200~{payload}\x1b[201")) {
            events.extend(decoder.record(&record).unwrap());
            state.event(serde_json::json!({"type":"event","eventId":uuid::Uuid::new_v4().to_string(),"kind":"ProgressReported","workId":"other"}));
            assert!(
                events.is_empty(),
                "partial paste must not expose an Enter or draft prefix"
            );
        }
        events.extend(decoder.record(&record(b'~' as u16, true, 0, 1, 0)).unwrap());
        assert_eq!(events, vec![Event::Paste(payload.into())]);
        for event in events {
            match event {
                Event::Paste(text) => handle_paste(&mut state, &text),
                Event::Key(key) => handle_key(&mut state, key.code, key.modifiers, &jobs).unwrap(),
                _ => {}
            }
        }
        assert_eq!(
            state.editor_view().unwrap().draft,
            payload.replace("\r\n", "\n")
        );
        assert!(
            receiver.try_recv().is_err(),
            "paste must not queue any operation"
        );
        assert!(!state.busy);
        let physical_enter = decoder.record(&record(13, true, 13, 1, 0)).unwrap();
        assert_eq!(
            physical_enter,
            vec![Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            ))]
        );
    }

    #[test]
    fn native_modifiers_navigation_and_vt_controls_remain_distinct_from_paste() {
        let mut decoder = Decoder::default();
        for (vk, unit, flags, code, modifiers) in [
            (0x25, 0, SHIFT_PRESSED, KeyCode::Left, KeyModifiers::SHIFT),
            (0x0d, 13, SHIFT_PRESSED, KeyCode::Enter, KeyModifiers::SHIFT),
            (
                0x0d,
                13,
                LEFT_CTRL_PRESSED,
                KeyCode::Enter,
                KeyModifiers::CONTROL,
            ),
            (
                0x41,
                1,
                LEFT_CTRL_PRESSED,
                KeyCode::Char('a'),
                KeyModifiers::CONTROL,
            ),
            (0x71, 0, 0, KeyCode::F(2), KeyModifiers::NONE),
            (0x1b, 27, 0, KeyCode::Esc, KeyModifiers::NONE),
            (
                0x45,
                0x20ac,
                RIGHT_ALT_PRESSED | LEFT_CTRL_PRESSED,
                KeyCode::Char('\u{20ac}'),
                KeyModifiers::NONE,
            ),
        ] {
            assert_eq!(
                decoder.record(&record(unit, true, vk, 1, flags)).unwrap(),
                vec![Event::Key(KeyEvent::new(code, modifiers))]
            );
        }
        for (sequence, code, modifiers) in [
            ("\x1b[1;2D", KeyCode::Left, KeyModifiers::SHIFT),
            ("\x1b[1;5F", KeyCode::End, KeyModifiers::CONTROL),
            ("\x1bOQ", KeyCode::F(2), KeyModifiers::NONE),
            ("\x1b[3~", KeyCode::Delete, KeyModifiers::NONE),
        ] {
            let events = records(sequence)
                .iter()
                .flat_map(|record| decoder.record(record).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(events, vec![Event::Key(KeyEvent::new(code, modifiers))]);
        }
        let empty = records("\x1b[200~\x1b[201~")
            .iter()
            .flat_map(|record| decoder.record(record).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(empty, vec![Event::Paste(String::new())]);
    }

    #[test]
    fn alt_code_text_uses_release_without_inserting_numpad_digits() {
        let mut decoder = Decoder::default();
        assert!(decoder
            .record(&record(b'1' as u16, true, 0x61, 1, LEFT_ALT_PRESSED))
            .unwrap()
            .is_empty());
        let expected = "\u{4e2d}\u{1f469}\u{200d}\u{1f4bb}";
        let mut actual = String::new();
        for unit in expected.encode_utf16() {
            for event in decoder
                .record(&record(unit, false, 0x12, 1, LEFT_ALT_PRESSED))
                .unwrap()
            {
                let Event::Key(KeyEvent {
                    code: KeyCode::Char(ch),
                    modifiers,
                    ..
                }) = event
                else {
                    panic!("expected Alt-code text")
                };
                assert_eq!(modifiers, KeyModifiers::NONE);
                actual.push(ch);
            }
        }
        assert_eq!(actual, expected);
        let events = decoder
            .record(&record(9, true, 9, 1, SHIFT_PRESSED))
            .unwrap();
        assert_eq!(
            events,
            vec![Event::Key(KeyEvent::new(
                KeyCode::BackTab,
                KeyModifiers::SHIFT
            ))]
        );
    }

    #[tokio::test]
    async fn native_console_reader_runs_in_an_owned_hidden_console_and_restores_mode() {
        use std::process::Stdio;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "agent_center::ui::input::tests::owned_console_reader_child",
                "--ignored",
                "--nocapture",
            ])
            .env("WTA_OWNED_CONSOLE_INPUT_TEST", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .kill_on_drop(true);
        let output = tokio::time::timeout(std::time::Duration::from_secs(20), command.output())
            .await
            .expect("owned reader process must exit without leaving a blocked input thread")
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    #[ignore = "launched only by the parent test in an isolated hidden console"]
    async fn owned_console_reader_child() {
        use windows_sys::Win32::System::Console::WriteConsoleInputW;
        let transport = std::env::var("WTA_OWNED_CONSOLE_INPUT_TEST").unwrap();
        assert!(transport == "1" || transport == "conpty");
        let mut input = ConsoleInput::new().unwrap();
        let console = input.console.clone();
        let original_mode = input.original_mode;
        let output_handle = input
            .keyboard_mode
            .as_ref()
            .unwrap()
            .output
            .try_clone()
            .unwrap();
        let original_output_mode = input.keyboard_mode.as_ref().unwrap().original_mode;
        let payload = "a\u{1f469}\u{200d}\u{1f4bb}z\r\n/round9-invalid-two";
        // Per-unit down/up pairs reproduce the real ConPTY record shape.
        if transport == "conpty" {
            use std::io::Write;
            let mut output = OpenOptions::new().write(true).open("CONOUT$").unwrap();
            output.write_all(b"CENTER_INPUT_READY\r\n").unwrap();
            output.flush().unwrap();
        }
        for record in records(&format!("\x1b[200~{payload}\x1b[201~"))
            .into_iter()
            .filter(|_| transport == "1")
        {
            let mut count = 0;
            assert_ne!(
                unsafe { WriteConsoleInputW(console.as_raw_handle(), &record, 1, &mut count) },
                0
            );
            assert_eq!(count, 1);
        }
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), input.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(event, Event::Paste(payload.into()));
        for (unit, vk, flags, code, modifiers) in [
            (13, 13, SHIFT_PRESSED, KeyCode::Enter, KeyModifiers::SHIFT),
            (
                13,
                13,
                LEFT_CTRL_PRESSED,
                KeyCode::Enter,
                KeyModifiers::CONTROL,
            ),
            (27, 27, 0, KeyCode::Esc, KeyModifiers::NONE),
            (13, 13, 0, KeyCode::Enter, KeyModifiers::NONE),
        ] {
            if transport == "1" {
                let mut count = 0;
                let key = record(unit, true, vk, 1, flags);
                assert_ne!(
                    unsafe { WriteConsoleInputW(console.as_raw_handle(), &key, 1, &mut count) },
                    0
                );
            }
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), input.next())
                .await
                .expect("a physical key must not wait for the next key")
                .unwrap()
                .unwrap();
            assert_eq!(event, Event::Key(KeyEvent::new(code, modifiers)));
            assert_ui_key_semantics(&event);
            if transport == "conpty" && code == KeyCode::Esc {
                let mut output = OpenOptions::new().write(true).open("CONOUT$").unwrap();
                output.write_all(b"CENTER_ESCAPE_OBSERVED\r\n").unwrap();
                output.flush().unwrap();
            }
        }
        drop(input);
        let mut mode = 0;
        assert_ne!(
            unsafe { GetConsoleMode(console.as_raw_handle(), &mut mode) },
            0
        );
        assert_eq!(mode, original_mode);
        assert_ne!(
            unsafe { GetConsoleMode(output_handle.as_raw_handle(), &mut mode) },
            0
        );
        assert_eq!(mode, original_output_mode);
    }

    #[test]
    fn real_conpty_preserves_paste_delimiters_unicode_and_physical_enter() {
        use std::io::{Read, Write};
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::System::{
            Console::{ClosePseudoConsole, CreatePseudoConsole, COORD},
            Pipes::CreatePipe,
            Threading::{
                CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
                InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
                WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
                PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTUPINFOEXW,
            },
        };
        fn pipe() -> (OwnedHandle, OwnedHandle) {
            let (mut read, mut write) = (std::ptr::null_mut(), std::ptr::null_mut());
            assert_ne!(
                unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) },
                0
            );
            unsafe {
                (
                    OwnedHandle::from_raw_handle(read),
                    OwnedHandle::from_raw_handle(write),
                )
            }
        }
        let (input_read, input_write) = pipe();
        let (output_read, output_write) = pipe();
        let mut pty = 0;
        assert_eq!(
            unsafe {
                CreatePseudoConsole(
                    COORD { X: 100, Y: 30 },
                    input_read.as_raw_handle(),
                    output_write.as_raw_handle(),
                    0,
                    &mut pty,
                )
            },
            0
        );
        drop(input_read);
        drop(output_write);
        let mut size = 0;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        }
        let mut attributes = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
        let list = attributes.as_mut_ptr().cast();
        assert_ne!(
            unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) },
            0
        );
        assert_ne!(
            unsafe {
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    pty as *const _,
                    std::mem::size_of_val(&pty),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            },
            0
        );
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.lpAttributeList = list;
        let mut command = format!("\"{}\" --exact agent_center::ui::input::tests::owned_console_reader_child --ignored --nocapture",
            std::env::current_exe().unwrap().display()).encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let mut variables = std::env::vars_os()
            .filter(|(name, _)| name != "WTA_OWNED_CONSOLE_INPUT_TEST")
            .collect::<Vec<_>>();
        variables.push(("WTA_OWNED_CONSOLE_INPUT_TEST".into(), "conpty".into()));
        variables.sort_by_key(|(name, _)| name.to_string_lossy().to_uppercase());
        let mut environment = Vec::new();
        for (mut name, value) in variables {
            name.push("=");
            name.push(value);
            environment.extend(name.encode_wide().chain(Some(0)));
        }
        environment.push(0);
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let created = unsafe {
            CreateProcessW(
                std::ptr::null(),
                command.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                environment.as_ptr().cast(),
                std::ptr::null(),
                &startup.StartupInfo,
                &mut process,
            )
        };
        let error = io::Error::last_os_error();
        unsafe {
            DeleteProcThreadAttributeList(list);
        }
        if created == 0 {
            unsafe {
                ClosePseudoConsole(pty);
            }
            panic!("create test-owned ConPTY child: {error}");
        }
        let process_handle = unsafe { OwnedHandle::from_raw_handle(process.hProcess) };
        drop(unsafe { OwnedHandle::from_raw_handle(process.hThread) });
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let output = std::thread::spawn(move || {
            let mut reader = File::from(output_read);
            let mut collected = Vec::new();
            let mut buffer = [0; 4096];
            let mut ready = false;
            let mut escaped = false;
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        collected.extend_from_slice(&buffer[..count]);
                        if !ready
                            && String::from_utf8_lossy(&collected).contains("CENTER_INPUT_READY")
                        {
                            ready = true;
                            let _ = ready_tx.send(());
                        }
                        if !escaped
                            && String::from_utf8_lossy(&collected)
                                .contains("CENTER_ESCAPE_OBSERVED")
                        {
                            escaped = true;
                            let _ = ready_tx.send(());
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::BrokenPipe => break,
                    Err(error) => panic!("read owned ConPTY output: {error}"),
                }
            }
            collected
        });
        let mut writer = File::from(input_write);
        let sent = ready_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .is_ok();
        // No clipboard or desktop is used here: exercise the actual UTF-8 ConPTY
        // transport separately from the packaged physical-paste suite.
        let sent = sent
            && writer
                .write_all(
                    concat!(
                        "\x1b[200~a\u{1f469}\u{200d}\u{1f4bb}z\r\n/round9-invalid-two\x1b[201~",
                        "\x1b[13;0;13;1;16;1_\x1b[13;0;13;0;16;1_",
                        "\x1b[13;0;13;1;8;1_\x1b[13;0;13;0;8;1_",
                        "\x1b[27;0;27;1;0;1_\x1b[27;0;27;0;0;1_"
                    )
                    .as_bytes(),
                )
                .is_ok();
        let sent = sent
            && ready_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok()
            && writer.write_all(b"\x1b[13;0;13;1;0;1_").is_ok();
        let waited = unsafe { WaitForSingleObject(process_handle.as_raw_handle(), 10000) };
        if waited != WAIT_OBJECT_0 {
            unsafe {
                TerminateProcess(process_handle.as_raw_handle(), 1);
            }
        }
        let mut exit_code = 1;
        unsafe {
            GetExitCodeProcess(process_handle.as_raw_handle(), &mut exit_code);
            ClosePseudoConsole(pty);
        }
        drop(writer);
        let output = output.join().unwrap();
        assert!(
            sent && waited == WAIT_OBJECT_0 && exit_code == 0,
            "ConPTY child failed ({exit_code}): {}",
            String::from_utf8_lossy(&output)
        );
    }
}
