use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context};
use tokio::sync::mpsc;
use windows::core::{BSTR, GUID, HRESULT, IUnknown, Interface, PCWSTR};
use windows::Win32::Foundation::{SysAllocString, SysStringLen};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSIDFromString, CoTaskMemFree,
    CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED,
};

use crate::app::DebugMessage;
use super::WtChannel;

// ITerminalProtocolServer IID — must match the C++ IDL.
const IID_TERMINAL_PROTOCOL_SERVER: GUID = GUID::from_values(
    0x7B3F8A1E, 0x5C2D, 0x4E6F,
    [0x9A, 0x8B, 0x1D, 0x3E, 0x5F, 0x7A, 0x9B, 0x0C],
);

// ============================================================================
// MIDL struct equivalents — must match C layout exactly
// ============================================================================

#[repr(C)]
pub struct ProtocolWindowInfo {
    pub window_id: *mut u16,  // BSTR
    pub title: *mut u16,
    pub is_focused: i32,      // BOOL
    pub tab_count: u32,
}

#[repr(C)]
pub struct ProtocolTabInfo {
    pub tab_id: *mut u16,
    pub window_id: *mut u16,
    pub title: *mut u16,
    pub is_active: i32,
    pub pane_count: u32,
}

#[repr(C)]
pub struct ProtocolPaneInfo {
    pub pane_id: *mut u16,
    pub tab_id: *mut u16,
    pub window_id: *mut u16,
    pub title: *mut u16,
    pub profile: *mut u16,
    pub is_active: i32,
    pub pid: u32,
    pub rows: i32,
    pub columns: i32,
}

#[repr(C)]
pub struct ProtocolPaneOutput {
    pub pane_id: *mut u16,
    pub content: *mut u16,
    pub line_count: i32,
    pub truncated: i32,
}

#[repr(C)]
pub struct ProtocolProcessStatus {
    pub pane_id: *mut u16,
    pub state: *mut u16,
    pub pid: u32,
    pub exit_code: i32,
    pub has_exit_code: i32,
}

#[repr(C)]
pub struct ProtocolSessionVariable {
    pub pane_id: *mut u16,
    pub name: *mut u16,
    pub value: *mut u16,
    pub exists: i32,
}

#[repr(C)]
pub struct ProtocolTabCreationResult {
    pub tab_id: *mut u16,
    pub pane_id: *mut u16,
    pub window_id: *mut u16,
    pub pid: u32,
}

// ============================================================================
// BSTR helpers
// ============================================================================

/// Create a BSTR from a Rust &str via SysAllocString. Returns the raw pointer.
/// The caller must free with bstr_free() or BSTR::from_raw().
///
/// Returns null for empty strings. The automation proxy (oleautomation marshaling)
/// does not correctly marshal non-null empty BSTRs created by `BSTR::from("")`
/// in the `windows` crate. Returning null for empty is standard COM convention,
/// and the C++ server handles null BSTR `[in]` params gracefully (e.g.
/// `token ? winrt::to_string(...) : std::string{}`).
unsafe fn bstr_alloc(s: &str) -> *const u16 {
    if s.is_empty() {
        return std::ptr::null();
    }
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    SysAllocString(PCWSTR(wide.as_ptr())).as_ptr()
}

/// Read a BSTR pointer into a Rust String, then free it.
unsafe fn bstr_to_string_free(ptr: *mut u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let bstr = BSTR::from_raw(ptr);
    bstr.to_string()
}

/// Read a BSTR pointer into a Rust String without freeing (for struct members
/// that will be freed separately).
unsafe fn bstr_to_string(ptr: *mut u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // Wrap in ManuallyDrop so BSTR::from_raw doesn't free the pointer on drop.
    let bstr = std::mem::ManuallyDrop::new(BSTR::from_raw(ptr));
    let char_len = SysStringLen(&*bstr) as usize;
    let slice = std::slice::from_raw_parts(ptr, char_len);
    String::from_utf16_lossy(slice)
}

/// Free a BSTR without reading it.
unsafe fn bstr_free(ptr: *mut u16) {
    if !ptr.is_null() {
        let _ = BSTR::from_raw(ptr); // Drop frees it
    }
}

/// Free all BSTRs in a struct's fields, then free the array.
unsafe fn free_window_info_array(ptr: *mut ProtocolWindowInfo, count: u32) {
    if ptr.is_null() { return; }
    for i in 0..count as usize {
        let item = &*ptr.add(i);
        bstr_free(item.window_id);
        bstr_free(item.title);
    }
    CoTaskMemFree(Some(ptr as *const core::ffi::c_void));
}

unsafe fn free_tab_info_array(ptr: *mut ProtocolTabInfo, count: u32) {
    if ptr.is_null() { return; }
    for i in 0..count as usize {
        let item = &*ptr.add(i);
        bstr_free(item.tab_id);
        bstr_free(item.window_id);
        bstr_free(item.title);
    }
    CoTaskMemFree(Some(ptr as *const core::ffi::c_void));
}

unsafe fn free_pane_info_array(ptr: *mut ProtocolPaneInfo, count: u32) {
    if ptr.is_null() { return; }
    for i in 0..count as usize {
        let item = &*ptr.add(i);
        bstr_free(item.pane_id);
        bstr_free(item.tab_id);
        bstr_free(item.window_id);
        bstr_free(item.title);
        bstr_free(item.profile);
    }
    CoTaskMemFree(Some(ptr as *const core::ffi::c_void));
}

// ============================================================================
// COM vtable — must match IDL method order exactly
// ============================================================================

#[repr(C)]
#[allow(non_snake_case)]
struct ProtocolVtbl {
    // IUnknown (slots 0-2)
    QueryInterface: unsafe extern "system" fn(*mut core::ffi::c_void, *const GUID, *mut *mut core::ffi::c_void) -> HRESULT,
    AddRef: unsafe extern "system" fn(*mut core::ffi::c_void) -> u32,
    Release: unsafe extern "system" fn(*mut core::ffi::c_void) -> u32,

    // slot 3: HandleRequest (JSON fallback)
    HandleRequest: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16, *mut *mut u16) -> HRESULT,

    // slot 4: Authenticate
    Authenticate: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16, *mut i32, *mut *mut u16) -> HRESULT,

    // slot 5: GetCapabilities
    GetCapabilities: unsafe extern "system" fn(*mut core::ffi::c_void, *mut *mut u16, *mut *mut u16) -> HRESULT,

    // slot 6: GetActivePane
    GetActivePane: unsafe extern "system" fn(*mut core::ffi::c_void, *mut ProtocolPaneInfo) -> HRESULT,

    // slot 7: ListWindows
    ListWindows: unsafe extern "system" fn(*mut core::ffi::c_void, *mut u32, *mut *mut ProtocolWindowInfo) -> HRESULT,

    // slot 8: ListTabs
    ListTabs: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16, *mut u32, *mut *mut ProtocolTabInfo) -> HRESULT,

    // slot 9: ListPanes
    ListPanes: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16, *const u16, *mut u32, *mut *mut ProtocolPaneInfo) -> HRESULT,

    // slot 10: ReadPaneOutput
    ReadPaneOutput: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16, *const u16, i32, *mut ProtocolPaneOutput) -> HRESULT,

    // slot 11: GetProcessStatus
    GetProcessStatus: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16, *mut ProtocolProcessStatus) -> HRESULT,

    // slot 12: GetSessionVariable
    GetSessionVariable: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16, *const u16, *mut ProtocolSessionVariable) -> HRESULT,

    // slot 13: GetSettings
    GetSettings: unsafe extern "system" fn(*mut core::ffi::c_void, *mut *mut u16) -> HRESULT,

    // slot 14: CreateTab
    CreateTab: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16, *const u16, *const u16, *const u16, i32, i32, i32, *mut ProtocolTabCreationResult) -> HRESULT,

    // slot 15: SplitPane
    SplitPane: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16, *const u16, f32, *const u16, *const u16, i32, i32, *mut ProtocolTabCreationResult) -> HRESULT,

    // slot 16: ClosePane
    ClosePane: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16) -> HRESULT,

    // slot 17: SendInput
    SendInput: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16, *const u16) -> HRESULT,

    // slot 18: SetSessionVariable
    SetSessionVariable: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16, *const u16, *const u16) -> HRESULT,

    // slot 19: SetSettings
    SetSettings: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16, *mut *mut u16) -> HRESULT,
}

// ============================================================================
// Proxy wrapper
// ============================================================================

struct ProtocolServerProxy {
    ptr: *mut core::ffi::c_void,
}

impl ProtocolServerProxy {
    unsafe fn from_unknown(unk: &IUnknown) -> anyhow::Result<Self> {
        let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let hr = unk.query(
            &IID_TERMINAL_PROTOCOL_SERVER as *const GUID,
            &mut ptr as *mut *mut core::ffi::c_void,
        );
        hr.ok().context("QueryInterface for ITerminalProtocolServer failed")?;
        Ok(Self { ptr })
    }

    unsafe fn vtbl(&self) -> &ProtocolVtbl {
        &**(self.ptr as *const *const ProtocolVtbl)
    }

    // ── Typed method wrappers ──

    unsafe fn get_capabilities(&self) -> anyhow::Result<serde_json::Value> {
        let vt = self.vtbl();
        let mut ver_ptr: *mut u16 = std::ptr::null_mut();
        let mut methods_ptr: *mut u16 = std::ptr::null_mut();
        (vt.GetCapabilities)(self.ptr, &mut ver_ptr, &mut methods_ptr)
            .ok().context("GetCapabilities failed")?;
        let version = bstr_to_string_free(ver_ptr);
        let methods_json = bstr_to_string_free(methods_ptr);
        let methods: serde_json::Value = serde_json::from_str(&methods_json).unwrap_or_default();
        Ok(serde_json::json!({ "protocol_version": version, "methods": methods }))
    }

    unsafe fn authenticate(&self, token: &str) -> anyhow::Result<(bool, String)> {
        let vt = self.vtbl();
        let token_bstr = bstr_alloc(token);
        let mut authenticated: i32 = 0;
        let mut version_ptr: *mut u16 = std::ptr::null_mut();

        let hr = (vt.Authenticate)(self.ptr, token_bstr, &mut authenticated, &mut version_ptr);
        bstr_free(token_bstr as *mut u16);
        hr.ok().context("Authenticate failed")?;

        let version = bstr_to_string_free(version_ptr);
        Ok((authenticated != 0, version))
    }

    unsafe fn list_windows(&self) -> anyhow::Result<serde_json::Value> {
        let vt = self.vtbl();
        let mut count: u32 = 0;
        let mut results: *mut ProtocolWindowInfo = std::ptr::null_mut();

        (vt.ListWindows)(self.ptr, &mut count, &mut results)
            .ok().context("ListWindows failed")?;

        let mut windows = Vec::new();
        for i in 0..count as usize {
            let item = &*results.add(i);
            windows.push(serde_json::json!({
                "window_id": bstr_to_string(item.window_id),
                "title": bstr_to_string(item.title),
                "is_focused": item.is_focused != 0,
                "tab_count": item.tab_count,
            }));
        }
        free_window_info_array(results, count);

        Ok(serde_json::json!({ "windows": windows }))
    }

    unsafe fn list_tabs(&self, window_id: &str) -> anyhow::Result<serde_json::Value> {
        let vt = self.vtbl();
        let filter = bstr_alloc(window_id);
        let mut count: u32 = 0;
        let mut results: *mut ProtocolTabInfo = std::ptr::null_mut();

        let hr = (vt.ListTabs)(self.ptr, filter, &mut count, &mut results);
        bstr_free(filter as *mut u16);
        hr.ok().context("ListTabs failed")?;

        let mut tabs = Vec::new();
        for i in 0..count as usize {
            let item = &*results.add(i);
            tabs.push(serde_json::json!({
                "tab_id": bstr_to_string(item.tab_id),
                "window_id": bstr_to_string(item.window_id),
                "title": bstr_to_string(item.title),
                "is_active": item.is_active != 0,
                "pane_count": item.pane_count,
            }));
        }
        free_tab_info_array(results, count);

        Ok(serde_json::json!({ "tabs": tabs }))
    }

    unsafe fn list_panes(&self, window_id: &str, tab_id: &str) -> anyhow::Result<serde_json::Value> {
        let vt = self.vtbl();
        let wf = bstr_alloc(window_id);
        let tf = bstr_alloc(tab_id);
        let mut count: u32 = 0;
        let mut results: *mut ProtocolPaneInfo = std::ptr::null_mut();

        let hr = (vt.ListPanes)(self.ptr, wf, tf, &mut count, &mut results);
        // Free the BSTRs we allocated
        bstr_free(wf as *mut u16);
        bstr_free(tf as *mut u16);
        hr.ok().context("ListPanes failed")?;

        let mut panes = Vec::new();
        for i in 0..count as usize {
            let item = &*results.add(i);
            panes.push(serde_json::json!({
                "pane_id": bstr_to_string(item.pane_id),
                "tab_id": bstr_to_string(item.tab_id),
                "window_id": bstr_to_string(item.window_id),
                "title": bstr_to_string(item.title),
                "profile": bstr_to_string(item.profile),
                "is_active": item.is_active != 0,
                "pid": item.pid,
                "size": { "rows": item.rows, "columns": item.columns },
            }));
        }
        free_pane_info_array(results, count);

        Ok(serde_json::json!({ "panes": panes }))
    }

    unsafe fn get_active_pane(&self) -> anyhow::Result<serde_json::Value> {
        let vt = self.vtbl();
        let mut info: ProtocolPaneInfo = std::mem::zeroed();

        (vt.GetActivePane)(self.ptr, &mut info)
            .ok().context("GetActivePane failed")?;

        let result = serde_json::json!({
            "pane_id": bstr_to_string(info.pane_id),
            "tab_id": bstr_to_string(info.tab_id),
            "window_id": bstr_to_string(info.window_id),
            "title": bstr_to_string(info.title),
            "profile": bstr_to_string(info.profile),
            "is_active": info.is_active != 0,
            "pid": info.pid,
        });
        // Free BSTRs in the struct
        bstr_free(info.pane_id); bstr_free(info.tab_id); bstr_free(info.window_id);
        bstr_free(info.title); bstr_free(info.profile);
        Ok(result)
    }

    unsafe fn read_pane_output(&self, pane_id: &str, source: &str, max_lines: i32) -> anyhow::Result<serde_json::Value> {
        let vt = self.vtbl();
        let pid = bstr_alloc(pane_id);
        let src = bstr_alloc(source);
        let mut out: ProtocolPaneOutput = std::mem::zeroed();

        let hr = (vt.ReadPaneOutput)(self.ptr, pid, src, max_lines, &mut out);
        bstr_free(pid as *mut u16);
        bstr_free(src as *mut u16);
        hr.ok().context("ReadPaneOutput failed")?;

        let result = serde_json::json!({
            "pane_id": bstr_to_string(out.pane_id),
            "content": bstr_to_string(out.content),
            "line_count": out.line_count,
            "truncated": out.truncated != 0,
        });
        bstr_free(out.pane_id); bstr_free(out.content);
        Ok(result)
    }

    unsafe fn get_process_status(&self, pane_id: &str) -> anyhow::Result<serde_json::Value> {
        let vt = self.vtbl();
        let pid = bstr_alloc(pane_id);
        let mut out: ProtocolProcessStatus = std::mem::zeroed();

        let hr = (vt.GetProcessStatus)(self.ptr, pid, &mut out);
        bstr_free(pid as *mut u16);
        hr.ok().context("GetProcessStatus failed")?;

        let mut result = serde_json::json!({
            "pane_id": bstr_to_string(out.pane_id),
            "state": bstr_to_string(out.state),
            "pid": out.pid,
        });
        if out.has_exit_code != 0 {
            result["exit_code"] = serde_json::json!(out.exit_code);
        }
        bstr_free(out.pane_id); bstr_free(out.state);
        Ok(result)
    }

    unsafe fn get_session_variable(&self, pane_id: &str, name: &str) -> anyhow::Result<serde_json::Value> {
        let vt = self.vtbl();
        let pid = bstr_alloc(pane_id);
        let n = bstr_alloc(name);
        let mut out: ProtocolSessionVariable = std::mem::zeroed();

        let hr = (vt.GetSessionVariable)(self.ptr, pid, n, &mut out);
        bstr_free(pid as *mut u16);
        bstr_free(n as *mut u16);
        hr.ok().context("GetSessionVariable failed")?;

        let result = serde_json::json!({
            "pane_id": bstr_to_string(out.pane_id),
            "name": bstr_to_string(out.name),
            "value": if out.exists != 0 { serde_json::Value::String(bstr_to_string(out.value)) } else { serde_json::Value::Null },
            "exists": out.exists != 0,
        });
        bstr_free(out.pane_id); bstr_free(out.name); bstr_free(out.value);
        Ok(result)
    }

    unsafe fn get_settings(&self) -> anyhow::Result<serde_json::Value> {
        let vt = self.vtbl();
        let mut json_ptr: *mut u16 = std::ptr::null_mut();

        (vt.GetSettings)(self.ptr, &mut json_ptr)
            .ok().context("GetSettings failed")?;

        let content = bstr_to_string_free(json_ptr);
        Ok(serde_json::json!({ "settings": content }))
    }

    unsafe fn create_tab(&self, params: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let vt = self.vtbl();
        let window_id = bstr_alloc(params.get("window_id").and_then(|v| v.as_str()).unwrap_or(""));
        let profile = bstr_alloc(params.get("profile").and_then(|v| v.as_str()).unwrap_or(""));
        let commandline = bstr_alloc(params.get("commandline").and_then(|v| v.as_str()).unwrap_or(""));
        let title = bstr_alloc(params.get("title").and_then(|v| v.as_str()).unwrap_or(""));
        let suppress = if params.get("suppress_application_title").and_then(|v| v.as_bool()).unwrap_or(false) { 1i32 } else { 0 };
        let inject = if params.get("inject_mcp_credentials").and_then(|v| v.as_bool()).unwrap_or(false) { 1i32 } else { 0 };
        let bg = if params.get("background").and_then(|v| v.as_bool()).unwrap_or(true) { 1i32 } else { 0 };
        let mut out: ProtocolTabCreationResult = std::mem::zeroed();

        let hr = (vt.CreateTab)(self.ptr, window_id, profile, commandline,
                       title, suppress, inject, bg, &mut out);
        bstr_free(window_id as *mut u16); bstr_free(profile as *mut u16);
        bstr_free(commandline as *mut u16); bstr_free(title as *mut u16);
        hr.ok().context("CreateTab failed")?;

        let result = serde_json::json!({
            "tab_id": bstr_to_string(out.tab_id),
            "pane_id": bstr_to_string(out.pane_id),
            "window_id": bstr_to_string(out.window_id),
            "pid": out.pid,
        });
        bstr_free(out.tab_id); bstr_free(out.pane_id); bstr_free(out.window_id);
        Ok(result)
    }

    unsafe fn split_pane(&self, params: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let vt = self.vtbl();
        let pane_id = bstr_alloc(params.get("pane_id").and_then(|v| v.as_str()).unwrap_or(""));
        let direction = bstr_alloc(params.get("direction").and_then(|v| v.as_str()).unwrap_or("right"));
        let size = params.get("size").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
        let profile = bstr_alloc(params.get("profile").and_then(|v| v.as_str()).unwrap_or(""));
        let commandline = bstr_alloc(params.get("commandline").and_then(|v| v.as_str()).unwrap_or(""));
        let inject = if params.get("inject_mcp_credentials").and_then(|v| v.as_bool()).unwrap_or(false) { 1i32 } else { 0 };
        let bg = if params.get("background").and_then(|v| v.as_bool()).unwrap_or(true) { 1i32 } else { 0 };
        let mut out: ProtocolTabCreationResult = std::mem::zeroed();

        let hr = (vt.SplitPane)(self.ptr, pane_id, direction, size,
                       profile, commandline, inject, bg, &mut out);
        bstr_free(pane_id as *mut u16); bstr_free(direction as *mut u16);
        bstr_free(profile as *mut u16); bstr_free(commandline as *mut u16);
        hr.ok().context("SplitPane failed")?;

        let result = serde_json::json!({
            "tab_id": bstr_to_string(out.tab_id),
            "pane_id": bstr_to_string(out.pane_id),
            "window_id": bstr_to_string(out.window_id),
            "pid": out.pid,
        });
        bstr_free(out.tab_id); bstr_free(out.pane_id); bstr_free(out.window_id);
        Ok(result)
    }

    unsafe fn close_pane(&self, pane_id: &str) -> anyhow::Result<serde_json::Value> {
        let vt = self.vtbl();
        let pid = bstr_alloc(pane_id);
        let hr = (vt.ClosePane)(self.ptr, pid);
        bstr_free(pid as *mut u16);
        hr.ok().context("ClosePane failed")?;
        Ok(serde_json::json!({ "closed": true }))
    }

    unsafe fn send_input(&self, pane_id: &str, text: &str) -> anyhow::Result<serde_json::Value> {
        let vt = self.vtbl();
        let pid = bstr_alloc(pane_id);
        let t = bstr_alloc(text);
        let hr = (vt.SendInput)(self.ptr, pid, t);
        bstr_free(pid as *mut u16);
        bstr_free(t as *mut u16);
        hr.ok().context("SendInput failed")?;
        Ok(serde_json::json!({ "sent": true }))
    }

    unsafe fn set_session_variable(&self, pane_id: &str, name: &str, value: &str) -> anyhow::Result<serde_json::Value> {
        let vt = self.vtbl();
        let pid = bstr_alloc(pane_id);
        let n = bstr_alloc(name);
        let v = bstr_alloc(value);
        let hr = (vt.SetSessionVariable)(self.ptr, pid, n, v);
        bstr_free(pid as *mut u16);
        bstr_free(n as *mut u16);
        bstr_free(v as *mut u16);
        hr.ok().context("SetSessionVariable failed")?;
        Ok(serde_json::json!({ "set": true }))
    }

    unsafe fn set_settings(&self, content: &str) -> anyhow::Result<serde_json::Value> {
        let vt = self.vtbl();
        let c = bstr_alloc(content);
        let mut backup_ptr: *mut u16 = std::ptr::null_mut();
        let hr = (vt.SetSettings)(self.ptr, c, &mut backup_ptr);
        bstr_free(c as *mut u16);
        hr.ok().context("SetSettings failed")?;
        let backup = bstr_to_string_free(backup_ptr);
        Ok(serde_json::json!({ "applied": true, "backup_path": backup }))
    }
}

impl Drop for ProtocolServerProxy {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { (self.vtbl().Release)(self.ptr); }
        }
    }
}

unsafe impl Send for ProtocolServerProxy {}
unsafe impl Sync for ProtocolServerProxy {}

// ============================================================================
// ComChannel — typed COM channel implementing WtChannel
// ============================================================================

pub struct ComChannel {
    server: ProtocolServerProxy,
    available: AtomicBool,
    debug_tx: Option<mpsc::UnboundedSender<DebugMessage>>,
}

unsafe impl Send for ComChannel {}
unsafe impl Sync for ComChannel {}

impl ComChannel {
    pub async fn connect() -> anyhow::Result<Self> {
        let clsid_str = std::env::var("WT_COM_CLSID")
            .context("WT_COM_CLSID not set.")?;
        let token = std::env::var("WT_MCP_TOKEN").unwrap_or_default();
        Self::connect_with(&clsid_str, &token).await
    }

    pub async fn connect_with(clsid_str: &str, token: &str) -> anyhow::Result<Self> {
        let token = token.to_string();
        let clsid_str = clsid_str.to_string();

        let server = tokio::task::spawn_blocking(move || -> anyhow::Result<ProtocolServerProxy> {
            unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED)
                    .ok()
                    .map_err(|e| anyhow::anyhow!(
                        "CoInitializeEx failed (thread may have incompatible COM apartment): {}",
                        e.message()
                    ))?;

                let clsid_wide: Vec<u16> = clsid_str.encode_utf16().chain(std::iter::once(0)).collect();
                let clsid = CLSIDFromString(windows::core::PCWSTR(clsid_wide.as_ptr()))
                    .context(format!("Invalid CLSID: {}", clsid_str))?;

                let unk: IUnknown = CoCreateInstance(&clsid, None, CLSCTX_LOCAL_SERVER)
                    .map_err(|e| anyhow::anyhow!(
                        "CoCreateInstance({}) failed: HRESULT 0x{:08X} ({})",
                        clsid_str, e.code().0 as u32, e.message()
                    ))?;

                ProtocolServerProxy::from_unknown(&unk)
            }
        }).await??;

        let (authenticated, _version) = unsafe { server.authenticate(&token)? };
        if !authenticated {
            bail!("Authentication rejected by Windows Terminal");
        }

        let channel = Self {
            server,
            available: AtomicBool::new(true),
            debug_tx: None,
        };
        Ok(channel)
    }

    pub fn with_debug_sender(mut self, tx: mpsc::UnboundedSender<DebugMessage>) -> Self {
        self.debug_tx = Some(tx);
        self
    }

    fn emit_debug(&self, direction: crate::app::DebugDir, content: String) {
        if let Some(ref tx) = self.debug_tx {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            let _ = tx.send(DebugMessage { timestamp: ts, direction, content });
        }
    }
}

#[async_trait::async_trait]
impl WtChannel for ComChannel {
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        self.emit_debug(crate::app::DebugDir::Sent, format!("COM:{}({})", method, params));

        let result = unsafe {
            match method {
                "authenticate" => {
                    let token = params.get("token").and_then(|v| v.as_str()).unwrap_or("");
                    let (auth, version) = self.server.authenticate(token)?;
                    serde_json::json!({ "authenticated": auth, "protocol_version": version })
                }
                "get_capabilities" => self.server.get_capabilities()?,
                "get_active_pane" => self.server.get_active_pane()?,
                "list_windows" => self.server.list_windows()?,
                "list_tabs" => {
                    let wid = params.get("window_id").and_then(|v| v.as_str()).unwrap_or("");
                    self.server.list_tabs(wid)?
                }
                "list_panes" => {
                    let wid = params.get("window_id").and_then(|v| v.as_str()).unwrap_or("");
                    let tid = params.get("tab_id").and_then(|v| v.as_str()).unwrap_or("");
                    self.server.list_panes(wid, tid)?
                }
                "read_pane_output" => {
                    let pid = params.get("pane_id").and_then(|v| v.as_str()).unwrap_or("");
                    let src = params.get("source").and_then(|v| v.as_str()).unwrap_or("scrollback");
                    let max = params.get("max_lines").and_then(|v| v.as_i64()).unwrap_or(200) as i32;
                    self.server.read_pane_output(pid, src, max)?
                }
                "get_process_status" => {
                    let pid = params.get("pane_id").and_then(|v| v.as_str()).unwrap_or("");
                    self.server.get_process_status(pid)?
                }
                "get_session_variable" => {
                    let pid = params.get("pane_id").and_then(|v| v.as_str()).unwrap_or("");
                    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    self.server.get_session_variable(pid, name)?
                }
                "get_settings" => self.server.get_settings()?,
                "create_tab" => self.server.create_tab(&params)?,
                "split_pane" => self.server.split_pane(&params)?,
                "close_pane" => {
                    let pid = params.get("pane_id").and_then(|v| v.as_str()).unwrap_or("");
                    self.server.close_pane(pid)?
                }
                "send_input" => {
                    let pid = params.get("pane_id").and_then(|v| v.as_str()).unwrap_or("");
                    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    self.server.send_input(pid, text)?
                }
                "set_session_variable" => {
                    let pid = params.get("pane_id").and_then(|v| v.as_str()).unwrap_or("");
                    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("");
                    self.server.set_session_variable(pid, name, value)?
                }
                "set_settings" => {
                    let content = params.get("settings").and_then(|v| v.as_str()).unwrap_or("");
                    self.server.set_settings(content)?
                }
                other => bail!("Unknown method: {}", other),
            }
        };

        self.emit_debug(crate::app::DebugDir::Received, format!("{}", result));
        Ok(result)
    }

    fn is_available(&self) -> bool {
        self.available.load(Ordering::Relaxed)
    }
}
