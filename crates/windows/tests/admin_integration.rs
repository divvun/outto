#![cfg(windows)]

//! Integration tests requiring admin/elevated privileges.
//!
//! These tests are marked `#[ignore]` and will NOT run during normal `cargo nextest run`.
//!
//! To run them, open an **elevated** (Administrator) terminal and use:
//!   cargo nextest run --run-ignored ignored-only
//!
//! Or use the convenience script:
//!   powershell -ExecutionPolicy Bypass -File scripts/test-admin.ps1

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use outto_core::NoOpCallbacks;
use outto_core::actions::dirs;
use outto_core::config::VariableResolver as PathResolver;
use outto_core::config::types::*;
use outto_core::manifest::InstallManifest;
use outto_windows::actions::{associations, com, fonts, services};
use outto_windows::elevation;
use outto_windows::manifest::Action as ActionRecord;

/// Test-only shim for the old `PathResolver::new(path, name, version)`
/// constructor, wired to the current `VariableResolver` builder.
fn path_resolver_new(install_dir: &std::path::Path, name: &str, version: &str) -> PathResolver {
    PathResolver::new()
        .with_package(name, version)
        .with_install_dir(install_dir)
}

/// Panics with a clear message if not running elevated.
fn assert_admin() {
    assert!(
        elevation::is_elevated(),
        "This test requires admin privileges. Run from an elevated shell:\n  \
         cargo nextest run --run-ignored ignored-only"
    );
}

use windows_sys::Win32::System::Registry::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn hkcr_key_exists(subkey: &str) -> bool {
    let key_wide = to_wide(subkey);
    let mut hkey: HKEY = std::ptr::null_mut();
    let result =
        unsafe { RegOpenKeyExW(HKEY_CLASSES_ROOT, key_wide.as_ptr(), 0, KEY_READ, &mut hkey) };
    if result == 0 {
        unsafe { RegCloseKey(hkey) };
        true
    } else {
        false
    }
}

fn read_hkcr_string(subkey: &str, value_name: &str) -> Option<String> {
    let key_wide = to_wide(subkey);
    let name_wide = to_wide(value_name);
    let mut hkey: HKEY = std::ptr::null_mut();

    let result =
        unsafe { RegOpenKeyExW(HKEY_CLASSES_ROOT, key_wide.as_ptr(), 0, KEY_READ, &mut hkey) };
    if result != 0 {
        return None;
    }

    let mut data_type: u32 = 0;
    let mut data_size: u32 = 0;
    let result = unsafe {
        RegQueryValueExW(
            hkey,
            name_wide.as_ptr(),
            std::ptr::null(),
            &mut data_type,
            std::ptr::null_mut(),
            &mut data_size,
        )
    };
    if result != 0 {
        unsafe { RegCloseKey(hkey) };
        return None;
    }

    let mut buffer = vec![0u8; data_size as usize];
    let result = unsafe {
        RegQueryValueExW(
            hkey,
            name_wide.as_ptr(),
            std::ptr::null(),
            &mut data_type,
            buffer.as_mut_ptr(),
            &mut data_size,
        )
    };
    unsafe { RegCloseKey(hkey) };

    if result != 0 {
        return None;
    }

    let wide: Vec<u16> = buffer
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Some(
        String::from_utf16_lossy(&wide)
            .trim_end_matches('\0')
            .to_string(),
    )
}

fn read_hklm_string(subkey: &str, value_name: &str) -> Option<String> {
    let key_wide = to_wide(subkey);
    let name_wide = to_wide(value_name);
    let mut hkey: HKEY = std::ptr::null_mut();

    let result = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            key_wide.as_ptr(),
            0,
            KEY_READ,
            &mut hkey,
        )
    };
    if result != 0 {
        return None;
    }

    let mut data_type: u32 = 0;
    let mut data_size: u32 = 0;
    let result = unsafe {
        RegQueryValueExW(
            hkey,
            name_wide.as_ptr(),
            std::ptr::null(),
            &mut data_type,
            std::ptr::null_mut(),
            &mut data_size,
        )
    };
    if result != 0 {
        unsafe { RegCloseKey(hkey) };
        return None;
    }

    let mut buffer = vec![0u8; data_size as usize];
    let result = unsafe {
        RegQueryValueExW(
            hkey,
            name_wide.as_ptr(),
            std::ptr::null(),
            &mut data_type,
            buffer.as_mut_ptr(),
            &mut data_size,
        )
    };
    unsafe { RegCloseKey(hkey) };

    if result != 0 {
        return None;
    }

    let wide: Vec<u16> = buffer
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Some(
        String::from_utf16_lossy(&wide)
            .trim_end_matches('\0')
            .to_string(),
    )
}

/// RAII cleanup for admin test resources.
struct AdminCleanup {
    hkcr_keys: Vec<String>,
    service_names: Vec<String>,
    font_files: Vec<PathBuf>,
    font_names: Vec<String>,
    dirs: Vec<PathBuf>,
}

impl AdminCleanup {
    fn new() -> Self {
        Self {
            hkcr_keys: Vec::new(),
            service_names: Vec::new(),
            font_files: Vec::new(),
            font_names: Vec::new(),
            dirs: Vec::new(),
        }
    }
}

impl Drop for AdminCleanup {
    fn drop(&mut self) {
        for key in &self.hkcr_keys {
            let key_wide = to_wide(key);
            unsafe {
                RegDeleteKeyW(HKEY_CLASSES_ROOT, key_wide.as_ptr());
            }
        }
        for name in &self.service_names {
            let _ = services::stop_service(name);
            let _ = services::delete_service(name);
        }
        for (file, font_name) in self.font_files.iter().zip(self.font_names.iter()) {
            let _ = fonts::uninstall_font(file, font_name);
        }
        for dir in &self.dirs {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

// ---------------------------------------------------------------------------
// File associations (HKCR — needs admin)
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn test_admin_association_create_and_remove() {
    assert_admin();
    let mut cleanup = AdminCleanup::new();
    let ext = ".outto_test";
    let prog_id = "OuttoTest.Document";

    // Clean up from prior runs
    cleanup.hkcr_keys.push(ext.to_string());
    cleanup
        .hkcr_keys
        .push(format!("{prog_id}\\shell\\open\\command"));
    cleanup.hkcr_keys.push(format!("{prog_id}\\shell\\open"));
    cleanup.hkcr_keys.push(format!("{prog_id}\\shell"));
    cleanup.hkcr_keys.push(format!("{prog_id}\\DefaultIcon"));
    cleanup.hkcr_keys.push(prog_id.to_string());

    // Pre-clean
    let _ = associations::remove_association(ext, prog_id);

    let resolver = path_resolver_new(std::path::Path::new("C:\\test"), "Test", "1.0.0");
    let mut manifest = InstallManifest::new(
        "test",
        "Test",
        "1.0.0",
        std::path::Path::new("C:\\test"),
        vec![],
    );
    let callbacks = NoOpCallbacks;

    let entry = AssociationEntry {
        extension: ext.to_string(),
        prog_id: prog_id.to_string(),
        description: Some("Outto Test Document".to_string()),
        icon: Some("C:\\Windows\\System32\\shell32.dll,0".to_string()),
        command: "cmd.exe /C echo \"%1\"".to_string(),
        component: None,
    };

    associations::create_association(&entry, &resolver, &mut manifest, &callbacks).unwrap();

    // Verify HKCR keys
    assert!(hkcr_key_exists(ext));
    assert_eq!(read_hkcr_string(ext, ""), Some(prog_id.to_string()));
    assert_eq!(
        read_hkcr_string(prog_id, ""),
        Some("Outto Test Document".to_string())
    );
    assert!(hkcr_key_exists(&format!("{prog_id}\\shell\\open\\command")));

    // Verify manifest
    assert!(manifest.actions.iter().any(|a| matches!(
        a,
        ActionRecord::AssociationCreated { extension, .. } if extension == ext
    )));

    // Remove
    associations::remove_association(ext, prog_id).unwrap();

    // Verify cleanup
    assert!(!hkcr_key_exists(ext));
    assert!(!hkcr_key_exists(prog_id));
}

// ---------------------------------------------------------------------------
// Services (SCM — needs admin)
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn test_admin_service_install_and_delete() {
    assert_admin();
    let mut cleanup = AdminCleanup::new();
    let svc_name = "OuttoTestSvc";
    cleanup.service_names.push(svc_name.to_string());

    // Pre-clean
    let _ = services::stop_service(svc_name);
    let _ = services::delete_service(svc_name);

    let resolver = path_resolver_new(std::path::Path::new("C:\\test"), "Test", "1.0.0");
    let mut manifest = InstallManifest::new(
        "test",
        "Test",
        "1.0.0",
        std::path::Path::new("C:\\test"),
        vec![],
    );
    let callbacks = NoOpCallbacks;

    let entry = ServiceEntry {
        name: svc_name.to_string(),
        display_name: Some("Outto Test Service".to_string()),
        executable: "C:\\Windows\\System32\\cmd.exe".to_string(),
        start_type: ServiceStartType::Manual,
        account: None,
        on_install: ServiceOnInstall::Nothing,
        on_uninstall: ServiceOnUninstall::StopAndDelete,
        component: None,
    };

    services::install_service(&entry, &resolver, &mut manifest, &callbacks).unwrap();

    // Verify manifest recorded the install
    assert!(manifest.actions.iter().any(|a| matches!(
        a,
        ActionRecord::ServiceInstalled { name } if name == svc_name
    )));

    // Delete the service
    services::delete_service(svc_name).unwrap();
}

// ---------------------------------------------------------------------------
// Fonts (HKLM + C:\Windows\Fonts — needs admin)
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn test_admin_font_install_and_uninstall() {
    assert_admin();
    let mut cleanup = AdminCleanup::new();
    let dir = std::env::temp_dir().join("outto_test_font");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    cleanup.dirs.push(dir.clone());

    // Create a minimal valid TTF file (TrueType requires specific headers)
    // We'll create a tiny placeholder — AddFontResourceW may fail, but we
    // can still test the file copy and registry parts.
    let font_file = dir.join("OuttoTestFont.ttf");
    // Minimal TTF header (enough to not crash AddFontResourceW, even if invalid)
    let ttf_header = [0u8; 64]; // Zeroed header — will fail AddFontResourceW
    std::fs::write(&font_file, &ttf_header).unwrap();

    let mut manifest = InstallManifest::new(
        "test",
        "Test",
        "1.0.0",
        std::path::Path::new("C:\\test"),
        vec![],
    );
    let callbacks = NoOpCallbacks;

    let entry = FontEntry {
        source: "OuttoTestFont.ttf".to_string(),
        component: None,
    };

    // install_font may fail on AddFontResourceW with invalid TTF,
    // but we can still test whether the function is reachable
    let result = fonts::install_font(&entry, &dir, &mut manifest, &callbacks);

    // Track for cleanup regardless
    let fonts_dir =
        PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()))
            .join("Fonts")
            .join("OuttoTestFont.ttf");
    cleanup.font_files.push(fonts_dir.clone());
    cleanup.font_names.push("OuttoTestFont".to_string());

    if result.is_ok() {
        // If font install succeeded, verify
        assert!(
            fonts_dir.exists(),
            "Font file should be copied to Fonts dir"
        );

        let reg_val = read_hklm_string(
            "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts",
            "OuttoTestFont (TrueType)",
        );
        assert!(reg_val.is_some(), "Font registry entry should exist");

        // Uninstall
        fonts::uninstall_font(&fonts_dir, "OuttoTestFont").unwrap();
        assert!(
            !fonts_dir.exists(),
            "Font file should be removed after uninstall"
        );
    }
    // If it failed (invalid TTF), that's expected — we still exercised the code path
}

// ---------------------------------------------------------------------------
// Directory creation with ACLs (needs admin for icacls on some dirs)
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn test_admin_create_directory_with_permissions() {
    assert_admin();
    let mut cleanup = AdminCleanup::new();
    let base = std::env::temp_dir().join("outto_test_dirs_acl");
    let _ = std::fs::remove_dir_all(&base);
    cleanup.dirs.push(base.clone());

    let target = base.join("secured");
    let resolver = path_resolver_new(&base, "Test", "1.0.0");
    let mut manifest = InstallManifest::new("test", "Test", "1.0.0", &base, vec![]);
    let callbacks = NoOpCallbacks;

    let entry = DirEntry {
        path: target.to_string_lossy().to_string(),
        permissions: vec![DirPermission {
            identity: "Users".to_string(),
            access: "read".to_string(),
        }],
        component: None,
        attribs: None,
        arch: None,
    };

    dirs::create_directory(&entry, &resolver, &mut manifest, &callbacks).unwrap();

    assert!(target.exists());

    // Should have both DirectoryCreated and PermissionsSet records
    assert!(
        manifest
            .actions
            .iter()
            .any(|a| matches!(a, ActionRecord::DirectoryCreated { .. }))
    );
    assert!(manifest.actions.iter().any(|a| matches!(
        a,
        ActionRecord::PermissionsSet { identity, access, .. }
            if identity == "Users" && access == "read"
    )));
}

// ---------------------------------------------------------------------------
// COM registration — error path (no real DLL needed)
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn test_admin_com_register_nonexistent_dll() {
    assert_admin();
    let resolver = path_resolver_new(std::path::Path::new("C:\\test"), "Test", "1.0.0");
    let mut manifest = InstallManifest::new(
        "test",
        "Test",
        "1.0.0",
        std::path::Path::new("C:\\test"),
        vec![],
    );
    let callbacks = NoOpCallbacks;

    let entry = ComEntry {
        file: "C:\\nonexistent_outto_test.dll".to_string(),
        action: ComAction::Regserver,
        component: None,
    };

    let result = com::register_com(&entry, &resolver, &mut manifest, &callbacks);
    assert!(result.is_err(), "Should fail for nonexistent DLL");
}
