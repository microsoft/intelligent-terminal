//! Small native Windows helpers used to call OS services directly instead of
//! routing through external shell helpers.

#[cfg(windows)]
use std::io;

/// Copilot CLI stores its OAuth credential in Windows Credential Manager under
/// target names containing `copilot-cli`. This predicate is deliberately kept
/// pure so the matching behavior is testable without touching a user's
/// Credential Manager store.
pub(crate) fn credential_target_matches_copilot(target: &str) -> bool {
    target.to_ascii_lowercase().contains("copilot-cli")
}

#[cfg(windows)]
unsafe fn wide_ptr_to_string(ptr: *const u16) -> String {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    OsString::from_wide(slice).to_string_lossy().into_owned()
}

#[cfg(windows)]
struct CredentialArray(*mut *mut windows_sys::Win32::Security::Credentials::CREDENTIALW);

#[cfg(windows)]
impl Drop for CredentialArray {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Security::Credentials::CredFree(self.0 as _);
        }
    }
}

/// Read-only Copilot credential presence check using the native Credential
/// Manager API. We inspect target names only; the credential secret/blob is
/// never read.
#[cfg(windows)]
pub(crate) fn copilot_credential_present() -> bool {
    use windows_sys::Win32::Security::Credentials::{CredEnumerateW, CREDENTIALW};

    let mut count = 0u32;
    let mut credentials: *mut *mut CREDENTIALW = std::ptr::null_mut();
    // Enumerate all targets and apply our own substring predicate to preserve
    // parity with the old shell-based substring probe. Copilot CLI has
    // used both prefix (`copilot-cli/...`) and suffix (`... .copilot-cli`)
    // target shapes; CredEnumerateW's filter is prefix-only and would miss the
    // suffix form. We still inspect target names only — never credential blobs.
    let ok = unsafe { CredEnumerateW(std::ptr::null(), 0, &mut count, &mut credentials) != 0 };
    if !ok || credentials.is_null() || count == 0 {
        return false;
    }

    let _guard = CredentialArray(credentials);
    let entries = unsafe { std::slice::from_raw_parts(credentials, count as usize) };
    entries.iter().any(|&cred| {
        if cred.is_null() {
            return false;
        }
        let target = unsafe { wide_ptr_to_string((*cred).TargetName) };
        credential_target_matches_copilot(&target)
    })
}

#[cfg(not(windows))]
pub(crate) fn copilot_credential_present() -> bool {
    false
}

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
        EmptyClipboard();
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

    if (result as isize) <= 32 {
        Err(io::Error::last_os_error())
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

#[cfg(test)]
mod tests {
    use super::credential_target_matches_copilot;

    #[test]
    fn copilot_credential_match_accepts_known_target_shapes() {
        assert!(credential_target_matches_copilot(
            "LegacyGeneric:target=copilot-cli/https://github.com:haonanttt"
        ));
        assert!(credential_target_matches_copilot(
            "LegacyGeneric:target=https://github.com:haonanttt.copilot-cli"
        ));
        assert!(credential_target_matches_copilot(
            "legacygeneric:target=COPILOT-CLI/https://example.ghe.com:user"
        ));
    }

    #[test]
    fn copilot_credential_match_rejects_unrelated_targets() {
        assert!(!credential_target_matches_copilot(""));
        assert!(!credential_target_matches_copilot(
            "LegacyGeneric:target=github.com:haonanttt"
        ));
        assert!(!credential_target_matches_copilot(
            "LegacyGeneric:target=other-agent-cli"
        ));
    }
}
