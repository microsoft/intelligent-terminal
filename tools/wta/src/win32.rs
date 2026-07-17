//! Small native Windows helpers used to call OS services directly instead of
//! routing through external shell helpers.

#[cfg(windows)]
use std::io;

#[cfg(windows)]
struct ClipboardGuard;

#[cfg(windows)]
impl ClipboardGuard {
    fn open() -> io::Result<Self> {
        use windows_sys::Win32::System::DataExchange::OpenClipboard;

        for _ in 0..10 {
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Ok(Self);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::DataExchange::CloseClipboard();
        }
    }
}

/// Copy UTF-16 text to the Windows clipboard without spawning external helper
/// processes or invoking a shell parser.
#[cfg(windows)]
pub(crate) fn copy_text_to_clipboard(text: &str) -> io::Result<()> {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{EmptyClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };

    // CF_UNICODETEXT. windows-sys exposes it under Win32_System_Ole, but the
    // numeric clipboard format is stable and avoids pulling in Ole just for a
    // constant.
    const CF_UNICODETEXT: u32 = 13;

    let _guard = ClipboardGuard::open()?;
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let bytes = wide.len() * std::mem::size_of::<u16>();

    unsafe {
        if EmptyClipboard() == 0 {
            return Err(io::Error::last_os_error());
        }
        let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }

        let ptr = GlobalLock(handle);
        if ptr.is_null() {
            GlobalFree(handle);
            return Err(io::Error::last_os_error());
        }

        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr as *mut u16, wide.len());
        GlobalUnlock(handle);

        // On success, SetClipboardData transfers ownership of `handle` to the
        // OS; on failure it remains ours and must be freed.
        if SetClipboardData(CF_UNICODETEXT, handle as _).is_null() {
            GlobalFree(handle);
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn copy_text_to_clipboard(_text: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "clipboard is only supported on Windows",
    ))
}

/// Read text suitable for paste from the Windows clipboard.
#[cfg(windows)]
pub(crate) fn read_paste_string_from_clipboard() -> io::Result<String> {
    use windows_sys::Win32::System::DataExchange::{GetClipboardData, IsClipboardFormatAvailable};
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    const CF_UNICODETEXT: u32 = 13;
    const MAX_CLIPBOARD_TEXT_BYTES: usize = 4 * 1024 * 1024;

    let _guard = ClipboardGuard::open()?;
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT) != 0 {
            let handle = GetClipboardData(CF_UNICODETEXT);
            if !handle.is_null() {
                let ptr = GlobalLock(handle);
                if !ptr.is_null() {
                    let size = GlobalSize(handle);
                    if size == 0 || size > MAX_CLIPBOARD_TEXT_BYTES {
                        GlobalUnlock(handle);
                    } else {
                        let units = std::slice::from_raw_parts(
                            ptr as *const u16,
                            size / std::mem::size_of::<u16>(),
                        );
                        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
                        let text = String::from_utf16_lossy(&units[..end]);
                        GlobalUnlock(handle);
                        return Ok(text);
                    }
                }
            }
        }

        if let Some(path) = clipboard_file_path_from_open_clipboard() {
            return Ok(path.to_string_lossy().into_owned());
        }

        Ok(String::new())
    }
}

#[cfg(not(windows))]
pub(crate) fn read_paste_string_from_clipboard() -> std::io::Result<String> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "clipboard is only supported on Windows",
    ))
}

/// First file path from a CF_HDROP clipboard payload.
///
/// Must be called while the clipboard is already open.
#[cfg(windows)]
pub(crate) unsafe fn clipboard_file_path_from_open_clipboard() -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::DataExchange::{GetClipboardData, IsClipboardFormatAvailable};
    use windows_sys::Win32::UI::Shell::DragQueryFileW;

    const CF_HDROP: u32 = 15;

    if IsClipboardFormatAvailable(CF_HDROP) == 0 {
        return None;
    }
    let handle = GetClipboardData(CF_HDROP);
    if handle.is_null() {
        return None;
    }
    let needed = DragQueryFileW(handle as _, 0, std::ptr::null_mut(), 0);
    if needed == 0 {
        return None;
    }
    let mut buf = vec![0u16; needed as usize + 1];
    let got = DragQueryFileW(handle as _, 0, buf.as_mut_ptr(), buf.len() as u32);
    if got == 0 {
        return None;
    }
    buf.truncate(got as usize);
    Some(std::path::PathBuf::from(std::ffi::OsString::from_wide(&buf)))
}

/// Open a URL with the user's default handler using ShellExecuteW instead of a
/// shell wrapper.
#[cfg(windows)]
pub(crate) fn open_url_in_default_browser(url: &str) -> io::Result<()> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;

    let operation: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    let file: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        )
    };

    let code = result as isize;
    if code <= 32 {
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("ShellExecuteW failed with code {code}"),
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
pub(crate) fn open_url_in_default_browser(_url: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "opening URLs is only supported on Windows",
    ))
}
