// tools/wta/src/conpty_handle.rs
//
// Thin Read/Write wrappers around raw Windows HANDLEs that point at the
// slave side of a conpty (pseudo-terminal). The shared wta process
// receives these HANDLEs from Terminal via `_internal.attach_pane`
// over the existing IProtocolEventCallback channel: WT creates the
// conpty, `DuplicateHandle`s the slave ends into wta's process, and
// sends the resulting numeric HANDLE values as JSON numbers in the
// OnEvent payload. wta wraps those raw values with the types here so
// that:
//
//   * `ConptyReader` can be fed to a tokio task that pumps user input
//     into the right TabSession dispatcher.
//   * `ConptyWriter` can be passed to Ratatui's
//     `Terminal<CrosstermBackend<W>>` to render the agent-pane UI
//     directly into the conpty's master-side reader (i.e. WT's
//     TermControl).
//
// Lifetime: each wrapper takes ownership of its HANDLE. Dropping the
// wrapper closes the HANDLE via `OwnedHandle`'s drop, which on a pipe
// surfaces EOF to the matching end. Terminal must `CloseHandle` its
// own copies of the slave HANDLEs only AFTER it has handed them to
// wta — once `OnEvent` returns, wta's duplicates are independent
// references and Terminal's are no longer needed.
//
// Errors: we use the OS handle directly (no FFI helper crate) so that
// the failure modes are exactly the Windows error model. `ReadFile`
// on a closed pipe surfaces ERROR_BROKEN_PIPE which the std I/O
// adapter translates to UnexpectedEof — we normalise that to a
// zero-length read so callers see the expected "EOF" semantic.

use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

use windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE;
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};

/// Wraps the slave-side read HANDLE of a conpty. wta reads user
/// keystrokes (and the conpty master side's input echoes) from this.
pub struct ConptyReader {
    handle: OwnedHandle,
}

impl ConptyReader {
    /// Wrap an already-DuplicateHandle'd raw HANDLE living in this
    /// process's handle table.
    ///
    /// # Safety
    /// The caller must guarantee that `handle`:
    ///   * is a valid HANDLE in the current process,
    ///   * supports `ReadFile` (i.e. has at least GENERIC_READ access),
    ///   * has no other live owners (this wrapper will close it on
    ///     drop).
    pub unsafe fn from_raw_handle(handle: RawHandle) -> Self {
        Self {
            handle: unsafe { OwnedHandle::from_raw_handle(handle) },
        }
    }
}

impl Read for ConptyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut bytes_read: u32 = 0;
        let ok = unsafe {
            ReadFile(
                self.handle.as_raw_handle() as _,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut bytes_read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let err = io::Error::last_os_error();
            // The far end of the pipe was closed. Pipe semantics treat
            // this as EOF; surface it as a zero-length read instead of
            // propagating the OS error so the standard read loop can
            // exit cleanly.
            if err.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                return Ok(0);
            }
            return Err(err);
        }
        Ok(bytes_read as usize)
    }
}

/// Wraps the slave-side write HANDLE of a conpty. Ratatui renders
/// ANSI bytes here; they travel through the conpty's kernel object
/// and surface on the master side, where TermControl reads and
/// renders them into the user's window.
pub struct ConptyWriter {
    handle: OwnedHandle,
}

impl ConptyWriter {
    /// Wrap an already-DuplicateHandle'd raw HANDLE. See
    /// `ConptyReader::from_raw_handle` for the safety contract; the
    /// only difference is that this HANDLE needs GENERIC_WRITE access.
    pub unsafe fn from_raw_handle(handle: RawHandle) -> Self {
        Self {
            handle: unsafe { OwnedHandle::from_raw_handle(handle) },
        }
    }
}

impl Write for ConptyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut bytes_written: u32 = 0;
        let ok = unsafe {
            WriteFile(
                self.handle.as_raw_handle() as _,
                buf.as_ptr(),
                buf.len() as u32,
                &mut bytes_written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(bytes_written as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        // Pipe writes are not OS-buffered. Higher-level buffers (e.g.
        // Ratatui's render-side `BufWriter`) live above us; their
        // `flush` will drive ours via the Write trait chain.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::windows::io::RawHandle;
    use windows_sys::Win32::System::Pipes::CreatePipe;

    /// Allocate an anonymous pipe and wrap both ends. The read end of
    /// the OS pipe becomes our `ConptyReader`; the write end becomes
    /// our `ConptyWriter`. Independent of any actual conpty — we are
    /// testing the wrapper plumbing here, not the conpty kernel
    /// object (that's covered by integration tests at the WT-side
    /// boundary).
    fn make_pipe() -> (ConptyReader, ConptyWriter) {
        let mut read_h: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut write_h: *mut std::ffi::c_void = std::ptr::null_mut();
        let ok = unsafe { CreatePipe(&mut read_h, &mut write_h, std::ptr::null_mut(), 0) };
        assert_ne!(ok, 0, "CreatePipe failed: {}", std::io::Error::last_os_error());
        unsafe {
            (
                ConptyReader::from_raw_handle(read_h as RawHandle),
                ConptyWriter::from_raw_handle(write_h as RawHandle),
            )
        }
    }

    #[test]
    fn write_then_read_roundtrips_bytes() {
        let (mut reader, mut writer) = make_pipe();
        writer.write_all(b"hello").unwrap();
        let mut buf = [0u8; 16];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello");
    }

    #[test]
    fn two_pipes_are_independent() {
        // The core multi-pane invariant: two ConptyWriter instances in
        // the same process drive two unrelated pipes without
        // cross-contamination. This is the property that lets a
        // single wta serve N agent panes.
        let (mut reader_a, mut writer_a) = make_pipe();
        let (mut reader_b, mut writer_b) = make_pipe();

        writer_a.write_all(b"AAA").unwrap();
        writer_b.write_all(b"BBBB").unwrap();

        let mut buf_a = [0u8; 8];
        let mut buf_b = [0u8; 8];
        let na = reader_a.read(&mut buf_a).unwrap();
        let nb = reader_b.read(&mut buf_b).unwrap();

        assert_eq!(&buf_a[..na], b"AAA");
        assert_eq!(&buf_b[..nb], b"BBBB");
    }

    #[test]
    fn dropping_writer_surfaces_eof_to_reader() {
        // Models the close path: when wta drops a RenderCtx in
        // response to _internal.detach_pane, the writer is dropped,
        // its HANDLE is closed, and the reader on the matching end
        // sees a clean EOF (zero-length read, no error). This is the
        // signal Terminal-side TermControl would use to know wta has
        // finished talking to it.
        let (mut reader, writer) = make_pipe();
        drop(writer);

        let mut buf = [0u8; 8];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0, "expected EOF after dropping writer");
    }

    #[test]
    fn write_returns_partial_count_correctly() {
        // Sanity-check the byte count: a multi-byte write through
        // WriteFile should report the full count we sent.
        let (mut _reader, mut writer) = make_pipe();
        let payload = b"the quick brown fox jumps over the lazy dog";
        let n = writer.write(payload).unwrap();
        assert_eq!(n, payload.len());
    }
}
