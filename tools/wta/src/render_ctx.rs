// tools/wta/src/render_ctx.rs
//
// Per-pane render context. Each attached agent pane has one of these
// in the shared wta process, keyed by tab StableId in
// `App::render_ctxs`:
//
//     render_ctxs: HashMap<TabStableId, RenderCtx>
//
// A RenderCtx owns:
//   * `terminal`: a Ratatui `Terminal<CrosstermBackend<ConptyWriter>>`.
//     Rendered ANSI bytes flow into the ConptyWriter, through the
//     conpty kernel object, out to TermControl on the master side.
//   * `reader`: the matching ConptyReader. The input dispatch task
//     pulls user keystrokes out of this and routes them to the
//     TabSession that wta is hosting for the same tab id.
//
// Ratatui's `Terminal::new` calls `backend.size()` at construction
// time, which queries the controlling tty. Since our writer is just
// a HANDLE to a conpty slave (not the process's tty), we go via
// `Terminal::with_options` + `Viewport::Fixed` and feed it the
// dimensions the conpty was created with — Terminal knows these
// from `CreatePseudoConsole({rows, cols}, ...)` and passes them on
// `_internal.attach_pane`. Subsequent `_internal.resize_pane` events
// drive `Terminal::resize` to keep the viewport in sync.

use std::io;

use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Rect, Size};
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::conpty_handle::{ConptyReader, ConptyWriter};

pub struct RenderCtx {
    terminal: Terminal<CrosstermBackend<ConptyWriter>>,
    reader: Option<ConptyReader>,
}

impl RenderCtx {
    /// Build a render context from an already-attached pair of conpty
    /// slave HANDLEs. `cols` and `rows` describe the initial pty
    /// dimensions; they should match what Terminal passed to
    /// `CreatePseudoConsole`.
    pub fn attach(
        reader: ConptyReader,
        writer: ConptyWriter,
        cols: u16,
        rows: u16,
    ) -> io::Result<Self> {
        let backend = CrosstermBackend::new(writer);
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, cols, rows)),
            },
        )?;
        Ok(Self {
            terminal,
            reader: Some(reader),
        })
    }

    /// Mutable access to the Ratatui terminal so render code can call
    /// `.draw(...)` against this pane's view.
    pub fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<ConptyWriter>> {
        &mut self.terminal
    }

    /// Take ownership of the reader. The input dispatch task calls
    /// this once at attach time and runs the read loop on its own
    /// tokio task. After the take, subsequent calls return `None`.
    pub fn take_reader(&mut self) -> Option<ConptyReader> {
        self.reader.take()
    }

    /// Update the viewport dimensions in response to
    /// `_internal.resize_pane`. The conpty's own SIGWINCH-equivalent
    /// will also propagate to the agent CLI; this call keeps Ratatui's
    /// view of the size correct.
    pub fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        self.terminal.resize(Rect::new(0, 0, cols, rows))
    }

    /// Current viewport size, for diagnostics and resize idempotency
    /// checks.
    pub fn size(&mut self) -> Size {
        let area = self.terminal.get_frame().area();
        Size::new(area.width, area.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conpty_handle::{ConptyReader, ConptyWriter};
    use std::io::Read;
    use std::os::windows::io::RawHandle;
    use std::ptr::null_mut;
    use windows_sys::Win32::System::Pipes::CreatePipe;

    /// Same pipe-building helper used by `conpty_handle` tests. Pipes
    /// stand in for conpties at this level: we don't need a real
    /// conpty kernel object to verify that Ratatui-via-ConptyWriter
    /// emits bytes the matching reader can see.
    fn make_pipe_pair() -> (ConptyReader, ConptyWriter, ConptyReader, ConptyWriter) {
        // Two pipes: one for input (reader-side belongs to wta), one
        // for output (writer-side belongs to wta). For RenderCtx
        // testing we use the wta-output pipe's matching reader to
        // observe what got rendered.
        let mut in_read: *mut std::ffi::c_void = null_mut();
        let mut in_write: *mut std::ffi::c_void = null_mut();
        let mut out_read: *mut std::ffi::c_void = null_mut();
        let mut out_write: *mut std::ffi::c_void = null_mut();
        unsafe {
            assert_ne!(
                CreatePipe(&mut in_read, &mut in_write, null_mut(), 0),
                0,
                "CreatePipe (in) failed"
            );
            assert_ne!(
                CreatePipe(&mut out_read, &mut out_write, null_mut(), 0),
                0,
                "CreatePipe (out) failed"
            );
            (
                ConptyReader::from_raw_handle(in_read as RawHandle),
                ConptyWriter::from_raw_handle(in_write as RawHandle),
                ConptyReader::from_raw_handle(out_read as RawHandle),
                ConptyWriter::from_raw_handle(out_write as RawHandle),
            )
        }
    }

    #[test]
    fn attach_yields_terminal_with_requested_size() {
        let (wta_reader, _master_in, _master_observer, wta_writer) = make_pipe_pair();
        let mut ctx = RenderCtx::attach(wta_reader, wta_writer, 80, 24).unwrap();
        let size = ctx.size();
        assert_eq!(size.width, 80);
        assert_eq!(size.height, 24);
    }

    #[test]
    fn draw_emits_bytes_observable_on_pipe() {
        // The core integration test: a draw call against the
        // RenderCtx writes ANSI to the conpty slave-write HANDLE.
        // The matching pipe read end (which in production is held by
        // WT's TermControl) sees those bytes.
        let (wta_reader, _master_in, mut master_observer, wta_writer) = make_pipe_pair();
        let mut ctx = RenderCtx::attach(wta_reader, wta_writer, 40, 5).unwrap();

        ctx.terminal_mut()
            .draw(|frame| {
                use ratatui::widgets::Paragraph;
                let p = Paragraph::new("HELLO_PANE");
                frame.render_widget(p, frame.area());
            })
            .unwrap();

        // The reader is non-blocking semantically only when bytes are
        // ready. We drained the draw output synchronously into the
        // pipe; reading should return promptly. Read until we either
        // find the marker or exhaust the available buffer.
        let mut buf = vec![0u8; 4096];
        let n = master_observer.read(&mut buf).unwrap();
        assert!(n > 0, "expected non-empty render output");
        let rendered = String::from_utf8_lossy(&buf[..n]);
        assert!(
            rendered.contains("HELLO_PANE"),
            "rendered output did not contain marker; got bytes: {rendered:?}"
        );
    }

    #[test]
    fn two_render_ctxs_render_independently() {
        // The multi-pane invariant at the Ratatui level: two
        // RenderCtx instances in the same process drive disjoint
        // pipes without their renders interleaving.
        let (wta_reader_a, _mi_a, mut obs_a, wta_writer_a) = make_pipe_pair();
        let (wta_reader_b, _mi_b, mut obs_b, wta_writer_b) = make_pipe_pair();

        let mut ctx_a = RenderCtx::attach(wta_reader_a, wta_writer_a, 30, 4).unwrap();
        let mut ctx_b = RenderCtx::attach(wta_reader_b, wta_writer_b, 30, 4).unwrap();

        ctx_a
            .terminal_mut()
            .draw(|f| {
                use ratatui::widgets::Paragraph;
                f.render_widget(Paragraph::new("AAA_MARKER"), f.area());
            })
            .unwrap();
        ctx_b
            .terminal_mut()
            .draw(|f| {
                use ratatui::widgets::Paragraph;
                f.render_widget(Paragraph::new("BBB_MARKER"), f.area());
            })
            .unwrap();

        let mut buf_a = vec![0u8; 4096];
        let mut buf_b = vec![0u8; 4096];
        let na = obs_a.read(&mut buf_a).unwrap();
        let nb = obs_b.read(&mut buf_b).unwrap();

        let rendered_a = String::from_utf8_lossy(&buf_a[..na]);
        let rendered_b = String::from_utf8_lossy(&buf_b[..nb]);

        assert!(rendered_a.contains("AAA_MARKER"), "ctx_a missing marker");
        assert!(rendered_b.contains("BBB_MARKER"), "ctx_b missing marker");
        // Each observer should ONLY see its own pane's output:
        assert!(
            !rendered_a.contains("BBB_MARKER"),
            "ctx_a observer saw ctx_b output"
        );
        assert!(
            !rendered_b.contains("AAA_MARKER"),
            "ctx_b observer saw ctx_a output"
        );
    }

    #[test]
    fn take_reader_returns_reader_once() {
        let (wta_reader, _master_in, _obs, wta_writer) = make_pipe_pair();
        let mut ctx = RenderCtx::attach(wta_reader, wta_writer, 20, 3).unwrap();
        assert!(ctx.take_reader().is_some(), "first take should yield reader");
        assert!(
            ctx.take_reader().is_none(),
            "second take should yield None (reader already given to the input task)"
        );
    }

    #[test]
    fn resize_updates_reported_size() {
        let (wta_reader, _master_in, _obs, wta_writer) = make_pipe_pair();
        let mut ctx = RenderCtx::attach(wta_reader, wta_writer, 40, 10).unwrap();
        assert_eq!(ctx.size(), Size::new(40, 10));
        ctx.resize(120, 30).unwrap();
        assert_eq!(ctx.size(), Size::new(120, 30));
    }
}
