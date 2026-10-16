use super::*;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BRIDGE: &str = "bridge.js";
const V2_FILES: &[&str] = &[
    OPENCODE_PLUGIN_JS,
    "package.json",
    "server.js",
    "rpc.js",
    "tui.js",
];
const API_MARKER: &str = "// OpenCode API variant: ";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiVariant {
    V1,
    V2,
}

impl ApiVariant {
    fn name(self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::V2 => "v2",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "v1" => Some(Self::V1),
            "v2" => Some(Self::V2),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Compatibility {
    pub cli_version: Option<String>,
    pub api_variant: Option<ApiVariant>,
    pub installed_api_variant: Option<ApiVariant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn parse_cli_version(output: &str) -> Result<(String, ApiVariant), String> {
    let output = output
        .trim()
        .strip_prefix("opencode ")
        .unwrap_or(output.trim());
    let value = output.strip_prefix('v').unwrap_or(output);
    let valid_identifiers = |value: &str| {
        value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    };
    let without_build = match value.split_once('+') {
        Some((version, build)) if valid_identifiers(build) => version,
        Some(_) => return Err("OpenCode --version returned invalid build metadata".into()),
        None => value,
    };
    if let Some((_, prerelease)) = without_build.split_once('-') {
        if !valid_identifiers(prerelease) {
            return Err("OpenCode --version returned an invalid prerelease version".into());
        }
    }
    let numeric = value.split(['-', '+']).next().unwrap_or("");
    let version = numeric
        .parse::<Version>()
        .map_err(|_| "OpenCode --version did not return a semantic version".to_string())?;
    let variant = match version.major {
        1 => ApiVariant::V1,
        2 => ApiVariant::V2,
        major => return Err(format!("OpenCode major version {major} is not supported")),
    };
    Ok((value.to_string(), variant))
}

fn detect(executable: &str) -> Result<(String, ApiVariant), String> {
    let result = probe_version(executable).and_then(|output| parse_cli_version(&output));
    if let Err(error) = &result {
        tracing::warn!(target: "agent_hooks", executable, %error, "OpenCode version detection failed");
    }
    result
}

fn probe_version(executable: &str) -> Result<String, String> {
    use std::os::windows::process::CommandExt;

    let mut child = Command::new(executable)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000)
        .spawn()
        .map_err(|error| format!("failed to start OpenCode --version: {error}"))?;
    let job = match ProbeJob::new(&child) {
        Ok(job) => job,
        Err(error) => {
            if let Err(cleanup) = child.kill().and_then(|()| child.wait().map(|_| ())) {
                tracing::warn!(target: "agent_hooks", %cleanup, "failed to reclaim OpenCode version probe");
            }
            return Err(error);
        }
    };
    let stdout = child
        .stdout
        .take()
        .ok_or("OpenCode stdout pipe is unavailable")?;
    let reader = std::thread::Builder::new()
        .name("opencode-version".into())
        .spawn(move || {
            let mut output = String::new();
            stdout
                .take(65536)
                .read_to_string(&mut output)
                .map(|_| output)
        })
        .map_err(|error| format!("failed to read OpenCode version: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => break Err("OpenCode --version timed out".to_string()),
            Err(error) => break Err(format!("failed to wait for OpenCode --version: {error}")),
        }
    };
    // The probe may be an npm .cmd shim. Closing its job also reclaims only
    // that probe's descendants before joining the stdout reader.
    drop(job);
    let _ = child.wait();
    let output = reader
        .join()
        .map_err(|_| "OpenCode version reader panicked".to_string())?
        .map_err(|error| format!("failed to read OpenCode version: {error}"))?;
    let status = status?;
    if !status.success() {
        return Err(format!(
            "OpenCode --version exited with {:?}",
            status.code()
        ));
    }
    Ok(output)
}

struct ProbeJob(windows_sys::Win32::Foundation::HANDLE);

impl ProbeJob {
    fn new(child: &std::process::Child) -> Result<Self, String> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::*;
        // This job owns only the newly spawned read-only version probe.
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(format!(
                    "failed to create OpenCode probe job: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let job = Self(handle);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            ) == 0
                || AssignProcessToJobObject(handle, child.as_raw_handle()) == 0
            {
                return Err(format!(
                    "failed to assign OpenCode probe job: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(job)
        }
    }
}

impl Drop for ProbeJob {
    fn drop(&mut self) {
        // Closing this non-inherited handle terminates this probe's job only.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

fn installed_variant(home: &Path) -> Option<ApiVariant> {
    let manifest = opencode_support_dir(home).join(OPENCODE_MANIFEST);
    if !opencode_manifest_is_managed(&manifest) {
        return None;
    }
    let value: Value = serde_json::from_str(&fs::read_to_string(manifest).ok()?).ok()?;
    ApiVariant::parse(value.get("api_variant")?.as_str()?)
}

fn managed_file(path: &Path) -> bool {
    fs::read_to_string(path).is_ok_and(|text| text.contains(OPENCODE_MANAGED_MARKER))
}

pub(super) fn complete(home: &Path) -> bool {
    let support = opencode_support_dir(home);
    let variant = installed_variant(home);
    let entry = match variant {
        Some(ApiVariant::V2) => support.join(OPENCODE_PLUGIN_JS),
        _ => opencode_plugins_dir(home).join(OPENCODE_PLUGIN_JS),
    };
    if !managed_file(&entry) || !opencode_manifest_is_managed(&support.join(OPENCODE_MANIFEST)) {
        return false;
    }
    let Some(variant) = variant else { return true };
    let marker = format!("{API_MARKER}{}", variant.name());
    if !fs::read_to_string(entry).is_ok_and(|text| text.contains(&marker))
        || !managed_file(&support.join(BRIDGE))
    {
        return false;
    }
    if variant == ApiVariant::V2 {
        return V2_FILES.iter().all(|file| {
            let path = support.join(file);
            if *file == "package.json" {
                package_is_managed(&path)
            } else {
                managed_file(&path)
            }
        }) && !opencode_plugins_dir(home).join(OPENCODE_PLUGIN_JS).exists();
    }
    !support.join("package.json").exists()
}

fn package_is_managed(path: &Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|value| {
            value.get("managed_by").and_then(Value::as_str) == Some(OPENCODE_MANIFEST_MANAGED_BY)
        })
}

pub(super) fn managed_entry(home: &Path) -> bool {
    let support = opencode_support_dir(home);
    managed_file(&support.join(OPENCODE_PLUGIN_JS)) || managed_file(&support.join("server.js"))
}

pub(super) fn managed_support(home: &Path) -> bool {
    let support = opencode_support_dir(home);
    opencode_manifest_is_managed(&support.join(OPENCODE_MANIFEST))
        || managed_file(&support.join(BRIDGE))
}

pub(super) fn apply_compatibility(status: &mut CliStatus, home: Option<&Path>) {
    let detected = status.binary_path.as_deref().map(detect);
    let installed = home.and_then(installed_variant);
    apply_detected(status, installed, detected);
}

fn apply_detected(
    status: &mut CliStatus,
    installed: Option<ApiVariant>,
    detected: Option<Result<(String, ApiVariant), String>>,
) {
    let (cli_version, variant, error) = match detected {
        Some(Ok((version, variant))) => (Some(version), Some(variant), None),
        Some(Err(error)) => (None, None, Some(error)),
        None => (None, None, None),
    };
    if status.binary_on_path && (variant.is_none() || installed != variant) {
        status.plugin_enabled = false;
        status.detection_fallback = Some(if variant.is_none() {
            "opencode-version-unknown"
        } else {
            "opencode-api-variant-mismatch"
        });
    }
    status.opencode = Some(Compatibility {
        cli_version,
        api_variant: variant,
        installed_api_variant: installed,
        error,
    });
}

fn check_owned_files(home: &Path) -> std::io::Result<()> {
    let support = opencode_support_dir(home);
    let root = opencode_plugins_dir(home).join(OPENCODE_PLUGIN_JS);
    let marker = support.join(OPENCODE_MANIFEST);
    match fs::symlink_metadata(&marker) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "OpenCode ownership manifest is not a regular file",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if let Ok(text) = fs::read_to_string(&marker) {
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            if value.get("managed_by").and_then(Value::as_str) != Some(OPENCODE_MANIFEST_MANAGED_BY)
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "OpenCode ownership manifest belongs to another plugin",
                ));
            }
        }
    }
    for file in std::iter::once(root).chain(
        [
            BRIDGE,
            OPENCODE_PLUGIN_JS,
            "server.js",
            "rpc.js",
            "tui.js",
            "package.json",
        ]
        .iter()
        .map(|file| support.join(file)),
    ) {
        match fs::symlink_metadata(&file) {
            Ok(metadata) => {
                let managed = if file.file_name().is_some_and(|name| name == "package.json") {
                    package_is_managed(&file)
                } else {
                    managed_file(&file)
                };
                if !metadata.file_type().is_file() || !managed {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!("{} is not a regular managed file", file.display()),
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let nested = support.join(OPENCODE_SUPPORT_DIR);
    match fs::symlink_metadata(&nested) {
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "OpenCode legacy support path is not a regular directory",
            ));
        }
        Ok(_) => {
            let rpc = nested.join("rpc.js");
            match fs::symlink_metadata(&rpc) {
                Ok(metadata) if !metadata.file_type().is_file() || !managed_file(&rpc) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "OpenCode legacy RPC file is not managed by Intelligent Terminal",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

fn remove_legacy_rpc(home: &Path) -> std::io::Result<()> {
    let nested = opencode_support_dir(home).join(OPENCODE_SUPPORT_DIR);
    let rpc = nested.join("rpc.js");
    if rpc.exists() {
        fs::remove_file(rpc)?;
    }
    if nested.is_dir() && fs::read_dir(&nested)?.next().is_none() {
        fs::remove_dir(nested)?;
    }
    Ok(())
}

pub(super) fn copy_bundle(source: &Path, home: &Path, variant: ApiVariant) -> std::io::Result<()> {
    copy_bundle_with_writer(source, home, variant, |file, bytes| file.write_all(bytes))
}

// Like prompt::write_atomic, stage beside the destination before replacing it.
// Reserve the staging file so cleanup never removes a preexisting foreign file.
fn replace_payload(
    path: &Path,
    bytes: &[u8],
    write: &mut impl FnMut(&mut fs::File, &[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let staging = path.with_file_name(format!(".wta-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)?;
    let result = write(&mut file, bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = result.and_then(|()| fs::rename(&staging, path));
    if let Err(error) = result {
        return match fs::remove_file(&staging) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(std::io::Error::new(
                error.kind(),
                format!("{error}; cannot clean {}: {cleanup}", staging.display()),
            )),
        };
    }
    Ok(())
}

fn copy_bundle_with_writer(
    source: &Path,
    home: &Path,
    variant: ApiVariant,
    mut write: impl FnMut(&mut fs::File, &[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    check_owned_files(home)?;
    let destination = opencode_plugins_dir(home);
    let support = opencode_support_dir(home);
    let root = destination.join(OPENCODE_PLUGIN_JS);
    if support.exists() && !managed_support(home) && !managed_file(&root) && !managed_entry(home) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "OpenCode support directory is not managed by Intelligent Terminal",
        ));
    }
    // Validate the complete source before touching the installed version.
    let mut manifest: Value =
        serde_json::from_str(&fs::read_to_string(source.join(OPENCODE_MANIFEST))?)
            .map_err(std::io::Error::other)?;
    if manifest.get("managed_by").and_then(Value::as_str) != Some(OPENCODE_MANIFEST_MANAGED_BY)
        || manifest.get("name").and_then(Value::as_str) != Some(PLUGIN_NAME)
        || manifest
            .get("version")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<Version>().ok())
            .is_none()
    {
        return Err(std::io::Error::other(
            "OpenCode bundle manifest is not a valid managed bundle",
        ));
    }
    manifest["api_variant"] = Value::String(variant.name().into());
    let adapter = source.join(variant.name());
    let files: &[&str] = if variant == ApiVariant::V2 {
        V2_FILES
    } else {
        &[OPENCODE_PLUGIN_JS]
    };
    let payloads = files
        .iter()
        .map(|file| fs::read(adapter.join(file)).map(|bytes| (*file, bytes)))
        .collect::<std::io::Result<Vec<_>>>()?;
    let bridge = fs::read(source.join("shared").join(BRIDGE))?;
    for (file, bytes) in &payloads {
        let owned = if *file == "package.json" {
            serde_json::from_slice::<Value>(bytes).is_ok_and(|value| {
                value.get("managed_by").and_then(Value::as_str)
                    == Some(OPENCODE_MANIFEST_MANAGED_BY)
            })
        } else {
            String::from_utf8_lossy(bytes).contains(OPENCODE_MANAGED_MARKER)
        };
        if !owned {
            return Err(std::io::Error::other(format!(
                "OpenCode bundle contains an unmanaged {file}"
            )));
        }
    }
    if !String::from_utf8_lossy(&bridge).contains(OPENCODE_MANAGED_MARKER) {
        return Err(std::io::Error::other(
            "OpenCode bundle contains an unmanaged bridge",
        ));
    }
    fs::create_dir_all(&destination)?;
    let created_support = match fs::create_dir(&support) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error),
    };
    // Invalidate the old completion marker first; a partial switch is retryable
    // even when the source bundle's version number did not change.
    let marker = support.join(OPENCODE_MANIFEST);
    if marker.is_file() {
        fs::remove_file(&marker)?;
    }
    if let Err(error) = replace_payload(&support.join(BRIDGE), &bridge, &mut write) {
        // Before the first committed payload there is no ownership marker.
        // Remove only our new, empty directory; never recursively remove files.
        if created_support {
            if let Err(cleanup) = fs::remove_dir(&support) {
                return Err(std::io::Error::new(
                    error.kind(),
                    format!("{error}; cannot clean {}: {cleanup}", support.display()),
                ));
            }
        }
        return Err(error);
    }
    for (file, bytes) in payloads {
        let target = if variant == ApiVariant::V1 {
            root.clone()
        } else {
            support.join(file)
        };
        replace_payload(&target, &bytes, &mut write)?;
    }
    if variant == ApiVariant::V2 {
        if root.exists() {
            fs::remove_file(&root)?;
        }
    } else {
        for file in V2_FILES {
            let path = support.join(file);
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
    }
    let legacy = support.join(OPENCODE_LEGACY_BRIDGE_PS1);
    if legacy.exists() {
        fs::remove_file(legacy)?;
    }
    remove_legacy_rpc(home)?;
    replace_payload(
        &marker,
        &serde_json::to_vec_pretty(&manifest).map_err(std::io::Error::other)?,
        &mut write,
    )
}

pub(super) fn install(home: &Path) -> InstallOutcome {
    let Some(executable) = crate::agent_check::find_exe("opencode") else {
        return InstallOutcome::Skipped;
    };
    let (_, variant) = match detect(&executable) {
        Ok(detected) => detected,
        Err(error) => return InstallOutcome::Failed(error),
    };
    let Some(source) = bundle::resolve_cli_dir(CliKind::OpenCode) else {
        return InstallOutcome::Failed("OpenCode hook bundle is unavailable".into());
    };
    match copy_bundle(&source, home, variant) {
        Ok(()) => InstallOutcome::Installed,
        Err(error) => {
            tracing::warn!(target: "agent_hooks", %error, api_variant = variant.name(), "OpenCode hook installation failed");
            InstallOutcome::Failed(format!("OpenCode hook installation failed: {error}"))
        }
    }
}

pub(super) fn remove_support_files(home: &Path) -> std::io::Result<()> {
    check_owned_files(home)?;
    remove_legacy_rpc(home)?;
    let support = opencode_support_dir(home);
    for file in std::iter::once(BRIDGE).chain(
        V2_FILES
            .iter()
            .copied()
            .filter(|file| *file != OPENCODE_PLUGIN_JS),
    ) {
        let path = support.join(file);
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_legacy_v2_layout_and_preserves_foreign_nested_dependencies() {
        let fixture = Fixture::new();
        let support = opencode_support_dir(&fixture.0);
        let nested = support.join(OPENCODE_SUPPORT_DIR);
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            support.join(OPENCODE_MANIFEST),
            fs::read(source().join(OPENCODE_MANIFEST)).unwrap(),
        )
        .unwrap();
        fs::write(
            support.join("server.js"),
            "// Managed by Intelligent Terminal: wt-agent-hooks",
        )
        .unwrap();
        fs::write(nested.join("rpc.js"), "user dependency").unwrap();
        assert!(copy_bundle(&source(), &fixture.0, ApiVariant::V2).is_err());
        assert!(!opencode_uninstall(Some(&fixture.0)).succeeded());
        assert_eq!(
            fs::read_to_string(nested.join("rpc.js")).unwrap(),
            "user dependency"
        );
        fs::write(
            nested.join("rpc.js"),
            "// Managed by Intelligent Terminal: wt-agent-hooks",
        )
        .unwrap();
        copy_bundle(&source(), &fixture.0, ApiVariant::V2).unwrap();
        assert!(complete(&fixture.0));
        assert!(!nested.exists());
        assert!(opencode_uninstall(Some(&fixture.0)).succeeded());
        assert!(!support.exists());
    }

    #[test]
    fn versions_select_only_supported_majors() {
        for (value, expected) in [
            ("1.18.15\n", ApiVariant::V1),
            ("v1.19.0", ApiVariant::V1),
            ("2.0.10", ApiVariant::V2),
            ("2.0.10-beta.1+build.2", ApiVariant::V2),
            ("opencode v2.0.28\n", ApiVariant::V2),
        ] {
            assert_eq!(parse_cli_version(value).unwrap().1, expected);
        }
        for value in [
            "",
            "2",
            "2.0",
            "OpenCode 2.0.10",
            "3.0.0",
            "0.1.0",
            "2.0.10\n1.0.0",
            "2.0.10-",
            "2.0.10+build+other",
        ] {
            assert!(parse_cli_version(value).is_err(), "{value:?}");
        }
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("wta-opencode-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn source() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("wt-agent-hooks")
            .join("opencode")
    }

    fn interrupted_write(file: &mut fs::File, bytes: &[u8]) -> std::io::Result<()> {
        file.write_all(&bytes[..1])?;
        Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            "injected partial write",
        ))
    }

    #[test]
    fn first_payload_write_failure_cleans_only_new_support_and_allows_retry() {
        for variant in [ApiVariant::V1, ApiVariant::V2] {
            let fixture = Fixture::new();
            let home = &fixture.0;
            let support = opencode_support_dir(home);
            assert!(copy_bundle_with_writer(&source(), home, variant, interrupted_write).is_err());
            assert!(!support.exists());
            copy_bundle(&source(), home, variant).unwrap();
            assert!(complete(home));

            let user_file = support.join("user.txt");
            fs::write(&user_file, "preserve").unwrap();
            assert!(copy_bundle_with_writer(&source(), home, variant, interrupted_write).is_err());
            assert_eq!(fs::read_to_string(&user_file).unwrap(), "preserve");
            check_owned_files(home).unwrap();
            copy_bundle(&source(), home, variant).unwrap();
            assert!(complete(home));
        }
    }

    #[test]
    fn interrupted_payload_updates_preserve_owned_files_and_allow_retry() {
        for variant in [ApiVariant::V1, ApiVariant::V2] {
            let payload_count = if variant == ApiVariant::V1 { 2 } else { 6 };
            // Include the last-written completion manifest as a failure point.
            for fail_at in 0..=payload_count {
                let fixture = Fixture::new();
                let home = &fixture.0;
                copy_bundle(&source(), home, variant).unwrap();
                let support = opencode_support_dir(home);
                let mut call = 0;
                let error = copy_bundle_with_writer(&source(), home, variant, |file, bytes| {
                    let current = call;
                    call += 1;
                    if current == fail_at {
                        interrupted_write(file, bytes)
                    } else {
                        file.write_all(bytes)
                    }
                })
                .unwrap_err();
                assert_eq!(error.kind(), std::io::ErrorKind::WriteZero);
                assert!(!complete(home));
                check_owned_files(home).unwrap();
                assert!(managed_file(&support.join(BRIDGE)));
                assert!(!support.join(OPENCODE_MANIFEST).exists());
                assert!(!fs::read_dir(&support).unwrap().any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")));
                copy_bundle(&source(), home, variant).unwrap();
                assert!(complete(home));
                assert!(opencode_uninstall(Some(home)).succeeded());
                assert!(!support.exists());
            }
        }
    }

    #[test]
    fn bridge_only_partial_install_can_be_uninstalled() {
        for variant in [ApiVariant::V1, ApiVariant::V2] {
            let fixture = Fixture::new();
            let home = &fixture.0;
            let mut writes = 0;
            assert!(
                copy_bundle_with_writer(&source(), home, variant, |file, bytes| {
                    writes += 1;
                    if writes == 2 {
                        interrupted_write(file, bytes)
                    } else {
                        file.write_all(bytes)
                    }
                })
                .is_err()
            );
            let support = opencode_support_dir(home);
            assert!(managed_file(&support.join(BRIDGE)));
            assert!(!managed_entry(home));
            assert!(!support.join(OPENCODE_MANIFEST).exists());
            let removed = opencode_uninstall(Some(home));
            assert!(removed.succeeded(), "{:?}", removed.messages);
            assert!(!support.exists());
        }
    }

    #[test]
    fn bridge_only_uninstall_preserves_foreign_bridges_and_support_files() {
        let fixture = Fixture::new();
        let home = &fixture.0;
        let support = opencode_support_dir(home);
        fs::create_dir_all(&support).unwrap();
        let bridge = support.join(BRIDGE);
        fs::write(&bridge, "foreign bridge").unwrap();
        assert!(!opencode_uninstall(Some(home)).succeeded());
        assert_eq!(fs::read_to_string(&bridge).unwrap(), "foreign bridge");

        let original = fs::read(source().join("shared").join(BRIDGE)).unwrap();
        fs::write(&bridge, &original).unwrap();
        let foreign = support.join("rpc.js");
        fs::write(&foreign, "foreign dependency").unwrap();
        assert!(!opencode_uninstall(Some(home)).succeeded());
        assert_eq!(fs::read(&bridge).unwrap(), original);
        assert_eq!(fs::read_to_string(&foreign).unwrap(), "foreign dependency");
        fs::remove_file(&foreign).unwrap();
        assert!(opencode_uninstall(Some(home)).succeeded());
        assert!(!support.exists());
    }

    #[test]
    fn version_probe_runs_the_resolved_shim_and_bounds_its_descendants() {
        let fixture = Fixture::new();
        let executable = fixture.0.join("opencode with spaces.cmd");
        fs::write(&executable, "@echo off\r\necho 2.0.10-beta.1\r\n").unwrap();
        assert_eq!(
            detect(executable.to_str().unwrap()).unwrap().1,
            ApiVariant::V2
        );
        fs::write(&executable, "@echo off\r\npowershell.exe -NoProfile -NonInteractive -Command \"Start-Sleep -Seconds 30\"\r\n").unwrap();
        let start = Instant::now();
        assert!(probe_version(executable.to_str().unwrap())
            .unwrap_err()
            .contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(15));
    }

    #[test]
    fn variant_switches_replace_the_entrypoint_without_two_loaded_plugins() {
        let fixture = Fixture::new();
        let home = &fixture.0;
        copy_bundle(&source(), home, ApiVariant::V1).unwrap();
        let support = opencode_support_dir(home);
        let root = opencode_plugins_dir(home).join(OPENCODE_PLUGIN_JS);
        fs::write(support.join("user.txt"), "preserve").unwrap();
        assert!(root.is_file());
        assert!(complete(home));
        assert_eq!(installed_variant(home), Some(ApiVariant::V1));
        let version = read_version_field(&support.join(OPENCODE_MANIFEST));

        copy_bundle(&source(), home, ApiVariant::V2).unwrap();
        assert!(!root.exists());
        assert!(support.join("tui.js").is_file());
        assert!(complete(home));
        assert_eq!(installed_variant(home), Some(ApiVariant::V2));
        assert_eq!(
            read_version_field(&support.join(OPENCODE_MANIFEST)),
            version
        );

        copy_bundle(&source(), home, ApiVariant::V1).unwrap();
        assert!(root.is_file());
        assert!(!support.join("package.json").exists());
        assert!(!support.join(OPENCODE_PLUGIN_JS).exists());
        assert!(complete(home));
        assert_eq!(
            fs::read_to_string(support.join("user.txt")).unwrap(),
            "preserve"
        );
    }

    #[test]
    fn unknown_or_changed_cli_versions_never_report_enabled_hooks() {
        let mut status = CliStatus::stub_skipped(CliKind::OpenCode);
        status.binary_on_path = true;
        status.plugin_enabled = true;
        apply_detected(
            &mut status,
            Some(ApiVariant::V1),
            Some(Ok(("2.0.10".into(), ApiVariant::V2))),
        );
        assert!(!status.plugin_enabled);
        assert_eq!(
            status.detection_fallback,
            Some("opencode-api-variant-mismatch")
        );
        assert_eq!(
            decide_install_action(CliKind::OpenCode, &status, None),
            InstallAction::Install
        );
        apply_detected(
            &mut status,
            Some(ApiVariant::V1),
            Some(Err("timeout".into())),
        );
        assert_eq!(status.detection_fallback, Some("opencode-version-unknown"));
        assert_eq!(
            status.opencode.as_ref().unwrap().error.as_deref(),
            Some("timeout")
        );
    }

    #[test]
    fn missing_v2_payload_and_user_collisions_remain_repairable_without_overwriting() {
        let fixture = Fixture::new();
        let home = &fixture.0;
        copy_bundle(&source(), home, ApiVariant::V2).unwrap();
        let support = opencode_support_dir(home);
        fs::remove_file(support.join("rpc.js")).unwrap();
        assert!(!complete(home));
        assert_eq!(
            read_installed_opencode(home).unwrap().unwrap().version,
            None
        );
        copy_bundle(&source(), home, ApiVariant::V2).unwrap();
        assert!(complete(home));
        fs::write(support.join("tui.js"), "user owned").unwrap();
        let marker = fs::read(support.join(OPENCODE_MANIFEST)).unwrap();
        assert!(copy_bundle(&source(), home, ApiVariant::V1).is_err());
        assert_eq!(fs::read(support.join(OPENCODE_MANIFEST)).unwrap(), marker);
        assert_eq!(
            fs::read_to_string(support.join("tui.js")).unwrap(),
            "user owned"
        );
        assert!(!opencode_uninstall(Some(home)).succeeded());
    }

    #[test]
    fn v2_uninstall_preserves_its_entrypoint_until_marker_cleanup_succeeds() {
        let fixture = Fixture::new();
        let home = &fixture.0;
        copy_bundle(&source(), home, ApiVariant::V2).unwrap();
        let support = opencode_support_dir(home);
        let marker = support.join(OPENCODE_MANIFEST);
        fs::remove_file(&marker).unwrap();
        fs::create_dir(&marker).unwrap();
        assert!(!opencode_uninstall(Some(home)).succeeded());
        assert!(support.join(OPENCODE_PLUGIN_JS).is_file());
        fs::remove_dir(&marker).unwrap();
        assert!(opencode_uninstall(Some(home)).succeeded());
        assert!(!support.exists());
    }
}
