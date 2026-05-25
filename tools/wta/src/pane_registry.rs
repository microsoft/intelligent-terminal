// tools/wta/src/pane_registry.rs
//
// Owns the live RenderCtx of every attached agent pane in the shared
// wta process, keyed by tab StableId. The OnEvent dispatcher calls
// into this when it sees an `_internal.attach_pane` /
// `_internal.detach_pane` event.
//
// Architectural note: this is the wta-side counterpart of the WT-side
// per-tab pane ownership change (spec §"WT-side changes"). Each tab
// in the user's UI that has an agent pane corresponds to exactly one
// entry here. Tabs without an agent pane have no entry. Window
// membership is not tracked — a tab being "in Window A vs B" is a
// WT-side concept that wta is intentionally ignorant of.
//
// Replacement semantics: `attach` for a tab id that's already
// attached drops the old RenderCtx and inserts the new one. The
// returned `Option<RenderCtx>` lets the caller inspect what was
// displaced (mostly for diagnostics — under normal operation tabs
// don't double-attach because WT issues attach exactly once per pane
// open). When the old ctx is dropped, its conpty HANDLEs close,
// which surfaces EOF on TermControl's master-side read; the old
// agent CLI child process exits when its stdio breaks.

use std::collections::HashMap;
use std::io;
use std::os::windows::io::RawHandle;

use crate::conpty_handle::{ConptyReader, ConptyWriter};
use crate::protocol::internal_control::AttachPaneParams;
use crate::render_ctx::RenderCtx;

pub struct PaneRegistry {
    ctxs: HashMap<String, RenderCtx>,
}

impl PaneRegistry {
    pub fn new() -> Self {
        Self {
            ctxs: HashMap::new(),
        }
    }

    /// Materialise the conpty HANDLEs from `params`, build a
    /// RenderCtx, and insert it under `params.tab_id`. If a context
    /// was already attached for that tab, the old one is returned
    /// (its drop closes the displaced HANDLEs).
    ///
    /// # Safety
    /// The caller — wta's OnEvent dispatcher — must have verified
    /// that `params.pty_in` and `params.pty_out` are valid HANDLEs
    /// in wta's process, were `DuplicateHandle`'d into wta by
    /// Terminal just before this call, and are not owned by anything
    /// else (the wrappers will close them on drop). The protocol
    /// contract documented in
    /// doc/specs/Multi-window-agent-pane.md → "Handle marshaling"
    /// is what bridges this safety requirement to runtime behaviour.
    pub unsafe fn attach(
        &mut self,
        params: AttachPaneParams,
    ) -> io::Result<Option<RenderCtx>> {
        let reader = unsafe { ConptyReader::from_raw_handle(params.pty_in as RawHandle) };
        let writer = unsafe { ConptyWriter::from_raw_handle(params.pty_out as RawHandle) };
        let ctx = RenderCtx::attach(reader, writer, params.cols, params.rows)?;
        Ok(self.ctxs.insert(params.tab_id, ctx))
    }

    /// Drop the context for `tab_id`. Returns the displaced ctx (so
    /// the dispatcher can confirm something was actually attached
    /// when emitting the `detach_pane_ack`).
    pub fn detach(&mut self, tab_id: &str) -> Option<RenderCtx> {
        self.ctxs.remove(tab_id)
    }

    /// Mutable lookup for the dispatcher / render loop. Returns
    /// `None` for tabs that have no agent pane attached.
    pub fn get_mut(&mut self, tab_id: &str) -> Option<&mut RenderCtx> {
        self.ctxs.get_mut(tab_id)
    }

    /// Diagnostic counter.
    pub fn len(&self) -> usize {
        self.ctxs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ctxs.is_empty()
    }
}

impl Default for PaneRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::ptr::null_mut;
    use windows_sys::Win32::System::Pipes::CreatePipe;

    /// Create two anonymous pipes that stand in for a conpty's
    /// slave-in / slave-out handle pair. Returns the wta-side raw
    /// HANDLE values (to plug into AttachPaneParams as `pty_in` /
    /// `pty_out`) plus an observer reader (the matching end of the
    /// output pipe) that lets tests verify rendered bytes.
    ///
    /// In production, `params.pty_in` is the slave-side READ handle
    /// (so wta reads user input) and `params.pty_out` is the
    /// slave-side WRITE handle (so wta writes its render). The
    /// "master" sides are held by WT's TermControl. Here we mimic
    /// that split with two separate anonymous pipes.
    fn fresh_attach_handles() -> (u64, u64, ConptyReader) {
        let mut input_read: *mut std::ffi::c_void = null_mut();
        let mut input_write: *mut std::ffi::c_void = null_mut();
        let mut output_read: *mut std::ffi::c_void = null_mut();
        let mut output_write: *mut std::ffi::c_void = null_mut();
        unsafe {
            assert_ne!(
                CreatePipe(&mut input_read, &mut input_write, null_mut(), 0),
                0
            );
            assert_ne!(
                CreatePipe(&mut output_read, &mut output_write, null_mut(), 0),
                0
            );
        }
        // wta's pty_in is the input pipe's READ end (user keystrokes
        // flow in from the master-side write). wta's pty_out is the
        // output pipe's WRITE end (renders flow out to the
        // master-side read).
        let pty_in = input_read as u64;
        let pty_out = output_write as u64;

        // We don't need the unused master-side ends in tests; closing
        // input_write would EOF wta's reader, which we want to avoid
        // during the test. Leak them as raw values that the OS will
        // reclaim on process exit.
        let _ = input_write;
        let observer = unsafe { ConptyReader::from_raw_handle(output_read as RawHandle) };

        (pty_in, pty_out, observer)
    }

    fn sample_params(tab_id: &str, pty_in: u64, pty_out: u64) -> AttachPaneParams {
        AttachPaneParams {
            tab_id: tab_id.to_string(),
            pty_in,
            pty_out,
            cols: 80,
            rows: 24,
            agent_id: "copilot".to_string(),
            initial_cwd: "C:\\".to_string(),
            initial_view: "chat".to_string(),
        }
    }

    #[test]
    fn attach_then_detach_returns_to_empty() {
        let mut reg = PaneRegistry::new();
        assert!(reg.is_empty());

        let (pin, pout, _obs) = fresh_attach_handles();
        let displaced = unsafe { reg.attach(sample_params("T1", pin, pout)) }.unwrap();
        assert!(displaced.is_none(), "first attach should not displace anything");
        assert_eq!(reg.len(), 1);

        let dropped = reg.detach("T1");
        assert!(dropped.is_some());
        assert!(reg.is_empty());
    }

    #[test]
    fn attaches_can_coexist_for_different_tabs() {
        let mut reg = PaneRegistry::new();
        let (pin_a, pout_a, _obs_a) = fresh_attach_handles();
        let (pin_b, pout_b, _obs_b) = fresh_attach_handles();

        unsafe { reg.attach(sample_params("T1", pin_a, pout_a)).unwrap() };
        unsafe { reg.attach(sample_params("T2", pin_b, pout_b)).unwrap() };

        assert_eq!(reg.len(), 2);
        assert!(reg.get_mut("T1").is_some());
        assert!(reg.get_mut("T2").is_some());
    }

    #[test]
    fn detach_one_leaves_others_alone() {
        let mut reg = PaneRegistry::new();
        let (pin_a, pout_a, _obs_a) = fresh_attach_handles();
        let (pin_b, pout_b, _obs_b) = fresh_attach_handles();

        unsafe { reg.attach(sample_params("T1", pin_a, pout_a)).unwrap() };
        unsafe { reg.attach(sample_params("T2", pin_b, pout_b)).unwrap() };

        let _ = reg.detach("T1");
        assert_eq!(reg.len(), 1);
        assert!(reg.get_mut("T1").is_none());
        assert!(reg.get_mut("T2").is_some(), "T2 should survive T1's detach");
    }

    #[test]
    fn detach_unknown_tab_returns_none() {
        let mut reg = PaneRegistry::new();
        assert!(reg.detach("nonexistent").is_none());
    }

    #[test]
    fn second_attach_to_same_tab_displaces_first() {
        let mut reg = PaneRegistry::new();
        let (pin_1, pout_1, _obs_1) = fresh_attach_handles();
        let (pin_2, pout_2, _obs_2) = fresh_attach_handles();

        unsafe { reg.attach(sample_params("T1", pin_1, pout_1)).unwrap() };
        let displaced = unsafe { reg.attach(sample_params("T1", pin_2, pout_2)) }.unwrap();
        assert!(displaced.is_some(), "second attach should return the displaced ctx");
        assert_eq!(reg.len(), 1, "only one ctx per tab id remains");
    }

    #[test]
    fn attached_ctx_can_render_to_its_pipe() {
        // Smoke test: end-to-end through the registry, prove the
        // RenderCtx behind a tab id actually drives bytes through to
        // its observer. Exercises that attach materialized the
        // HANDLEs correctly and stored a usable RenderCtx.
        let mut reg = PaneRegistry::new();
        let (pin, pout, mut observer) = fresh_attach_handles();

        unsafe { reg.attach(sample_params("T9", pin, pout)).unwrap() };

        let ctx = reg.get_mut("T9").unwrap();
        ctx.terminal_mut()
            .draw(|frame| {
                use ratatui::widgets::Paragraph;
                frame.render_widget(Paragraph::new("REG_PROOF"), frame.area());
            })
            .unwrap();

        let mut buf = vec![0u8; 4096];
        let n = observer.read(&mut buf).unwrap();
        let rendered = String::from_utf8_lossy(&buf[..n]);
        assert!(
            rendered.contains("REG_PROOF"),
            "render via registry didn't surface; got: {rendered:?}"
        );
    }
}
