use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

pub struct PipeSecurity {
    sa: windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
    psd: *mut std::ffi::c_void,
}

impl PipeSecurity {
    fn sa_ptr(&self) -> *mut std::ffi::c_void {
        &self.sa as *const _ as *mut std::ffi::c_void
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        if !self.psd.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::LocalFree(self.psd);
            }
        }
    }
}

pub fn build() -> Option<PipeSecurity> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;

    let user_sid = current_user_sid_string()?;
    let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{user_sid})S:(ML;;NW;;;ME)");
    let sddl_w: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut psd: *mut std::ffi::c_void = std::ptr::null_mut();
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_w.as_ptr(),
            SDDL_REVISION_1 as u32,
            &mut psd,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 || psd.is_null() {
        tracing::warn!(
            target: "named_pipe",
            "failed to build current-user security descriptor"
        );
        return None;
    }

    Some(PipeSecurity {
        sa: SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: psd,
            bInheritHandle: 0,
        },
        psd,
    })
}

pub fn build_required() -> std::io::Result<PipeSecurity> {
    build().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "failed to build the current-user named-pipe security descriptor",
        )
    })
}

pub fn create_server(
    pipe_name: &str,
    first_instance: bool,
    security: Option<&PipeSecurity>,
) -> std::io::Result<NamedPipeServer> {
    let mut options = ServerOptions::new();
    options.first_pipe_instance(first_instance);
    options.reject_remote_clients(true);
    match security {
        Some(security) => unsafe {
            options.create_with_security_attributes_raw(pipe_name, security.sa_ptr())
        },
        None => options.create(pipe_name),
    }
}

fn current_user_sid_string() -> Option<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut len = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
        if len == 0 {
            CloseHandle(token);
            return None;
        }
        let mut buffer = vec![0u8; len as usize];
        let ok = GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), len, &mut len);
        CloseHandle(token);
        if ok == 0 {
            return None;
        }
        let token_user = std::ptr::read_unaligned(buffer.as_ptr().cast::<TOKEN_USER>());
        let mut sid_string: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(token_user.User.Sid, &mut sid_string) == 0 || sid_string.is_null()
        {
            return None;
        }
        let mut length = 0;
        while *sid_string.add(length) != 0 {
            length += 1;
        }
        let result = String::from_utf16_lossy(std::slice::from_raw_parts(sid_string, length));
        LocalFree(sid_string.cast());
        Some(result)
    }
}
