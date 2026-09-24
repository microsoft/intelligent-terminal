use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{
    bundle, cli_binary_on_path, copy_dir_recursive, run_plugin_cli_with_env, CliKind, CliStatus,
    CliUninstallResult, InstallOutcome, InstalledInfo, InstalledProbe, Version, PLUGIN_NAME,
    STAGING_SUBDIR,
};

pub(super) const MANIFEST: &str = "intelligent-terminal.json";
const DESCRIPTION: &str = "Managed by Intelligent Terminal: wt-agent-hooks";
const MANAGED_BY: &str = "Intelligent Terminal: wt-agent-hooks";
const FILES: [&str; 3] = ["plugin.json", "hooks.json", MANIFEST];

struct JsonFile {
    value: Value,
}

impl JsonFile {
    fn read(path: PathBuf) -> Result<Option<Self>, String> {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
        };
        if !metadata.file_type().is_file() {
            return Err(format!("{} is not a regular file", path.display()));
        }
        let original =
            fs::read(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let value: Value = serde_json::from_slice(&original)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        if !value.is_object() {
            return Err(format!("{} must contain a JSON object", path.display()));
        }
        Ok(Some(Self { value }))
    }
}

struct Installation {
    directory: PathBuf,
    imports: Option<JsonFile>,
    config: Option<JsonFile>,
    registered: bool,
    owned: bool,
    complete: bool,
    enabled: bool,
    version: Option<Version>,
}

fn owns(document: &JsonFile, marker: bool) -> bool {
    document.value.get("name").and_then(Value::as_str) == Some(PLUGIN_NAME)
        && if marker {
            document.value.get("managed_by").and_then(Value::as_str) == Some(MANAGED_BY)
        } else {
            document.value.get("description").and_then(Value::as_str) == Some(DESCRIPTION)
        }
}

fn load(home: &Path) -> Result<Installation, String> {
    // Plugin configuration is shared; it is not the CLI/ACP conversation home.
    let root = home.join(".gemini").join("config");
    let directory = root.join("plugins").join(PLUGIN_NAME);
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err(format!(
                "{} is not a regular plugin directory",
                directory.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot inspect {}: {error}", directory.display())),
    }

    let descriptor = JsonFile::read(directory.join("plugin.json"));
    let marker = JsonFile::read(directory.join(MANIFEST));
    let descriptor_owned = descriptor
        .as_ref()
        .ok()
        .and_then(Option::as_ref)
        .is_some_and(|document| owns(document, false));
    let marker_owned = marker
        .as_ref()
        .ok()
        .and_then(Option::as_ref)
        .is_some_and(|document| owns(document, true));
    let owned = descriptor_owned || marker_owned;
    for result in [&descriptor, &marker] {
        if let Err(error) = result {
            if !owned {
                return Err(error.clone());
            }
            tracing::warn!(target: "agent_hooks", %error, "partial managed Antigravity plugin needs repair");
        }
    }

    let imports = JsonFile::read(root.join("import_manifest.json"))?;
    let mut registered = false;
    if let Some(entries) = imports.as_ref().and_then(|file| file.value.get("imports")) {
        if !entries.is_null() {
            let entries = entries
                .as_array()
                .ok_or("Antigravity imports must be an array or null")?;
            for entry in entries
                .iter()
                .filter(|entry| entry.get("name").and_then(Value::as_str) == Some(PLUGIN_NAME))
            {
                if registered || entry.get("source").and_then(Value::as_str) != Some("antigravity")
                {
                    return Err("conflicting Antigravity hook import registration".into());
                }
                registered = entry
                    .get("components")
                    .and_then(Value::as_array)
                    .is_some_and(|components| {
                        components
                            .iter()
                            .any(|component| component.as_str() == Some("hooks"))
                    });
            }
        }
    }
    let config = JsonFile::read(root.join("config.json"))?;
    let mut enabled = true;
    if let Some(plugins) = config.as_ref().and_then(|file| file.value.get("plugins")) {
        if !plugins.is_null() {
            let plugins = plugins
                .as_object()
                .ok_or("Antigravity plugins configuration must be an object")?;
            if let Some(plugin) = plugins.get(PLUGIN_NAME) {
                let plugin = plugin
                    .as_object()
                    .ok_or("Antigravity hook configuration must be an object")?;
                if let Some(value) = plugin.get("enabled") {
                    enabled = value
                        .as_bool()
                        .ok_or("Antigravity hook enabled value must be boolean")?;
                }
            }
        }
    }
    let version = marker
        .as_ref()
        .ok()
        .and_then(Option::as_ref)
        .filter(|document| owns(document, true))
        .and_then(|document| document.value.get("version"))
        .and_then(Value::as_str)
        .and_then(|value| value.parse().ok());
    let complete = registered
        && descriptor_owned
        && marker_owned
        && version.is_some()
        && directory.join("hooks.json").is_file();
    Ok(Installation {
        directory,
        imports,
        config,
        registered,
        owned,
        complete,
        enabled,
        version,
    })
}

pub(super) fn status(on_path: bool, binary_path: Option<String>, home: Option<&Path>) -> CliStatus {
    let mut result = CliStatus {
        name: "antigravity",
        binary_on_path: on_path,
        binary_path,
        marketplace_registered: false,
        marketplace_path: None,
        marketplace_path_valid: false,
        plugin_installed: false,
        plugin_enabled: false,
        installed_version: None,
        bundle_version: None,
        detection_fallback: None,
    };
    let Some(home) = home else { return result };
    match load(home) {
        Ok(installation) => {
            result.marketplace_registered = installation.registered || installation.owned;
            result.marketplace_path = result
                .marketplace_registered
                .then(|| installation.directory.to_string_lossy().into_owned());
            result.marketplace_path_valid = installation.complete;
            result.plugin_installed = installation.complete;
            result.plugin_enabled = installation.complete && installation.enabled;
            result.installed_version = installation.version.map(|version| version.to_string());
        }
        Err(error) => {
            result.detection_fallback = Some("fs");
            tracing::warn!(target: "agent_hooks", %error, "cannot read Antigravity hook state");
        }
    }
    result
}

pub(super) fn installed(home: &Path) -> InstalledProbe {
    let installation = load(home)?;
    if !installation.owned {
        return Ok(None);
    }
    Ok(Some(InstalledInfo {
        version: installation
            .complete
            .then_some(installation.version)
            .flatten(),
        enabled: installation.enabled,
        loads_live: false,
        registered_source: None,
        gemini_source: None,
        gemini_type: None,
    }))
}

fn validate_files(directory: &Path) -> Result<(), String> {
    for name in FILES {
        let path = directory.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.file_type().is_file() => {
                return Err(format!(
                    "refusing to replace or remove non-regular file {}",
                    path.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
        }
    }
    Ok(())
}

fn validate_owned_files(directory: &Path) -> Result<(), String> {
    validate_files(directory)?;
    for (name, marker) in [("plugin.json", false), (MANIFEST, true)] {
        if let Some(document) = JsonFile::read(directory.join(name))? {
            if !owns(&document, marker) {
                return Err(format!(
                    "refusing to change conflicting Antigravity ownership in {name}"
                ));
            }
        }
    }
    if directory.is_dir() {
        for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            if !FILES
                .iter()
                .any(|name| entry.file_name() == std::ffi::OsStr::new(name))
            {
                return Err(format!(
                    "preserve additional plugin file before changing hooks: {}",
                    entry.path().display()
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn with_staged_bundle(
    source: &Path,
    staging_root: &Path,
    action: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    fs::create_dir_all(staging_root)
        .map_err(|error| format!("cannot prepare Antigravity hook staging: {error}"))?;
    let staging = staging_root.join(uuid::Uuid::new_v4().to_string());
    fs::create_dir(&staging)
        .map_err(|error| format!("cannot create Antigravity hook staging: {error}"))?;
    // Reserve our unique directory before copying, and clean it after every ordinary exit.
    let result = (|| {
        copy_dir_recursive(source, &staging)
            .map_err(|error| format!("cannot stage Antigravity hooks: {error}"))?;
        fs::remove_file(staging.join(MANIFEST)).map_err(|error| error.to_string())?;
        action(&staging)
    })();
    match fs::remove_dir_all(&staging) {
        Ok(()) => result,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => result,
        Err(error) => {
            tracing::warn!(target: "agent_hooks", path = tracing::field::display(staging.display()), %error, "failed to remove Antigravity hook staging");
            let cleanup = format!(
                "cannot clean Antigravity hook staging {}: {error}",
                staging.display()
            );
            Err(match result {
                Ok(()) => cleanup,
                Err(original) => format!("{original}; {cleanup}"),
            })
        }
    }
}

pub(super) fn install(home: &Path) -> InstallOutcome {
    if !cli_binary_on_path(CliKind::Antigravity) {
        tracing::debug!(target: "agent_hooks", "agy is not installed; skipping Antigravity hooks");
        return InstallOutcome::Skipped;
    }
    let result = (|| -> Result<(), String> {
        let source = bundle::resolve_cli_dir(CliKind::Antigravity)
            .ok_or("the packaged Antigravity hook bundle was not found")?;
        let staging_root = crate::runtime_paths::intelligent_terminal_local_root()
            .ok_or("the Intelligent Terminal cache directory is unavailable")?
            .join(STAGING_SUBDIR)
            .join("antigravity");
        install_with(home, &source, &staging_root, |exe, args, environment| {
            run_plugin_cli_with_env(exe, args, environment, "agent_hooks", &[])
        })
    })();
    match result {
        Ok(()) => InstallOutcome::Installed,
        Err(error) => {
            tracing::warn!(target: "agent_hooks", %error, "Antigravity hook install failed");
            InstallOutcome::Failed(error)
        }
    }
}

pub(super) fn install_with(
    home: &Path,
    source: &Path,
    staging_root: &Path,
    mut run: impl FnMut(&str, &[&str], &[(&str, &str)]) -> std::io::Result<()>,
) -> Result<(), String> {
    let before = load(home)?;
    if before.directory.exists() && !before.owned {
        return Err(format!(
            "refusing to overwrite user-owned plugin {}",
            before.directory.display()
        ));
    }
    validate_owned_files(&before.directory)?;

    let marker = JsonFile::read(source.join(MANIFEST))?
        .filter(|file| owns(file, true))
        .ok_or("the bundled Antigravity ownership marker is invalid")?;
    let version = marker
        .value
        .get("version")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<Version>().ok())
        .ok_or("the bundled Antigravity hook version is invalid")?;
    // The provider copies its payload; publish our version only after verification.
    with_staged_bundle(source, staging_root, |staging| {
        let staged = staging.to_string_lossy();
        let home_value = home.to_string_lossy();
        let environment = [
            ("USERPROFILE", home_value.as_ref()),
            ("HOME", home_value.as_ref()),
        ];
        run("agy", &["plugin", "install", &staged], &environment)
            .map_err(|error| format!("Antigravity plugin install failed: {error}"))?;
        let copied = load(home)?;
        if !copied.registered || !copied.owned || !copied.directory.join("hooks.json").is_file() {
            return Err("Antigravity did not install the managed hooks in its expected configuration directory".into());
        }
        if !copied.enabled {
            run("agy", &["plugin", "enable", PLUGIN_NAME], &environment)
                .map_err(|error| format!("Antigravity plugin enable failed: {error}"))?;
        }
        fs::copy(source.join(MANIFEST), copied.directory.join(MANIFEST))
            .map_err(|error| format!("cannot commit Antigravity hook ownership: {error}"))?;
        let after = load(home)?;
        if !after.complete || !after.enabled || after.version != Some(version) {
            return Err("Antigravity hook installation did not pass verification".into());
        }
        Ok(())
    })
}

fn has_registration(installation: &Installation) -> bool {
    installation
        .imports
        .as_ref()
        .and_then(|file| file.value.get("imports"))
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry.get("name").and_then(Value::as_str) == Some(PLUGIN_NAME))
        })
        || installation
            .config
            .as_ref()
            .and_then(|file| file.value.get("plugins"))
            .and_then(Value::as_object)
            .is_some_and(|plugins| plugins.contains_key(PLUGIN_NAME))
}

fn remove(
    home: &Path,
    cli_available: bool,
    run: impl FnOnce(&str, &[&str], &[(&str, &str)]) -> std::io::Result<()>,
) -> Result<bool, String> {
    let root = home.join(".gemini").join("config");
    for directory in [home.join(".gemini"), root.clone(), root.join("plugins")] {
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if !metadata.file_type().is_dir() => {
                return Err(format!(
                    "{} is not a regular directory",
                    directory.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot inspect {}: {error}", directory.display())),
        }
    }
    let installation = load(home)?;
    if !installation.directory.exists() && !has_registration(&installation) {
        return Ok(false);
    }
    if !installation.owned {
        return Err(format!(
            "refusing to remove unowned Antigravity hooks from {}",
            installation.directory.display()
        ));
    }
    validate_owned_files(&installation.directory)?;
    if !cli_available {
        return Err("agy is not available; Antigravity hooks and configuration were not changed. Reinstall agy or restore it to PATH, then retry hook uninstall.".into());
    }

    // The provider owns mutations of its shared configuration; WTA only verifies the result.
    let home_value = home.to_string_lossy();
    let environment = [
        ("USERPROFILE", home_value.as_ref()),
        ("HOME", home_value.as_ref()),
    ];
    run("agy", &["plugin", "uninstall", PLUGIN_NAME], &environment)
        .map_err(|error| format!("Antigravity plugin uninstall failed: {error}"))?;
    let after = load(home)?;
    if after.directory.exists() || has_registration(&after) {
        return Err("Antigravity plugin uninstall did not remove all managed registration and assets; inspect agy plugin state and retry.".into());
    }
    Ok(true)
}

pub(super) fn uninstall(home: Option<&Path>) -> CliUninstallResult {
    uninstall_with(
        home,
        cli_binary_on_path(CliKind::Antigravity),
        |exe, args, environment| {
            run_plugin_cli_with_env(exe, args, environment, "agent_hooks", &[])
        },
    )
}

pub(super) fn uninstall_with(
    home: Option<&Path>,
    cli_available: bool,
    run: impl FnOnce(&str, &[&str], &[(&str, &str)]) -> std::io::Result<()>,
) -> CliUninstallResult {
    let mut result = CliUninstallResult {
        name: "antigravity",
        attempted: false,
        plugin_uninstalled: None,
        marketplace_removed: None,
        staging_dir_removed: true,
        messages: Vec::new(),
    };
    let Some(home) = home else {
        result.plugin_uninstalled = Some(false);
        result
            .messages
            .push("home directory unavailable; Antigravity hooks were not changed".into());
        return result;
    };
    let outcome = remove(home, cli_available, |exe, args, environment| {
        result.attempted = true;
        run(exe, args, environment)
    });
    match outcome {
        Ok(attempted) => {
            result.attempted = attempted;
            result.plugin_uninstalled = Some(true);
            result.messages.push(if attempted {
                "agy removed the managed Antigravity hook registration and assets".into()
            } else {
                "Antigravity hooks are not installed".into()
            });
        }
        Err(error) => {
            tracing::warn!(target: "agent_hooks", %error, "Antigravity hook removal failed");
            result.plugin_uninstalled = Some(false);
            result.messages.push(error);
        }
    }
    result
}
