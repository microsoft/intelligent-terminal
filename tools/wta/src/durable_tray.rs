//! External notification-area host for detached durable shell sessions.

use std::collections::{HashMap, HashSet};
use std::ffi::{c_void, OsStr};
use std::hash::{Hash, Hasher};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use rust_i18n::t;
use serde_json::Value;
use uuid::Uuid;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
};
use windows_sys::Win32::UI::Shell::{
    ExtractIconExW, ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetWindowLongPtrW, LoadIconW,
    MessageBoxW, PostMessageW, PostQuitMessage, SetForegroundWindow, SetTimer, SetWindowLongPtrW,
    TrackPopupMenu, TranslateMessage, CREATESTRUCTW, GWLP_USERDATA, HICON, IDI_APPLICATION,
    MB_ICONERROR, MB_OK, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
    TPM_RIGHTBUTTON, WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONUP, WM_NCCREATE,
    WM_NULL, WM_RBUTTONUP, WM_TIMER,
};

use crate::shell_session_store::{
    current_process_is_elevated, ShellSessionDeleteParams, ShellSessionRecord, ShellSessionStore,
};

const TRAY_MESSAGE: u32 = WM_APP + 41;
const POLL_TIMER: usize = 1;
const POLL_INTERVAL_MS: u32 = 2_000;
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const FIRST_COMMAND_ID: usize = 1_000;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const SW_SHOWNORMAL: i32 = 1;
const CURRENT_HOST_FILE: &str = "current-host.txt";

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(module_name: *const u16) -> *mut c_void;
    fn GetCurrentApplicationUserModelId(
        length: *mut u32,
        application_user_model_id: *mut u16,
    ) -> i32;
    fn GetPackagesByPackageFamily(
        package_family_name: *const u16,
        count: *mut u32,
        package_full_names: *mut *mut u16,
        buffer_length: *mut u32,
        buffer: *mut u16,
    ) -> i32;
    fn GetPackagePathByFullName(
        package_full_name: *const u16,
        path_length: *mut u32,
        path: *mut u16,
    ) -> i32;
}

#[repr(C)]
// `windows-sys` gates WNDCLASSW behind its GDI feature even though this
// message-only host uses no GDI. Keep the ABI-sized declaration local instead
// of expanding the dependency feature set.
struct WindowClass {
    style: u32,
    window_proc: Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>,
    class_extra: i32,
    window_extra: i32,
    instance: *mut c_void,
    icon: *mut c_void,
    cursor: *mut c_void,
    background: *mut c_void,
    menu_name: *const u16,
    class_name: *const u16,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(window_class: *const WindowClass) -> u16;
}

#[derive(Debug, Clone)]
pub struct TrayArgs {
    pub host: bool,
    pub wtcli: Option<PathBuf>,
    pub aumid: Option<String>,
    pub icon_source: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PsmuxSession {
    name: String,
    session_id: Uuid,
    attached: bool,
}

#[derive(Debug, Clone)]
struct BackgroundRecord {
    record: ShellSessionRecord,
}

#[derive(Debug, Clone)]
enum MenuAction {
    Restore(String),
    End(String),
    EndAll,
}

pub async fn run(args: TrayArgs) -> Result<()> {
    if !args.host {
        return bootstrap();
    }

    let wtcli = args
        .wtcli
        .ok_or_else(|| anyhow!("cached tray host is missing --wtcli"))?;
    let aumid = args
        .aumid
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("cached tray host is missing --aumid"))?;
    let icon_source = args
        .icon_source
        .ok_or_else(|| anyhow!("cached tray host is missing --icon-source"))?;
    let mutex = acquire_singleton()?;
    let Some(mutex) = mutex else {
        tracing::info!(target: "durable_tray", "tray singleton already running");
        return Ok(());
    };

    let store = ShellSessionStore::open_runtime()
        .await
        .context("failed to open durable shell-session store")?;
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let _mutex = mutex;
        TrayHost::new(handle, store, wtcli, aumid, icon_source).run()
    })
    .await
    .context("durable tray UI thread panicked")?
}

fn bootstrap() -> Result<()> {
    let source = std::env::current_exe().context("failed to resolve packaged wta.exe")?;
    let source_dir = source
        .parent()
        .ok_or_else(|| anyhow!("packaged wta.exe has no parent directory"))?;
    let wtcli = source_dir.join("wtcli.exe");
    if !wtcli.exists() {
        bail!("packaged wtcli.exe not found at {}", wtcli.display());
    }
    let icon_source = source_dir.join("WindowsTerminal.exe");
    if !icon_source.exists() {
        bail!(
            "packaged WindowsTerminal.exe not found at {}",
            icon_source.display()
        );
    }
    let aumid = current_aumid().context("failed to capture packaged application identity")?;
    let local_root = crate::runtime_paths::intelligent_terminal_local_root()
        .ok_or_else(|| anyhow!("Intelligent Terminal local cache root is unavailable"))?;
    let package_version = crate::logging::package_version();
    let binary_version = dev_binary_version();
    let host_version = package_version
        .as_ref()
        .map_or(binary_version.clone(), |version| {
            format!("{version}-{binary_version}")
        });
    let host_dir = local_root.join("durable-host").join(host_version);
    std::fs::create_dir_all(&host_dir)
        .with_context(|| format!("failed to create {}", host_dir.display()))?;
    let cached = host_dir.join("wta.exe");
    if !same_path(&source, &cached) && !cached.exists() {
        let temporary = host_dir.join(format!("wta-{}.tmp", Uuid::new_v4()));
        std::fs::copy(&source, &temporary).with_context(|| {
            format!(
                "failed to stage durable tray host from {} to {}",
                source.display(),
                temporary.display()
            )
        })?;
        if let Err(error) = std::fs::rename(&temporary, &cached) {
            let _ = std::fs::remove_file(&temporary);
            if !cached.exists() {
                return Err(error).with_context(|| {
                    format!("failed to publish durable tray host {}", cached.display())
                });
            }
        }
    }
    let durable_host_root = local_root.join("durable-host");
    std::fs::write(
        durable_host_root.join(CURRENT_HOST_FILE),
        cached.to_string_lossy().as_bytes(),
    )
    .context("failed to publish the current durable tray host path")?;

    let mut child = Command::new(&cached);
    child
        .arg("tray")
        .arg("--host")
        .arg("--wtcli")
        .arg(&wtcli)
        .arg("--aumid")
        .arg(&aumid)
        .arg("--icon-source")
        .arg(&icon_source)
        .env("WTA_LOCAL_ROOT", &local_root)
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    if let Some(version) = package_version {
        child.env("WTA_PACKAGE_VERSION", version);
    }
    child
        .spawn()
        .with_context(|| format!("failed to start cached tray host {}", cached.display()))?;
    tracing::info!(
        target: "durable_tray",
        cached_path = %cached.display(),
        wtcli_path = %wtcli.display(),
        aumid,
        "started cached durable tray host"
    );
    Ok(())
}

fn dev_binary_version() -> String {
    let metadata = std::fs::metadata(std::env::current_exe().unwrap_or_default()).ok();
    let size = metadata.as_ref().map_or(0, std::fs::Metadata::len);
    let modified = metadata
        .and_then(|value| value.modified().ok())
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |value| value.as_secs());
    format!("dev-{size:x}-{modified:x}")
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn current_aumid() -> Result<String> {
    let mut length = 0u32;
    let first = unsafe { GetCurrentApplicationUserModelId(&mut length, std::ptr::null_mut()) };
    if first != windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER as i32 || length == 0 {
        bail!("GetCurrentApplicationUserModelId sizing failed with {first}");
    }
    let mut buffer = vec![0u16; length as usize];
    let result = unsafe { GetCurrentApplicationUserModelId(&mut length, buffer.as_mut_ptr()) };
    if result != 0 {
        bail!("GetCurrentApplicationUserModelId failed with {result}");
    }
    let end = buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(buffer.len());
    Ok(String::from_utf16_lossy(&buffer[..end]))
}

struct OwnedHandle(isize);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                CloseHandle(self.0 as *mut c_void);
            }
        }
    }
}

fn acquire_singleton() -> Result<Option<OwnedHandle>> {
    let root = crate::runtime_paths::shell_session_runtime_root()
        .ok_or_else(|| anyhow!("shell-session runtime root is unavailable"))?;
    let executable = std::env::current_exe().context("failed to resolve tray host executable")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.to_string_lossy().to_lowercase().hash(&mut hasher);
    executable
        .to_string_lossy()
        .to_lowercase()
        .hash(&mut hasher);
    current_process_is_elevated().hash(&mut hasher);
    let name = wide(format!(
        "Local\\IntelligentTerminal.DurableTray.{:016x}",
        hasher.finish()
    ));
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        bail!("CreateMutexW failed with {}", unsafe { GetLastError() });
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            CloseHandle(handle);
        }
        return Ok(None);
    }
    Ok(Some(OwnedHandle(handle as isize)))
}

struct TrayHost {
    runtime: tokio::runtime::Handle,
    store: ShellSessionStore,
    wtcli: PathBuf,
    aumid: String,
    icon: HICON,
    hwnd: HWND,
    icon_visible: bool,
    records: Vec<BackgroundRecord>,
    actions: HashMap<usize, MenuAction>,
    saw_session: bool,
    empty_polls: u8,
    last_keep_alive: Option<Instant>,
}

impl TrayHost {
    fn new(
        runtime: tokio::runtime::Handle,
        store: ShellSessionStore,
        wtcli: PathBuf,
        aumid: String,
        icon_source: PathBuf,
    ) -> Self {
        Self {
            runtime,
            store,
            wtcli,
            aumid,
            icon: extract_app_icon(&icon_source),
            hwnd: std::ptr::null_mut(),
            icon_visible: false,
            records: Vec::new(),
            actions: HashMap::new(),
            saw_session: false,
            empty_polls: 0,
            last_keep_alive: None,
        }
    }

    fn run(mut self) -> Result<()> {
        let class_name = wide("IntelligentTerminalDurableTray");
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let window_class = WindowClass {
            style: 0,
            window_proc: Some(window_proc),
            class_extra: 0,
            window_extra: 0,
            instance,
            icon: std::ptr::null_mut(),
            cursor: std::ptr::null_mut(),
            background: std::ptr::null_mut(),
            menu_name: std::ptr::null(),
            class_name: class_name.as_ptr(),
        };
        if unsafe { RegisterClassW(&window_class) } == 0 {
            let error = unsafe { GetLastError() };
            if error != 1410 {
                bail!("RegisterClassW failed with {error}");
            }
        }
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                (&mut self as *mut Self).cast(),
            )
        };
        if hwnd.is_null() {
            bail!("CreateWindowExW failed with {}", unsafe { GetLastError() });
        }
        self.hwnd = hwnd;
        unsafe {
            SetTimer(hwnd, POLL_TIMER, POLL_INTERVAL_MS, None);
        }
        self.refresh();

        let mut message = MSG::default();
        while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        self.remove_icon();
        if !self.icon.is_null() {
            unsafe {
                DestroyIcon(self.icon);
            }
            self.icon = std::ptr::null_mut();
        }
        tracing::info!(target: "durable_tray", "durable tray host stopped");
        Ok(())
    }

    fn refresh(&mut self) {
        if !is_current_host() {
            tracing::info!(
                target: "durable_tray",
                "a newer durable tray host is available; stopping this host"
            );
            unsafe {
                DestroyWindow(self.hwnd);
            }
            return;
        }
        match list_psmux_sessions() {
            Ok(sessions) => {
                self.saw_session |= !sessions.is_empty();
                if sessions.is_empty() {
                    self.empty_polls = self.empty_polls.saturating_add(1);
                    self.records.clear();
                    self.update_icon();
                    if (self.saw_session && self.empty_polls >= 2) || self.empty_polls >= 5 {
                        unsafe {
                            DestroyWindow(self.hwnd);
                        }
                    }
                    return;
                }
                self.empty_polls = 0;
                let elevated = current_process_is_elevated();
                match self.runtime.block_on(self.store.list_records(elevated)) {
                    Ok(records) => {
                        let live_record_ids = map_live_record_ids(&records, &sessions);
                        self.records = map_detached_records(&records, &sessions);
                        self.keep_records_alive(live_record_ids);
                        self.update_icon();
                    }
                    Err(error) => {
                        tracing::error!(
                            target: "durable_tray",
                            error = ?error,
                            "failed to read durable session records"
                        );
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    target: "durable_tray",
                    error = ?error,
                    "failed to poll psmux sessions"
                );
            }
        }
    }

    fn keep_records_alive(&mut self, ids: Vec<String>) {
        if ids.is_empty()
            || self
                .last_keep_alive
                .is_some_and(|last| last.elapsed() < KEEP_ALIVE_INTERVAL)
        {
            return;
        }
        let elevated = current_process_is_elevated();
        match self
            .runtime
            .block_on(self.store.touch_records(ids, elevated))
        {
            Ok(()) => self.last_keep_alive = Some(Instant::now()),
            Err(error) => tracing::warn!(
                target: "durable_tray",
                error = ?error,
                "failed to refresh durable session retention"
            ),
        }
    }

    fn update_icon(&mut self) {
        let count = self.records.len();
        if count == 0 {
            self.remove_icon();
            return;
        }
        let mut data = self.icon_data();
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.uCallbackMessage = TRAY_MESSAGE;
        data.hIcon = if self.icon.is_null() {
            unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) }
        } else {
            self.icon
        };
        copy_wide(
            &mut data.szTip,
            &t!("tray.tooltip", count = count).into_owned(),
        );
        let operation = if self.icon_visible {
            NIM_MODIFY
        } else {
            NIM_ADD
        };
        if unsafe { Shell_NotifyIconW(operation, &data) } != 0 {
            self.icon_visible = true;
        } else if operation == NIM_MODIFY && unsafe { Shell_NotifyIconW(NIM_ADD, &data) } != 0 {
            self.icon_visible = true;
            tracing::info!(
                target: "durable_tray",
                "notification icon was recreated after its shell host restarted"
            );
        } else {
            self.icon_visible = false;
            tracing::error!(
                target: "durable_tray",
                operation,
                "failed to update notification icon"
            );
        }
    }

    fn remove_icon(&mut self) {
        if self.icon_visible {
            let data = self.icon_data();
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &data);
            }
            self.icon_visible = false;
        }
    }

    fn icon_data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            ..Default::default()
        }
    }

    fn show_menu(&mut self) {
        if self.records.is_empty() {
            return;
        }
        self.actions.clear();
        let root = unsafe { CreatePopupMenu() };
        if root.is_null() {
            return;
        }
        let mut command_id = FIRST_COMMAND_ID;
        for background in &self.records {
            if command_id > u16::MAX as usize - 2 {
                tracing::warn!(
                    target: "durable_tray",
                    displayed = self.records.len(),
                    "tray menu reached the Win32 command-id limit"
                );
                break;
            }
            let submenu = unsafe { CreatePopupMenu() };
            if submenu.is_null() {
                continue;
            }
            let restore_id = command_id;
            command_id += 1;
            let end_id = command_id;
            command_id += 1;
            self.actions.insert(
                restore_id,
                MenuAction::Restore(background.record.id.clone()),
            );
            self.actions
                .insert(end_id, MenuAction::End(background.record.id.clone()));
            append_menu(submenu, MF_STRING, restore_id, &t!("tray.restore"));
            append_menu(submenu, MF_STRING, end_id, &t!("tray.end"));
            append_menu(root, MF_POPUP, submenu as usize, &background.record.name);
        }
        append_menu(root, MF_SEPARATOR, 0, "");
        let end_all_id = command_id;
        self.actions.insert(end_all_id, MenuAction::EndAll);
        append_menu(root, MF_STRING, end_all_id, &t!("tray.end_all"));

        let mut point = POINT::default();
        unsafe {
            GetCursorPos(&mut point);
            SetForegroundWindow(self.hwnd);
            TrackPopupMenu(
                root,
                TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                self.hwnd,
                std::ptr::null(),
            );
            PostMessageW(self.hwnd, WM_NULL, 0, 0);
            DestroyMenu(root);
        }
    }

    fn invoke(&mut self, command_id: usize) {
        let Some(action) = self.actions.get(&command_id).cloned() else {
            return;
        };
        let result = match action {
            MenuAction::Restore(id) => self.restore(&id),
            MenuAction::End(id) => self.end(&id),
            MenuAction::EndAll => self.end_all(),
        };
        if let Err(error) = result {
            tracing::error!(
                target: "durable_tray",
                command_id,
                error = ?error,
                "durable tray action failed"
            );
            self.show_error(&error);
        }
        self.refresh();
    }

    fn restore(&self, id: &str) -> Result<()> {
        match self
            .current_wtcli()
            .and_then(|wtcli| run_restore(&wtcli, id))
        {
            Ok(()) => {
                tracing::info!(target: "durable_tray", durable_id = id, "restored durable session");
                return Ok(());
            }
            Err(error) => {
                tracing::debug!(
                    target: "durable_tray",
                    durable_id = id,
                    error = ?error,
                    "Terminal is not ready for immediate durable restore; activating package"
                );
            }
        }

        let target = wide(format!("shell:AppsFolder\\{}", self.aumid));
        let verb = current_process_is_elevated().then(|| wide("runas"));
        let result = unsafe {
            ShellExecuteW(
                self.hwnd,
                verb.as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if result as isize <= 32 {
            bail!("ShellExecuteW failed with {}", result as isize);
        }
        let mut last_error = None;
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(500));
            match self
                .current_wtcli()
                .and_then(|wtcli| run_restore(&wtcli, id))
            {
                Ok(()) => {
                    tracing::info!(
                        target: "durable_tray",
                        durable_id = id,
                        "activated Intelligent Terminal and restored durable session"
                    );
                    return Ok(());
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("restore command did not become ready")))
    }

    fn current_wtcli(&self) -> Result<PathBuf> {
        match resolve_packaged_wtcli(&self.aumid) {
            Ok(path) => Ok(path),
            Err(error) if self.wtcli.exists() => {
                tracing::debug!(
                    target: "durable_tray",
                    error = ?error,
                    fallback = %self.wtcli.display(),
                    "failed to resolve the current package; trying captured wtcli"
                );
                Ok(self.wtcli.clone())
            }
            Err(error) => Err(error),
        }
    }

    fn end(&self, id: &str) -> Result<()> {
        let background = self
            .records
            .iter()
            .find(|value| value.record.id == id)
            .ok_or_else(|| anyhow!("durable session {id} is no longer displayed"))?;
        let live_sessions = list_psmux_sessions()?;
        let mut failures = Vec::new();
        for session in map_record_sessions(&background.record, &live_sessions) {
            if let Err(error) = kill_psmux_session_if_present(&session.name) {
                failures.push(format!("{}: {error:#}", session.name));
            }
        }
        if !failures.is_empty() {
            bail!("failed to end psmux sessions: {}", failures.join("; "));
        }
        let response = self
            .runtime
            .block_on(self.store.delete(ShellSessionDeleteParams {
                id: id.to_string(),
                elevated: current_process_is_elevated(),
            }))?;
        if !response.deleted {
            bail!("durable session {id} was not found in its elevation scope");
        }
        tracing::info!(target: "durable_tray", durable_id = id, "ended durable session");
        Ok(())
    }

    fn end_all(&self) -> Result<()> {
        let mut failures = Vec::new();
        for id in self
            .records
            .iter()
            .map(|value| value.record.id.clone())
            .collect::<Vec<_>>()
        {
            if let Err(error) = self.end(&id) {
                failures.push(format!("{id}: {error:#}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            bail!(
                "one or more durable sessions could not be ended: {}",
                failures.join("; ")
            )
        }
    }

    fn show_error(&self, error: &anyhow::Error) {
        let title = wide(t!("tray.error_title").as_ref());
        let body = wide(t!("tray.error_body", error = format!("{error:#}")).as_ref());
        unsafe {
            MessageBoxW(
                self.hwnd,
                body.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

fn resolve_packaged_wtcli(aumid: &str) -> Result<PathBuf> {
    let (package_family, _) = aumid
        .split_once('!')
        .ok_or_else(|| anyhow!("invalid packaged application id {aumid}"))?;
    let names = package_full_names(package_family)?;
    let latest_version = names
        .iter()
        .map(|name| package_version_key(name))
        .max()
        .ok_or_else(|| anyhow!("no installed package found for family {package_family}"))?;
    let mut failures = Vec::new();
    for full_name in names
        .iter()
        .filter(|name| package_version_key(name) == latest_version)
    {
        match package_path(full_name) {
            Ok(path) => {
                let wtcli = path.join("wtcli.exe");
                if wtcli.exists() {
                    return Ok(wtcli);
                }
                failures.push(format!("{} has no wtcli.exe", path.display()));
            }
            Err(error) => failures.push(format!("{full_name}: {error:#}")),
        }
    }
    bail!(
        "latest package {latest_version:?} for family {package_family} has no usable wtcli.exe: {}",
        failures.join("; ")
    );
}

fn package_full_names(package_family: &str) -> Result<Vec<String>> {
    let family = wide(package_family);
    let mut count = 0u32;
    let mut buffer_length = 0u32;
    let first = unsafe {
        GetPackagesByPackageFamily(
            family.as_ptr(),
            &mut count,
            std::ptr::null_mut(),
            &mut buffer_length,
            std::ptr::null_mut(),
        )
    };
    if first != windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER as i32
        || count == 0
        || buffer_length == 0
    {
        bail!("GetPackagesByPackageFamily sizing failed with {first}");
    }

    for _ in 0..3 {
        let mut names = vec![std::ptr::null_mut(); count as usize];
        let mut buffer = vec![0u16; buffer_length as usize];
        let result = unsafe {
            GetPackagesByPackageFamily(
                family.as_ptr(),
                &mut count,
                names.as_mut_ptr(),
                &mut buffer_length,
                buffer.as_mut_ptr(),
            )
        };
        if result == windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER as i32 {
            continue;
        }
        if result != 0 {
            bail!("GetPackagesByPackageFamily failed with {result}");
        }

        let buffer_start = buffer.as_ptr() as usize;
        let buffer_end = buffer_start + buffer.len() * std::mem::size_of::<u16>();
        let mut full_names = Vec::new();
        for name in names.into_iter().take(count as usize) {
            let address = name as usize;
            if name.is_null() || address < buffer_start || address >= buffer_end {
                continue;
            }
            let offset = (address - buffer_start) / std::mem::size_of::<u16>();
            let length = buffer[offset..]
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(buffer.len() - offset);
            full_names.push(String::from_utf16_lossy(&buffer[offset..offset + length]));
        }
        return Ok(full_names);
    }
    bail!("package registrations kept changing while resolving {package_family}");
}

fn package_path(full_name: &str) -> Result<PathBuf> {
    let full_name = wide(full_name);
    let mut length = 0u32;
    let first =
        unsafe { GetPackagePathByFullName(full_name.as_ptr(), &mut length, std::ptr::null_mut()) };
    if first != windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER as i32 || length == 0 {
        bail!("GetPackagePathByFullName sizing failed with {first}");
    }
    let mut buffer = vec![0u16; length as usize];
    let result =
        unsafe { GetPackagePathByFullName(full_name.as_ptr(), &mut length, buffer.as_mut_ptr()) };
    if result != 0 {
        bail!("GetPackagePathByFullName failed with {result}");
    }
    let end = buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(buffer.len());
    Ok(PathBuf::from(String::from_utf16_lossy(&buffer[..end])))
}

fn package_version_key(full_name: &str) -> (u16, u16, u16, u16) {
    let mut parts = full_name
        .split('_')
        .nth(1)
        .into_iter()
        .flat_map(|version| version.split('.'))
        .filter_map(|part| part.parse::<u16>().ok());
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = lparam as *const CREATESTRUCTW;
        if !create.is_null() {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*create).lpCreateParams as isize);
        }
    }
    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TrayHost;
    if !state.is_null() {
        match message {
            WM_TIMER if wparam == POLL_TIMER => {
                (*state).refresh();
                return 0;
            }
            TRAY_MESSAGE => {
                let event = lparam as u32;
                if event == WM_RBUTTONUP || event == WM_CONTEXTMENU || event == WM_LBUTTONUP {
                    (*state).show_menu();
                }
                return 0;
            }
            WM_COMMAND => {
                (*state).invoke(wparam & 0xffff);
                return 0;
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                return 0;
            }
            _ => {}
        }
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}

fn append_menu(menu: *mut c_void, flags: u32, id: usize, label: &str) {
    let label = wide(label);
    unsafe {
        AppendMenuW(menu, flags, id, label.as_ptr());
    }
}

fn run_restore(wtcli: &Path, id: &str) -> Result<()> {
    let output = hidden_output(wtcli, ["restore-shell-session", id])?;
    command_succeeded(output, "wtcli restore-shell-session")
}

fn list_psmux_sessions() -> Result<Vec<PsmuxSession>> {
    let output = hidden_output(
        Path::new("psmux.exe"),
        ["-L", "windows-terminal", "list-sessions"],
    )?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lower = stderr.to_ascii_lowercase();
        if lower.contains("no server running")
            || lower.contains("no sessions")
            || lower.contains("cannot connect to server")
        {
            tracing::debug!(
                target: "durable_tray",
                stderr = %stderr.trim(),
                "psmux has no live Windows Terminal sessions"
            );
            return Ok(Vec::new());
        }
        bail!(
            "psmux list-sessions exited with {}: {}",
            output.status,
            stderr.trim()
        );
    }
    Ok(parse_psmux_sessions(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn kill_psmux_session(name: &str) -> Result<()> {
    let output = hidden_output(
        Path::new("psmux.exe"),
        ["-L", "windows-terminal", "kill-session", "-t", name],
    )?;
    command_succeeded(output, "psmux kill-session")
}

fn kill_psmux_session_if_present(name: &str) -> Result<()> {
    match kill_psmux_session(name) {
        Ok(()) => Ok(()),
        Err(error) => {
            let still_live = list_psmux_sessions()?
                .iter()
                .any(|session| session.name.eq_ignore_ascii_case(name));
            if still_live {
                Err(error)
            } else {
                tracing::debug!(
                    target: "durable_tray",
                    session_name = name,
                    "psmux session exited before the end request completed"
                );
                Ok(())
            }
        }
    }
}

fn is_current_host() -> bool {
    let Some(local_root) = crate::runtime_paths::intelligent_terminal_local_root() else {
        return true;
    };
    let marker = local_root.join("durable-host").join(CURRENT_HOST_FILE);
    let Ok(expected) = std::fs::read_to_string(marker) else {
        return true;
    };
    let Ok(current) = std::env::current_exe() else {
        return true;
    };
    same_path(Path::new(&expected), &current)
}

fn hidden_output<I, S>(program: &Path, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(program);
    command.args(args).creation_flags(CREATE_NO_WINDOW);
    command
        .output()
        .with_context(|| format!("failed to start {}", program.display()))
}

fn command_succeeded(output: Output, operation: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    bail!(
        "{operation} exited with {}: {} {}",
        output.status,
        stderr.trim(),
        stdout.trim()
    )
}

fn parse_psmux_sessions(output: &str) -> Vec<PsmuxSession> {
    output
        .lines()
        .filter_map(|line| {
            let start = line.find("wt-")?;
            let candidate = line.get(start + 3..start + 39)?;
            let session_id = Uuid::parse_str(candidate).ok()?;
            Some(PsmuxSession {
                name: format!("wt-{session_id}"),
                session_id,
                attached: line.contains("(attached)"),
            })
        })
        .collect()
}

fn extract_layout_session_ids(layout_json: &str) -> HashSet<Uuid> {
    let Ok(value) = serde_json::from_str::<Value>(layout_json) else {
        return HashSet::new();
    };
    let mut result = HashSet::new();
    extract_session_ids_from_value(&value, &mut result);
    result
}

fn extract_session_ids_from_value(value: &Value, result: &mut HashSet<Uuid>) {
    match value {
        Value::Object(values) => {
            for (key, value) in values {
                if key == "sessionId" {
                    if let Some(text) = value.as_str() {
                        if let Ok(id) = Uuid::parse_str(text) {
                            result.insert(id);
                        }
                    }
                }
                extract_session_ids_from_value(value, result);
            }
        }
        Value::Array(values) => {
            for value in values {
                extract_session_ids_from_value(value, result);
            }
        }
        _ => {}
    }
}

fn map_detached_records(
    records: &[ShellSessionRecord],
    sessions: &[PsmuxSession],
) -> Vec<BackgroundRecord> {
    records
        .iter()
        .filter_map(|record| {
            map_record_sessions(record, sessions)
                .iter()
                .any(|session| !session.attached)
                .then(|| BackgroundRecord {
                    record: record.clone(),
                })
        })
        .collect()
}

fn map_record_sessions(
    record: &ShellSessionRecord,
    sessions: &[PsmuxSession],
) -> Vec<PsmuxSession> {
    let ids = extract_layout_session_ids(&record.layout_json);
    sessions
        .iter()
        .filter(|session| ids.contains(&session.session_id))
        .cloned()
        .collect()
}

fn map_live_record_ids(records: &[ShellSessionRecord], sessions: &[PsmuxSession]) -> Vec<String> {
    let live = sessions
        .iter()
        .map(|session| session.session_id)
        .collect::<HashSet<_>>();
    records
        .iter()
        .filter(|record| {
            extract_layout_session_ids(&record.layout_json)
                .iter()
                .any(|id| live.contains(id))
        })
        .map(|record| record.id.clone())
        .collect()
}

fn extract_app_icon(path: &Path) -> HICON {
    let path_wide = wide(path.as_os_str());
    let mut icon = std::ptr::null_mut();
    if unsafe { ExtractIconExW(path_wide.as_ptr(), 0, std::ptr::null_mut(), &mut icon, 1) } == 0
        || icon.is_null()
    {
        tracing::warn!(
            target: "durable_tray",
            path = %path.display(),
            "failed to extract the Intelligent Terminal tray icon"
        );
        std::ptr::null_mut()
    } else {
        icon
    }
}

fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

fn copy_wide<const N: usize>(destination: &mut [u16; N], value: &str) {
    destination.fill(0);
    for (target, source) in destination
        .iter_mut()
        .take(N.saturating_sub(1))
        .zip(value.encode_utf16())
    {
        *target = source;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, name: &str, layout_json: &str) -> ShellSessionRecord {
        ShellSessionRecord {
            id: id.to_string(),
            name: name.to_string(),
            layout_json: layout_json.to_string(),
            elevated: false,
            created_at: 1,
            updated_at: 1,
            last_used_at: 1,
            revision: 1,
        }
    }

    #[test]
    fn parses_psmux_session_attachment_state() {
        let sessions = parse_psmux_sessions(
            "wt-11111111-1111-1111-1111-111111111111: 1 windows (created Tue) (attached)\n\
             wt-22222222-2222-2222-2222-222222222222: 1 windows (created Tue)\n\
             unrelated: 1 windows\n",
        );
        assert_eq!(sessions.len(), 2);
        assert!(sessions[0].attached);
        assert!(!sessions[1].attached);
    }

    #[test]
    fn parses_package_version_for_current_wtcli_selection() {
        assert_eq!(
            package_version_key("Microsoft.IntelligentTerminal_1.12.3.45_x64__publisher"),
            (1, 12, 3, 45)
        );
        assert_eq!(package_version_key("invalid"), (0, 0, 0, 0));
    }

    #[test]
    fn recursively_extracts_layout_session_ids() {
        let ids = extract_layout_session_ids(
            r#"{"root":{"sessionId":"11111111-1111-1111-1111-111111111111",
                "children":[{"content":{"sessionId":"{22222222-2222-2222-2222-222222222222}"}}],
                "ignored":{"sessionID":"33333333-3333-3333-3333-333333333333"}}}"#,
        );
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()));
        assert!(ids.contains(&Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()));
    }

    #[test]
    fn maps_only_records_with_detached_live_sessions() {
        let records = vec![
            record(
                "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "mixed",
                r#"{"panes":[
                    {"sessionId":"11111111-1111-1111-1111-111111111111"},
                    {"sessionId":"22222222-2222-2222-2222-222222222222"}]}"#,
            ),
            record(
                "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                "attached",
                r#"{"sessionId":"33333333-3333-3333-3333-333333333333"}"#,
            ),
            record(
                "cccccccc-cccc-cccc-cccc-cccccccccccc",
                "stale",
                r#"{"sessionId":"44444444-4444-4444-4444-444444444444"}"#,
            ),
        ];
        let sessions = parse_psmux_sessions(
            "wt-11111111-1111-1111-1111-111111111111: 1 windows\n\
             wt-22222222-2222-2222-2222-222222222222: 1 windows\n\
             wt-33333333-3333-3333-3333-333333333333: 1 windows (attached)\n",
        );
        let mapped = map_detached_records(&records, &sessions);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].record.name, "mixed");
    }

    #[test]
    fn maps_all_live_records_for_retention_keep_alive() {
        let records = vec![
            record(
                "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "detached",
                r#"{"sessionId":"11111111-1111-1111-1111-111111111111"}"#,
            ),
            record(
                "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                "attached",
                r#"{"sessionId":"22222222-2222-2222-2222-222222222222"}"#,
            ),
            record(
                "cccccccc-cccc-cccc-cccc-cccccccccccc",
                "stale",
                r#"{"sessionId":"33333333-3333-3333-3333-333333333333"}"#,
            ),
        ];
        let sessions = parse_psmux_sessions(
            "wt-11111111-1111-1111-1111-111111111111: 1 windows\n\
             wt-22222222-2222-2222-2222-222222222222: 1 windows (attached)\n",
        );

        assert_eq!(
            map_live_record_ids(&records, &sessions),
            vec![
                "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
                "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string()
            ]
        );
    }

    #[test]
    fn ending_a_partially_detached_record_includes_attached_panes() {
        let records = vec![record(
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "mixed",
            r#"{"panes":[
                {"sessionId":"11111111-1111-1111-1111-111111111111"},
                {"sessionId":"22222222-2222-2222-2222-222222222222"}]}"#,
        )];
        let sessions = parse_psmux_sessions(
            "wt-11111111-1111-1111-1111-111111111111: 1 windows\n\
             wt-22222222-2222-2222-2222-222222222222: 1 windows (attached)\n",
        );
        let mapped = map_record_sessions(&records[0], &sessions);
        assert_eq!(mapped.len(), 2);
        assert!(mapped.iter().any(|session| session.attached));
    }
}
