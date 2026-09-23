// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use anyhow::{bail, ensure, Context, Result};
use std::fs::{self, File};
use std::path::{Component, Path, PathBuf, Prefix};

pub(super) fn resolve(root: &str, create: bool) -> Result<PathBuf> {
    let path = Path::new(root);
    ensure!(
        path.is_absolute(),
        "project root must be an absolute directory"
    );
    if !create {
        let canonical = path
            .canonicalize()
            .context("resolve existing project root")?;
        ensure!(
            fs::metadata(&canonical)
                .context("inspect existing project directory")?
                .is_dir(),
            "project root must be an existing directory"
        );
        return Ok(canonical);
    }

    // Path::components normalizes "." away, so inspect the original spelling too.
    ensure!(
        !root
            .split(['\\', '/'])
            .any(|part| part == "." || part == ".."),
        "new project root must not contain traversal components"
    );
    ensure!(
        !root.ends_with(['\\', '/']),
        "new project root must name a directory leaf"
    );
    match path.components().next() {
        Some(Component::Prefix(prefix))
            if matches!(
                prefix.kind(),
                Prefix::Disk(_)
                    | Prefix::VerbatimDisk(_)
                    | Prefix::UNC(_, _)
                    | Prefix::VerbatimUNC(_, _)
            ) => {}
        _ => bail!("new project root must not use a device path"),
    }
    // A verbatim prefix permits ordinary disk/UNC paths, never other namespaces.
    for component in path.components() {
        if let Component::Normal(part) = component {
            validate_component(part.to_str().context("project path is not Unicode")?)?;
        }
    }
    let leaf = path
        .file_name()
        .context("new project root requires a leaf")?;
    validate_component(leaf.to_str().context("project leaf is not Unicode")?)?;
    let parent = path
        .parent()
        .context("new project root requires an existing parent")?
        .canonicalize()
        .context("resolve existing new-project parent")?;
    ensure!(
        fs::metadata(&parent)
            .context("inspect new-project parent")?
            .is_dir(),
        "new project parent must be a directory"
    );
    let target = parent.join(leaf);
    absent(&target)?;
    Ok(target)
}

fn validate_component(part: &str) -> Result<()> {
    ensure!(
        !part.is_empty()
            && !part.ends_with(['.', ' '])
            && part.encode_utf16().count() <= 255
            && !part.chars().any(|c| c < ' ' || "<>:\"/\\|?*".contains(c)),
        "new project path contains an invalid directory name"
    );
    let stem = part.split('.').next().unwrap_or("").trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    let reserved = matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        upper.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    });
    ensure!(
        !reserved,
        "new project path contains a reserved Windows name"
    );
    Ok(())
}

fn absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("new project target already exists; it will not be adopted or overwritten"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("inspect new project target"),
    }
}

// Keep every canonical ancestor open without write/delete sharing. Besides rename,
// this prevents turning an ancestor into a junction during path-based Git operations.
pub(super) struct DirectoryPins {
    _handles: Vec<File>,
}

fn pin_parents(parent: &Path) -> Result<Vec<File>> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
    };

    let mut handles = Vec::new();
    for directory in parent.ancestors().collect::<Vec<_>>().into_iter().rev() {
        let handle = fs::OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(directory)
            .with_context(|| format!("pin new-project parent {}", directory.display()))?;
        let metadata = handle.metadata().context("inspect pinned project parent")?;
        ensure!(
            metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "new project parent changed or contains a reparse point"
        );
        handles.push(handle);
    }
    ensure!(
        parent
            .canonicalize()
            .context("verify pinned project parent")?
            == parent,
        "new project parent changed while acquiring directory handles"
    );
    Ok(handles)
}

#[cfg(test)]
pub(super) fn create(root: &str) -> Result<PathBuf> {
    create_pinned(root).map(|(path, _pins)| path)
}

pub(super) fn create_pinned(root: &str) -> Result<(PathBuf, DirectoryPins)> {
    let target = resolve(root, true)?;
    let parent = target.parent().context("new project parent missing")?;
    let mut handles = pin_parents(parent)?;
    // The requested spelling may have passed through a junction before canonicalization.
    ensure!(
        resolve(root, true)? == target,
        "new project parent changed before directory creation"
    );
    let parent_handle = handles
        .last()
        .context("new project parent handle missing")?;
    let created = create_child(parent_handle, &target)?;
    handles.push(created);
    Ok((target, DirectoryPins { _handles: handles }))
}

fn create_child(parent: &File, target: &Path) -> Result<File> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
    };

    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *mut u16,
    }
    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root_directory: *mut c_void,
        object_name: *mut UnicodeString,
        attributes: u32,
        security_descriptor: *mut c_void,
        security_quality_of_service: *mut c_void,
    }
    #[repr(C)]
    struct IoStatusBlock {
        status: isize,
        information: usize,
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn NtCreateFile(
            handle: *mut *mut c_void,
            access: u32,
            attributes: *mut ObjectAttributes,
            status: *mut IoStatusBlock,
            allocation_size: *const i64,
            file_attributes: u32,
            share: u32,
            disposition: u32,
            options: u32,
            ea_buffer: *const c_void,
            ea_length: u32,
        ) -> i32;
        fn RtlNtStatusToDosError(status: i32) -> u32;
    }

    let mut leaf: Vec<u16> = target
        .file_name()
        .context("new project leaf missing")?
        .encode_wide()
        .collect();
    let length = u16::try_from(leaf.len() * 2).context("new project leaf is too long")?;
    let mut name = UnicodeString {
        length,
        maximum_length: length,
        buffer: leaf.as_mut_ptr(),
    };
    let mut attributes = ObjectAttributes {
        length: std::mem::size_of::<ObjectAttributes>() as u32,
        root_directory: parent.as_raw_handle(),
        object_name: &mut name,
        attributes: 0x40 | 0x1000, // OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE
        security_descriptor: std::ptr::null_mut(),
        security_quality_of_service: std::ptr::null_mut(),
    };
    let mut status = IoStatusBlock {
        status: 0,
        information: 0,
    };
    let mut handle = std::ptr::null_mut();
    // NtCreateFile is needed because CreateDirectoryW followed by CreateFileW leaves
    // a replacement race. FILE_CREATE atomically refuses every existing target and
    // returns the new directory already pinned against writes, retargeting and rename.
    // SAFETY: All ABI layouts match the Windows native API; the counted name and
    // output pointers remain live for this synchronous call, and parent owns its handle.
    let result = unsafe {
        NtCreateFile(
            &mut handle,
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | 0x0010_0000, // SYNCHRONIZE
            &mut attributes,
            &mut status,
            std::ptr::null(),
            0,
            FILE_SHARE_READ,
            2,                        // FILE_CREATE, never FILE_OPEN_IF
            0x1 | 0x20 | 0x0020_0000, // directory, synchronous, open reparse point
            std::ptr::null(),
            0,
        )
    };
    if result < 0 {
        // SAFETY: This conversion accepts any NTSTATUS and owns no resources.
        let error = unsafe { RtlNtStatusToDosError(result) };
        return Err(std::io::Error::from_raw_os_error(error as i32))
            .with_context(|| format!("create new project directory {}", target.display()));
    }
    // SAFETY: A successful synchronous FILE_CREATE returned one owned file handle.
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = std::env::current_dir()
                .unwrap()
                .join("target")
                .join(format!("center-project-directory-{}", Uuid::new_v4()));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn child(&self, leaf: &str) -> String {
            self.0.join(leaf).to_str().unwrap().to_owned()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn project_directory_accepts_only_new_direct_children() {
        let fixture = Fixture::new();
        let root = fixture.child("new-project");
        let expected = fixture.0.canonicalize().unwrap().join("new-project");
        assert_eq!(resolve(&root, true).unwrap(), expected);
        assert!(!expected.exists());
        assert_eq!(create(&root).unwrap(), expected);
        assert_eq!(resolve(&root, false).unwrap(), expected);
        assert!(create(&root).is_err());
        assert!(resolve(&fixture.child("missing\\child"), true).is_err());
        assert!(!fixture.0.join("missing").exists());
        assert!(resolve("relative-project", true).is_err());
        assert!(resolve("C:\\", true).is_err());
        assert!(resolve(r"\\.\C:\project", true).is_err());
        assert!(resolve(r"\\?\GLOBALROOT\Device\HarddiskVolume1\project", true).is_err());
    }

    #[test]
    fn project_directory_rejects_raw_traversal_and_invalid_names() {
        let fixture = Fixture::new();
        for leaf in [
            ".\\new",
            "..\\new",
            "new\\.",
            "new\\..",
            "new/../other",
            "new/./other",
            "NUL",
            "con.txt",
            "COM1",
            "Lpt9.log",
            "COM¹",
            "CONOUT$",
            "x:stream",
            "x.",
            "x ",
            "x?",
            "x*",
            "x|y",
            "x<y",
            "x>y",
            "x\"y",
            "x\0y",
            "new\\",
        ] {
            assert!(resolve(&fixture.child(leaf), true).is_err(), "{leaf:?}");
        }
        assert!(resolve(&fixture.child(&"x".repeat(256)), true).is_err());
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
    }

    #[test]
    fn project_directory_preserves_existing_files_and_directories() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("existing"), "untouched").unwrap();
        assert!(resolve(&fixture.child("existing"), false).is_err());
        assert!(create(&fixture.child("existing")).is_err());
        assert_eq!(
            fs::read_to_string(fixture.0.join("existing")).unwrap(),
            "untouched"
        );
        fs::create_dir(fixture.0.join("directory")).unwrap();
        fs::write(fixture.0.join("directory").join("dirty"), "user work").unwrap();
        assert!(create(&fixture.child("directory")).is_err());
        assert_eq!(
            fs::read_to_string(fixture.0.join("directory").join("dirty")).unwrap(),
            "user work"
        );
        let parent = fixture.0.canonicalize().unwrap();
        let pins = pin_parents(&parent).unwrap();
        for leaf in ["existing", "directory"] {
            assert!(create_child(pins.last().unwrap(), &parent.join(leaf)).is_err());
        }
    }

    #[test]
    fn project_directory_refuses_reparse_targets_and_resolves_parent_aliases() {
        let fixture = Fixture::new();
        let actual = fixture.0.join("actual");
        let alias = fixture.0.join("alias");
        fs::create_dir(&actual).unwrap();
        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&alias)
            .arg(&actual)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(create(alias.to_str().unwrap()).is_err());
        let child = alias.join("new-project");
        assert_eq!(
            resolve(child.to_str().unwrap(), true).unwrap(),
            actual.canonicalize().unwrap().join("new-project")
        );
        let parent = fixture.0.canonicalize().unwrap();
        let pins = pin_parents(&parent).unwrap();
        assert!(create_child(pins.last().unwrap(), &parent.join("alias")).is_err());
        assert!(pin_parents(&parent.join("alias")).is_err());
        drop(pins);
        fs::remove_dir(&actual).unwrap();
        assert!(create(alias.to_str().unwrap()).is_err());
        fs::remove_dir(&alias).unwrap();
    }

    #[test]
    fn project_directory_pins_parent_and_created_child_until_release() {
        let fixture = Fixture::new();
        let (created, pins) = create_pinned(&fixture.child("new-project")).unwrap();
        assert!(fs::rename(&created, fixture.0.join("replacement")).is_err());
        assert!(fs::rename(&fixture.0, fixture.0.with_extension("renamed")).is_err());
        fs::write(created.join("allowed-file"), "contents").unwrap();
        drop(pins);
        fs::rename(&created, fixture.0.join("replacement")).unwrap();
    }
}
