// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use anyhow::{Context, Result};
use tokio::sync::Notify;
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Threading::{
    OpenEventW, RegisterWaitForSingleObject, UnregisterWaitEx, INFINITE,
    SYNCHRONIZATION_SYNCHRONIZE, WT_EXECUTEONLYONCE,
};

/// One host-owned event per master generation. The registered wait avoids a
/// polling task or a blocking Tokio worker for the lifetime of an idle master.
pub(super) struct ShutdownSignal {
    _event: OwnedHandle,
    wait: HANDLE,
    notification: Option<Box<Notify>>,
}

impl ShutdownSignal {
    pub(super) fn from_environment() -> Result<Option<Self>> {
        let Some(name) =
            std::env::var_os("WTA_MASTER_SHUTDOWN_EVENT").filter(|name| !name.is_empty())
        else {
            return Ok(None);
        };
        Self::open(&name).map(Some)
    }

    fn open(name: &OsStr) -> Result<Self> {
        let name: Vec<u16> = name.encode_wide().chain(Some(0)).collect();
        // SAFETY: name is NUL-terminated and the successful handle is adopted once.
        let event = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
        if event.is_null() {
            return Err(std::io::Error::last_os_error()).context("open master shutdown event");
        }
        let event = unsafe { OwnedHandle::from_raw_handle(event) };
        let notification = Box::new(Notify::new());
        let mut wait = std::ptr::null_mut();
        // SAFETY: the boxed Notify stays at this address until Drop has
        // synchronously drained the single registered callback.
        let registered = unsafe {
            RegisterWaitForSingleObject(
                &mut wait,
                event.as_raw_handle(),
                Some(on_shutdown),
                (&*notification as *const Notify).cast_mut().cast(),
                INFINITE,
                WT_EXECUTEONLYONCE,
            )
        };
        if registered == 0 {
            return Err(std::io::Error::last_os_error()).context("register master shutdown wait");
        }
        Ok(Self {
            _event: event,
            wait,
            notification: Some(notification),
        })
    }

    pub(super) async fn requested(&self) {
        if let Some(notification) = &self.notification {
            notification.notified().await;
        }
    }
}

unsafe extern "system" fn on_shutdown(context: *mut std::ffi::c_void, _: bool) {
    // SAFETY: ShutdownSignal owns the Notify until this callback has drained.
    unsafe { &*context.cast::<Notify>() }.notify_one();
}

impl Drop for ShutdownSignal {
    fn drop(&mut self) {
        // SAFETY: wait is a successful registration; INVALID_HANDLE_VALUE
        // waits for callbacks before the event and Notify are released.
        if unsafe { UnregisterWaitEx(self.wait, INVALID_HANDLE_VALUE) } == 0 {
            tracing::error!(
                target: "master",
                error = %std::io::Error::last_os_error(),
                "failed to drain master shutdown wait"
            );
            // An undrained callback must never observe freed context.
            if let Some(notification) = self.notification.take() {
                std::mem::forget(notification);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};

    #[tokio::test]
    async fn host_shutdown_signal_survives_early_delivery() {
        let name = format!("Local\\wta-shutdown-test-{}", uuid::Uuid::new_v4());
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // SAFETY: a private manual-reset event with no inherited handles.
        let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, wide.as_ptr()) };
        assert!(!handle.is_null());
        let event = unsafe { OwnedHandle::from_raw_handle(handle) };
        let signal = ShutdownSignal::open(OsStr::new(&name)).unwrap();
        assert_ne!(unsafe { SetEvent(event.as_raw_handle()) }, 0);
        tokio::time::timeout(std::time::Duration::from_secs(5), signal.requested())
            .await
            .unwrap();
        drop(signal);
        // Unregistering the wait must not close or signal the host's handle.
        assert_ne!(unsafe { SetEvent(event.as_raw_handle()) }, 0);
    }

    #[test]
    fn missing_host_shutdown_event_fails_without_creating_one() {
        let name = format!("Local\\wta-shutdown-test-{}", uuid::Uuid::new_v4());
        assert!(ShutdownSignal::open(OsStr::new(&name)).is_err());
    }
}
