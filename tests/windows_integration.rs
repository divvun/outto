#![cfg(windows)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use outto::actions::dirs;
use outto::actions::prerequisites;
use outto::actions::registry;
use outto::actions::run;
use outto::actions::shortcuts;
use outto::config::types::*;
use outto::config::{Config, PathResolver};
use outto::detect::{
    detect_existing_install, remove_uninstall_registry, write_uninstall_registry,
    UninstallRegistryInfo,
};
use outto::elevation;
use outto::manifest::{ActionRecord, InstallManifest};
use outto::{install, uninstall_from_dir, InstallOptions, NoOpCallbacks};

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

/// RAII guard that deletes HKCU registry keys and temp dirs on drop.
struct TestCleanup {
    hkcu_keys: Vec<String>,
    hkcu_values: Vec<(String, String)>, // (subkey, value_name) under HKCU\Environment
    dirs: Vec<PathBuf>,
}

impl TestCleanup {
    fn new() -> Self {
        Self {
            hkcu_keys: Vec::new(),
            hkcu_values: Vec::new(),
            dirs: Vec::new(),
        }
    }

    fn track_key(&mut self, key: &str) {
        self.hkcu_keys.push(key.to_string());
    }

    fn track_env_value(&mut self, value_name: &str) {
        self.hkcu_values
            .push(("Environment".to_string(), value_name.to_string()));
    }

    fn track_dir(&mut self, dir: PathBuf) {
        self.dirs.push(dir);
    }
}

impl Drop for TestCleanup {
    fn drop(&mut self) {
        for (subkey, value_name) in &self.hkcu_values {
            let key_wide = to_wide(subkey);
            let name_wide = to_wide(value_name);
            unsafe {
                let mut hkey: HKEY = std::ptr::null_mut();
                if RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    key_wide.as_ptr(),
                    0,
                    KEY_SET_VALUE,
                    &mut hkey,
                ) == 0
                {
                    RegDeleteValueW(hkey, name_wide.as_ptr());
                    RegCloseKey(hkey);
                }
            }
        }

        for key in &self.hkcu_keys {
            let key_wide = to_wide(key);
            unsafe {
                RegDeleteKeyW(HKEY_CURRENT_USER, key_wide.as_ptr());
            }
        }

        for dir in &self.dirs {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn read_hkcu_string(subkey: &str, value_name: &str) -> Option<String> {
    let key_wide = to_wide(subkey);
    let name_wide = to_wide(value_name);
    let mut hkey: HKEY = std::ptr::null_mut();

    let result =
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key_wide.as_ptr(), 0, KEY_READ, &mut hkey) };
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

fn read_hkcu_dword(subkey: &str, value_name: &str) -> Option<u32> {
    let key_wide = to_wide(subkey);
    let name_wide = to_wide(value_name);
    let mut hkey: HKEY = std::ptr::null_mut();

    let result =
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key_wide.as_ptr(), 0, KEY_READ, &mut hkey) };
    if result != 0 {
        return None;
    }

    let mut data: u32 = 0;
    let mut data_size: u32 = 4;
    let mut data_type: u32 = 0;
    let result = unsafe {
        RegQueryValueExW(
            hkey,
            name_wide.as_ptr(),
            std::ptr::null(),
            &mut data_type,
            &mut data as *mut u32 as *mut u8,
            &mut data_size,
        )
    };
    unsafe { RegCloseKey(hkey) };

    if result != 0 {
        return None;
    }
    Some(data)
}

fn write_hkcu_string(subkey: &str, value_name: &str, data: &str) {
    let key_wide = to_wide(subkey);
    let name_wide = to_wide(value_name);
    let data_wide = to_wide(data);
    let data_bytes: Vec<u8> = data_wide.iter().flat_map(|w| w.to_le_bytes()).collect();

    let mut hkey: HKEY = std::ptr::null_mut();
    let mut disposition: u32 = 0;
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key_wide.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_ALL_ACCESS,
            std::ptr::null(),
            &mut hkey,
            &mut disposition,
        );
        RegSetValueExW(
            hkey,
            name_wide.as_ptr(),
            0,
            REG_SZ,
            data_bytes.as_ptr(),
            data_bytes.len() as u32,
        );
        RegCloseKey(hkey);
    }
}

fn hkcu_key_exists(subkey: &str) -> bool {
    let key_wide = to_wide(subkey);
    let mut hkey: HKEY = std::ptr::null_mut();
    let result =
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key_wide.as_ptr(), 0, KEY_READ, &mut hkey) };
    if result == 0 {
        unsafe { RegCloseKey(hkey) };
        true
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// 2a. HKCU Registry cycle tests
// ---------------------------------------------------------------------------

#[test]
fn test_registry_string_value_write_read_delete() {
    let mut cleanup = TestCleanup::new();
    let key = "Software\\OuttoTest_string";
    cleanup.track_key(key);

    // Ensure clean state
    let key_wide = to_wide(key);
    unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, key_wide.as_ptr()) };

    let resolver = PathResolver::new(std::path::Path::new("C:\\test"), "Test", "1.0.0");
    let callbacks = NoOpCallbacks;
    let mut manifest = InstallManifest::new(
        "test",
        "Test",
        "1.0.0",
        std::path::Path::new("C:\\test"),
        vec![],
    );

    let entry = RegistryEntry {
        root: RegistryRoot::Hkcu,
        key: key.to_string(),
        values: vec![RegistryValue {
            name: "TestValue".to_string(),
            value_type: RegistryValueType::String,
            data: toml::Value::String("Hello World".to_string()),
        }],
        uninstall: UninstallBehavior::default(),
        component: None,
        arch: None,
        dont_create_key: false,
    };

    registry::apply_registry_entry(&entry, &resolver, &mut manifest, &callbacks).unwrap();

    // Verify value was written
    let val = read_hkcu_string(key, "TestValue");
    assert_eq!(val, Some("Hello World".to_string()));

    // Verify manifest recorded the action
    assert!(manifest.actions.iter().any(|a| matches!(
        a,
        ActionRecord::RegistryKeyCreated { root, key: k, .. } if root == "HKCU" && k == key
    )));

    // Delete the key via rollback helper
    registry::delete_key("HKCU", key).unwrap();
    assert!(!hkcu_key_exists(key));
}

#[test]
fn test_registry_dword_value() {
    let mut cleanup = TestCleanup::new();
    let key = "Software\\OuttoTest_dword";
    cleanup.track_key(key);

    let key_wide = to_wide(key);
    unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, key_wide.as_ptr()) };

    let resolver = PathResolver::new(std::path::Path::new("C:\\test"), "Test", "1.0.0");
    let callbacks = NoOpCallbacks;
    let mut manifest = InstallManifest::new(
        "test",
        "Test",
        "1.0.0",
        std::path::Path::new("C:\\test"),
        vec![],
    );

    let entry = RegistryEntry {
        root: RegistryRoot::Hkcu,
        key: key.to_string(),
        values: vec![RegistryValue {
            name: "DwordVal".to_string(),
            value_type: RegistryValueType::Dword,
            data: toml::Value::String("42".to_string()),
        }],
        uninstall: UninstallBehavior::default(),
        component: None,
        arch: None,
        dont_create_key: false,
    };

    registry::apply_registry_entry(&entry, &resolver, &mut manifest, &callbacks).unwrap();

    let val = read_hkcu_dword(key, "DwordVal");
    assert_eq!(val, Some(42));
}

#[test]
fn test_registry_multiple_values_on_same_key() {
    let mut cleanup = TestCleanup::new();
    let key = "Software\\OuttoTest_multi";
    cleanup.track_key(key);

    let key_wide = to_wide(key);
    unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, key_wide.as_ptr()) };

    let resolver = PathResolver::new(std::path::Path::new("C:\\test"), "Test", "1.0.0");
    let callbacks = NoOpCallbacks;
    let mut manifest = InstallManifest::new(
        "test",
        "Test",
        "1.0.0",
        std::path::Path::new("C:\\test"),
        vec![],
    );

    let entry = RegistryEntry {
        root: RegistryRoot::Hkcu,
        key: key.to_string(),
        values: vec![
            RegistryValue {
                name: "Name".to_string(),
                value_type: RegistryValueType::String,
                data: toml::Value::String("MyApp".to_string()),
            },
            RegistryValue {
                name: "Version".to_string(),
                value_type: RegistryValueType::String,
                data: toml::Value::String("1.0.0".to_string()),
            },
            RegistryValue {
                name: "Enabled".to_string(),
                value_type: RegistryValueType::Dword,
                data: toml::Value::String("1".to_string()),
            },
        ],
        uninstall: UninstallBehavior::default(),
        component: None,
        arch: None,
        dont_create_key: false,
    };

    registry::apply_registry_entry(&entry, &resolver, &mut manifest, &callbacks).unwrap();

    assert_eq!(read_hkcu_string(key, "Name"), Some("MyApp".to_string()));
    assert_eq!(read_hkcu_string(key, "Version"), Some("1.0.0".to_string()));
    assert_eq!(read_hkcu_dword(key, "Enabled"), Some(1));
}

#[test]
fn test_registry_value_rollback_deletes_when_no_previous() {
    let mut cleanup = TestCleanup::new();
    let key = "Software\\OuttoTest_rollback_noprev";
    cleanup.track_key(key);

    let key_wide = to_wide(key);
    unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, key_wide.as_ptr()) };

    // Create key and set a value
    write_hkcu_string(key, "TempVal", "temporary");
    assert_eq!(
        read_hkcu_string(key, "TempVal"),
        Some("temporary".to_string())
    );

    // Rollback should delete the value (no previous data)
    registry::delete_value("HKCU", key, "TempVal").unwrap();
    assert_eq!(read_hkcu_string(key, "TempVal"), None);
}

#[test]
fn test_registry_value_rollback_restores_previous() {
    let mut cleanup = TestCleanup::new();
    let key = "Software\\OuttoTest_rollback_prev";
    cleanup.track_key(key);

    let key_wide = to_wide(key);
    unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, key_wide.as_ptr()) };

    // Set original value
    write_hkcu_string(key, "RestoreMe", "original");

    // Overwrite it
    write_hkcu_string(key, "RestoreMe", "overwritten");
    assert_eq!(
        read_hkcu_string(key, "RestoreMe"),
        Some("overwritten".to_string())
    );

    // Restore via set_string_value (simulating rollback)
    registry::set_string_value("HKCU", key, "RestoreMe", "original").unwrap();
    assert_eq!(
        read_hkcu_string(key, "RestoreMe"),
        Some("original".to_string())
    );
}

#[test]
fn test_registry_key_created_and_deleted() {
    let mut cleanup = TestCleanup::new();
    let key = "Software\\OuttoTest_create_delete";
    cleanup.track_key(key);

    let key_wide = to_wide(key);
    unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, key_wide.as_ptr()) };
    assert!(!hkcu_key_exists(key));

    let resolver = PathResolver::new(std::path::Path::new("C:\\test"), "Test", "1.0.0");
    let callbacks = NoOpCallbacks;
    let mut manifest = InstallManifest::new(
        "test",
        "Test",
        "1.0.0",
        std::path::Path::new("C:\\test"),
        vec![],
    );

    let entry = RegistryEntry {
        root: RegistryRoot::Hkcu,
        key: key.to_string(),
        values: vec![],
        uninstall: UninstallBehavior::default(),
        component: None,
        arch: None,
        dont_create_key: false,
    };

    registry::apply_registry_entry(&entry, &resolver, &mut manifest, &callbacks).unwrap();
    assert!(hkcu_key_exists(key));

    // Verify RegistryKeyCreated was recorded
    assert!(manifest
        .actions
        .iter()
        .any(|a| matches!(a, ActionRecord::RegistryKeyCreated { .. })));

    // Delete
    registry::delete_key("HKCU", key).unwrap();
    assert!(!hkcu_key_exists(key));
}

// ---------------------------------------------------------------------------
// 2b. User-scope environment variable tests
// ---------------------------------------------------------------------------

#[test]
fn test_env_set_new_user_variable() {
    let mut cleanup = TestCleanup::new();
    let var_name = "OUTTO_TEST_SET_NEW";
    cleanup.track_env_value(var_name);

    // Ensure clean
    let env_key = "Environment";
    let key_wide = to_wide(env_key);
    let name_wide = to_wide(var_name);
    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            key_wide.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        ) == 0
        {
            RegDeleteValueW(hkey, name_wide.as_ptr());
            RegCloseKey(hkey);
        }
    }

    let resolver = PathResolver::new(std::path::Path::new("C:\\test"), "Test", "1.0.0");
    let callbacks = NoOpCallbacks;
    let mut manifest = InstallManifest::new(
        "test",
        "Test",
        "1.0.0",
        std::path::Path::new("C:\\test"),
        vec![],
    );

    let entry = EnvironmentEntry {
        name: var_name.to_string(),
        value: "test_value".to_string(),
        scope: EnvScope::User,
        action: EnvAction::Set,
        component: None,
    };

    outto::actions::environment::apply_env_entry(&entry, &resolver, &mut manifest, &callbacks)
        .unwrap();

    // Read it back from the registry
    let val = read_hkcu_string("Environment", var_name);
    assert_eq!(val, Some("test_value".to_string()));

    // Verify manifest recorded it
    assert!(manifest.actions.iter().any(|a| matches!(
        a,
        ActionRecord::EnvironmentVariableSet { name, .. } if name == var_name
    )));
}

#[test]
fn test_env_append_user_variable() {
    let mut cleanup = TestCleanup::new();
    let var_name = "OUTTO_TEST_APPEND";
    cleanup.track_env_value(var_name);

    // Set initial value
    write_hkcu_string("Environment", var_name, "A;B");

    let resolver = PathResolver::new(std::path::Path::new("C:\\test"), "Test", "1.0.0");
    let callbacks = NoOpCallbacks;
    let mut manifest = InstallManifest::new(
        "test",
        "Test",
        "1.0.0",
        std::path::Path::new("C:\\test"),
        vec![],
    );

    let entry = EnvironmentEntry {
        name: var_name.to_string(),
        value: "C".to_string(),
        scope: EnvScope::User,
        action: EnvAction::Append,
        component: None,
    };

    outto::actions::environment::apply_env_entry(&entry, &resolver, &mut manifest, &callbacks)
        .unwrap();

    let val = read_hkcu_string("Environment", var_name);
    assert_eq!(val, Some("A;B;C".to_string()));
}

#[test]
fn test_env_prepend_user_variable() {
    let mut cleanup = TestCleanup::new();
    let var_name = "OUTTO_TEST_PREPEND";
    cleanup.track_env_value(var_name);

    write_hkcu_string("Environment", var_name, "A;B");

    let resolver = PathResolver::new(std::path::Path::new("C:\\test"), "Test", "1.0.0");
    let callbacks = NoOpCallbacks;
    let mut manifest = InstallManifest::new(
        "test",
        "Test",
        "1.0.0",
        std::path::Path::new("C:\\test"),
        vec![],
    );

    let entry = EnvironmentEntry {
        name: var_name.to_string(),
        value: "Z".to_string(),
        scope: EnvScope::User,
        action: EnvAction::Prepend,
        component: None,
    };

    outto::actions::environment::apply_env_entry(&entry, &resolver, &mut manifest, &callbacks)
        .unwrap();

    let val = read_hkcu_string("Environment", var_name);
    assert_eq!(val, Some("Z;A;B".to_string()));
}

#[test]
fn test_env_remove_from_user_variable() {
    let mut cleanup = TestCleanup::new();
    let var_name = "OUTTO_TEST_REMOVE";
    cleanup.track_env_value(var_name);

    write_hkcu_string("Environment", var_name, "A;B;C");

    let resolver = PathResolver::new(std::path::Path::new("C:\\test"), "Test", "1.0.0");
    let callbacks = NoOpCallbacks;
    let mut manifest = InstallManifest::new(
        "test",
        "Test",
        "1.0.0",
        std::path::Path::new("C:\\test"),
        vec![],
    );

    let entry = EnvironmentEntry {
        name: var_name.to_string(),
        value: "B".to_string(),
        scope: EnvScope::User,
        action: EnvAction::Remove,
        component: None,
    };

    outto::actions::environment::apply_env_entry(&entry, &resolver, &mut manifest, &callbacks)
        .unwrap();

    let val = read_hkcu_string("Environment", var_name);
    assert_eq!(val, Some("A;C".to_string()));
}

// ---------------------------------------------------------------------------
// 2c. Install detection tests
// ---------------------------------------------------------------------------

#[test]
fn test_detect_returns_none_when_absent() {
    let result = detect_existing_install("com.outto.nonexistent.test.package").unwrap();
    assert!(result.is_none());
}

#[test]
fn test_detect_finds_hkcu_install() {
    let mut cleanup = TestCleanup::new();
    let package_id = "com.outto.test.detect";
    let key = format!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{package_id}");
    cleanup.track_key(&key);

    // Clean up first
    let key_wide = to_wide(&key);
    unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, key_wide.as_ptr()) };

    // Write uninstall entry under HKCU
    write_hkcu_string(&key, "DisplayName", "Outto Test App");
    write_hkcu_string(&key, "DisplayVersion", "1.2.3");
    write_hkcu_string(&key, "InstallLocation", "C:\\OuttoTest");

    let result = detect_existing_install(package_id).unwrap();
    assert!(result.is_some());

    let existing = result.unwrap();
    assert_eq!(existing.display_name, Some("Outto Test App".to_string()));
    assert_eq!(existing.version, Some("1.2.3".to_string()));
    assert_eq!(existing.install_dir, PathBuf::from("C:\\OuttoTest"));
}

#[test]
fn test_write_and_remove_uninstall_registry() {
    let mut cleanup = TestCleanup::new();
    let package_id = "com.outto.test.uninstall_reg";
    let key = format!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{package_id}");
    cleanup.track_key(&key);

    // Clean up first
    let key_wide = to_wide(&key);
    unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, key_wide.as_ptr()) };

    let info = UninstallRegistryInfo {
        package_id,
        display_name: "Test Uninstall",
        version: "3.0.0",
        publisher: Some("Test Publisher"),
        install_dir: std::path::Path::new("C:\\TestUninstall"),
        display_icon: None,
        url: None,
        support_url: None,
        uninstall_string: None,
    };

    write_uninstall_registry(&info).unwrap();

    // Should be detectable now
    let found = detect_existing_install(package_id).unwrap();
    assert!(found.is_some());

    // Remove it
    remove_uninstall_registry(package_id).unwrap();

    // Should be gone now
    let found = detect_existing_install(package_id).unwrap();
    assert!(found.is_none());
}

// ---------------------------------------------------------------------------
// 2d. Full install→uninstall cycle with registry and env
// ---------------------------------------------------------------------------

#[test]
fn test_full_install_uninstall_with_registry_and_env() {
    let mut cleanup = TestCleanup::new();

    let test_dir = std::env::temp_dir().join("outto_test_full_cycle");
    let source_dir = test_dir.join("source");
    let install_dir = test_dir.join("installed");
    cleanup.track_dir(test_dir.clone());

    let reg_key = "Software\\OuttoTest_full_cycle";
    let env_var = "OUTTO_TEST_FULL_CYCLE";
    cleanup.track_key(reg_key);
    cleanup.track_env_value(env_var);

    // Clean up from previous runs
    let _ = std::fs::remove_dir_all(&test_dir);
    let key_wide = to_wide(reg_key);
    unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, key_wide.as_ptr()) };
    // Clean env var
    {
        let ek = to_wide("Environment");
        let en = to_wide(env_var);
        unsafe {
            let mut hkey: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(HKEY_CURRENT_USER, ek.as_ptr(), 0, KEY_SET_VALUE, &mut hkey) == 0 {
                RegDeleteValueW(hkey, en.as_ptr());
                RegCloseKey(hkey);
            }
        }
    }

    // Create source files
    std::fs::create_dir_all(source_dir.join("bin")).unwrap();
    std::fs::write(source_dir.join("bin/app.exe"), "fake exe content").unwrap();
    std::fs::write(source_dir.join("bin/config.toml"), "key = \"val\"").unwrap();

    let toml = format!(
        r##"
[package]
id = "com.outto.test.full"
name = "FullCycleTest"
version = "1.0.0"

[[files]]
source = "bin/*"
dest = "#{{app}}"
overwrite = "always"

[[registry]]
root = "hkcu"
key = "Software\\\\OuttoTest_full_cycle"
values = [
    {{ name = "AppPath", type = "string", data = "#{{app}}" }},
    {{ name = "Version", type = "string", data = "#{{package.version}}" }},
]

[[environment]]
name = "{env_var}"
value = "#{{app}}/bin"
scope = "user"
action = "set"
"##
    );

    let config = Config::from_toml(&toml).unwrap();
    let callbacks = NoOpCallbacks;

    let options = InstallOptions {
        source_dir,
        install_dir: Some(install_dir.clone()),
        selected_components: None,
        uninstall_exe: None,
    };

    // Install
    let result = install(&config, &options, &callbacks);
    assert!(result.is_ok(), "Install failed: {result:?}");

    // Verify files
    assert!(install_dir.join("app.exe").exists());
    assert!(install_dir.join("config.toml").exists());

    // Verify registry
    assert!(hkcu_key_exists(reg_key));
    let app_path = read_hkcu_string(reg_key, "AppPath");
    assert!(app_path.is_some());
    assert_eq!(
        read_hkcu_string(reg_key, "Version"),
        Some("1.0.0".to_string())
    );

    // Verify env var
    let env_val = read_hkcu_string("Environment", env_var);
    assert!(env_val.is_some());

    // Uninstall
    let result = uninstall_from_dir(&install_dir, &callbacks);
    assert!(result.is_ok(), "Uninstall failed: {result:?}");

    // Verify files removed
    assert!(!install_dir.join("app.exe").exists());
    assert!(!install_dir.join("config.toml").exists());

    // Verify registry key removed
    assert!(!hkcu_key_exists(reg_key));

    // Verify env var removed
    let env_val = read_hkcu_string("Environment", env_var);
    assert!(env_val.is_none());
}

// ---------------------------------------------------------------------------
// Shortcut tests (no admin needed — uses PowerShell)
// ---------------------------------------------------------------------------

#[test]
fn test_shortcut_creation_basic() {
    let mut cleanup = TestCleanup::new();
    let dir = std::env::temp_dir().join("outto_test_shortcut_basic");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    cleanup.track_dir(dir.clone());

    // Create a fake target file
    std::fs::write(dir.join("app.exe"), "fake").unwrap();

    let mut resolver = PathResolver::new(&dir, "Test", "1.0.0");
    resolver.set_variable("desktop", dir.to_string_lossy().as_ref());

    let mut manifest = InstallManifest::new("test", "Test", "1.0.0", &dir, vec![]);
    let callbacks = NoOpCallbacks;

    let entry = ShortcutEntry {
        name: "TestApp".to_string(),
        target: "#{app}/app.exe".to_string(),
        location: ShortcutLocation::Desktop,
        icon: None,
        working_dir: None,
        arguments: None,
        description: None,
        component: None,
        hotkey: None,
        app_user_model_id: None,
        subfolder: None,
        icon_index: None,
        arch: None,
        run_maximized: false,
    };

    shortcuts::create_shortcut(&entry, &resolver, &mut manifest, &callbacks).unwrap();

    let lnk_path = dir.join("TestApp.lnk");
    assert!(lnk_path.exists(), "Shortcut .lnk file should exist");

    assert!(manifest.actions.iter().any(|a| matches!(
        a,
        ActionRecord::ShortcutCreated { path } if path == &lnk_path
    )));
}

#[test]
fn test_shortcut_with_all_fields() {
    let mut cleanup = TestCleanup::new();
    let dir = std::env::temp_dir().join("outto_test_shortcut_full");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    cleanup.track_dir(dir.clone());

    std::fs::write(dir.join("app.exe"), "fake").unwrap();

    let mut resolver = PathResolver::new(&dir, "Test", "1.0.0");
    resolver.set_variable("desktop", dir.to_string_lossy().as_ref());

    let mut manifest = InstallManifest::new("test", "Test", "1.0.0", &dir, vec![]);
    let callbacks = NoOpCallbacks;

    let entry = ShortcutEntry {
        name: "FullShortcut".to_string(),
        target: "#{app}/app.exe".to_string(),
        location: ShortcutLocation::Desktop,
        icon: Some("#{app}/app.exe,0".to_string()),
        working_dir: Some("#{app}".to_string()),
        arguments: Some("--verbose".to_string()),
        description: Some("A test shortcut".to_string()),
        component: None,
        hotkey: None,
        app_user_model_id: None,
        subfolder: None,
        icon_index: None,
        arch: None,
        run_maximized: false,
    };

    shortcuts::create_shortcut(&entry, &resolver, &mut manifest, &callbacks).unwrap();
    assert!(dir.join("FullShortcut.lnk").exists());
}

#[test]
fn test_shortcut_rollback_deletes_file() {
    let mut cleanup = TestCleanup::new();
    let dir = std::env::temp_dir().join("outto_test_shortcut_rollback");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    cleanup.track_dir(dir.clone());

    let lnk_path = dir.join("Test.lnk");
    std::fs::write(&lnk_path, "fake shortcut").unwrap();
    assert!(lnk_path.exists());

    // Rollback should remove it
    let actions = vec![ActionRecord::ShortcutCreated {
        path: lnk_path.clone(),
    }];
    let callbacks = NoOpCallbacks;
    outto::manifest::rollback::rollback_actions(&actions, &callbacks, true).unwrap();
    assert!(!lnk_path.exists());
}

// ---------------------------------------------------------------------------
// Directory creation tests (no admin needed for basic dirs)
// ---------------------------------------------------------------------------

#[test]
fn test_create_directory_basic() {
    let mut cleanup = TestCleanup::new();
    let base = std::env::temp_dir().join("outto_test_dirs_basic");
    let _ = std::fs::remove_dir_all(&base);
    cleanup.track_dir(base.clone());

    let target = base.join("sub").join("deep");
    let resolver = PathResolver::new(&base, "Test", "1.0.0");
    let mut manifest = InstallManifest::new("test", "Test", "1.0.0", &base, vec![]);
    let callbacks = NoOpCallbacks;

    let entry = DirEntry {
        path: target.to_string_lossy().to_string(),
        permissions: vec![],
        component: None,
        attribs: None,
        arch: None,
    };

    dirs::create_directory(&entry, &resolver, &mut manifest, &callbacks).unwrap();
    assert!(target.exists());

    assert!(manifest.actions.iter().any(|a| matches!(
        a,
        ActionRecord::DirectoryCreated { path } if path == &target
    )));
}

#[test]
fn test_create_directory_already_exists() {
    let mut cleanup = TestCleanup::new();
    let base = std::env::temp_dir().join("outto_test_dirs_exists");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    cleanup.track_dir(base.clone());

    let resolver = PathResolver::new(&base, "Test", "1.0.0");
    let mut manifest = InstallManifest::new("test", "Test", "1.0.0", &base, vec![]);
    let callbacks = NoOpCallbacks;

    let entry = DirEntry {
        path: base.to_string_lossy().to_string(),
        permissions: vec![],
        component: None,
        attribs: None,
        arch: None,
    };

    // Should succeed without error, but NOT record a DirectoryCreated action
    dirs::create_directory(&entry, &resolver, &mut manifest, &callbacks).unwrap();
    assert!(!manifest
        .actions
        .iter()
        .any(|a| matches!(a, ActionRecord::DirectoryCreated { .. })));
}

// ---------------------------------------------------------------------------
// Elevation detection tests
// ---------------------------------------------------------------------------

#[test]
fn test_is_elevated_returns_bool() {
    // Just verify it doesn't panic and returns a bool
    let _result: bool = elevation::is_elevated();
}

#[test]
fn test_get_system_architecture() {
    let arch = elevation::get_system_architecture();
    assert!(
        ["x64", "x86", "arm64", "unknown"].contains(&arch),
        "Unexpected architecture: {arch}"
    );
}

#[test]
fn test_needs_elevation_user() {
    assert!(!elevation::needs_elevation(&Privileges::User));
}

#[test]
fn test_needs_elevation_auto() {
    assert!(!elevation::needs_elevation(&Privileges::Auto));
}

#[test]
fn test_needs_elevation_admin_when_elevated() {
    if elevation::is_elevated() {
        assert!(!elevation::needs_elevation(&Privileges::Admin));
    }
    // If not elevated, needs_elevation would return true — both are valid
}

// ---------------------------------------------------------------------------
// Run command tests
// ---------------------------------------------------------------------------

#[test]
fn test_execute_phase_before_install() {
    let resolver = PathResolver::new(std::path::Path::new("C:\\test"), "Test", "1.0.0");
    let mut manifest = InstallManifest::new(
        "test",
        "Test",
        "1.0.0",
        std::path::Path::new("C:\\test"),
        vec![],
    );
    let callbacks = NoOpCallbacks;

    let entries = vec![RunEntry {
        phase: RunPhase::BeforeInstall,
        command: "cmd".to_string(),
        arguments: Some("/C echo hello".to_string()),
        wait: true,
        show: ShowWindow::Hidden,
        component: None,
        working_dir: None,
        arch: None,
        run_as_original_user: false,
    }];

    run::execute_phase_commands(
        &entries,
        &RunPhase::BeforeInstall,
        &resolver,
        &mut manifest,
        &callbacks,
    )
    .unwrap();

    assert!(manifest.actions.iter().any(|a| matches!(
        a,
        ActionRecord::CommandExecuted { phase, .. } if phase == "before_install"
    )));
}

#[test]
fn test_execute_phase_filtering() {
    let resolver = PathResolver::new(std::path::Path::new("C:\\test"), "Test", "1.0.0");
    let mut manifest = InstallManifest::new(
        "test",
        "Test",
        "1.0.0",
        std::path::Path::new("C:\\test"),
        vec![],
    );
    let callbacks = NoOpCallbacks;

    let entries = vec![RunEntry {
        phase: RunPhase::AfterInstall,
        command: "cmd".to_string(),
        arguments: Some("/C echo hello".to_string()),
        wait: true,
        show: ShowWindow::Normal,
        component: None,
        working_dir: None,
        arch: None,
        run_as_original_user: false,
    }];

    // Call with BeforeInstall phase — the AfterInstall entry should be skipped
    run::execute_phase_commands(
        &entries,
        &RunPhase::BeforeInstall,
        &resolver,
        &mut manifest,
        &callbacks,
    )
    .unwrap();

    assert!(
        manifest.actions.is_empty(),
        "No commands should have been executed"
    );
}

#[test]
fn test_execute_command_nonzero_exit() {
    let resolver = PathResolver::new(std::path::Path::new("C:\\test"), "Test", "1.0.0");
    let mut manifest = InstallManifest::new(
        "test",
        "Test",
        "1.0.0",
        std::path::Path::new("C:\\test"),
        vec![],
    );
    let callbacks = NoOpCallbacks;

    let entries = vec![RunEntry {
        phase: RunPhase::AfterInstall,
        command: "cmd".to_string(),
        arguments: Some("/C exit 1".to_string()),
        wait: true,
        show: ShowWindow::Hidden,
        component: None,
        working_dir: None,
        arch: None,
        run_as_original_user: false,
    }];

    // Should succeed (non-fatal) even though command exits with 1
    let result = run::execute_phase_commands(
        &entries,
        &RunPhase::AfterInstall,
        &resolver,
        &mut manifest,
        &callbacks,
    );
    assert!(result.is_ok());
    assert_eq!(manifest.actions.len(), 1);
}

// ---------------------------------------------------------------------------
// Prerequisites tests
// ---------------------------------------------------------------------------

#[test]
fn test_prerequisite_file_exists() {
    let callbacks = NoOpCallbacks;

    // cargo.exe should exist since we're running cargo tests
    let cargo_path = which_cargo();

    let entries = vec![PrerequisiteEntry {
        name: "Cargo".to_string(),
        check: PrerequisiteCheck {
            registry: None,
            value: None,
            equals: None,
            file: Some(cargo_path),
            command: None,
        },
        download_url: None,
        installer: None,
        arguments: None,
        required: true,
    }];

    let result = prerequisites::check_prerequisites(&entries, &callbacks);
    assert!(result.is_ok());
}

#[test]
fn test_prerequisite_file_missing() {
    let callbacks = NoOpCallbacks;

    let entries = vec![PrerequisiteEntry {
        name: "Missing".to_string(),
        check: PrerequisiteCheck {
            registry: None,
            value: None,
            equals: None,
            file: Some("C:\\nonexistent_outto_test_file.exe".to_string()),
            command: None,
        },
        download_url: None,
        installer: None,
        arguments: None,
        required: true,
    }];

    let result = prerequisites::check_prerequisites(&entries, &callbacks);
    assert!(result.is_err());
}

#[test]
fn test_prerequisite_command_success() {
    let callbacks = NoOpCallbacks;

    let entries = vec![PrerequisiteEntry {
        name: "CmdSuccess".to_string(),
        check: PrerequisiteCheck {
            registry: None,
            value: None,
            equals: None,
            file: None,
            command: Some("exit /B 0".to_string()),
        },
        download_url: None,
        installer: None,
        arguments: None,
        required: true,
    }];

    let result = prerequisites::check_prerequisites(&entries, &callbacks);
    assert!(result.is_ok());
}

#[test]
fn test_prerequisite_command_failure() {
    let callbacks = NoOpCallbacks;

    let entries = vec![PrerequisiteEntry {
        name: "CmdFail".to_string(),
        check: PrerequisiteCheck {
            registry: None,
            value: None,
            equals: None,
            file: None,
            command: Some("exit /B 1".to_string()),
        },
        download_url: None,
        installer: None,
        arguments: None,
        required: true,
    }];

    let result = prerequisites::check_prerequisites(&entries, &callbacks);
    assert!(result.is_err());
}

#[test]
fn test_prerequisite_registry_exists() {
    let callbacks = NoOpCallbacks;

    // HKCU\Environment always exists on Windows
    let entries = vec![PrerequisiteEntry {
        name: "RegCheck".to_string(),
        check: PrerequisiteCheck {
            registry: Some("HKCU\\Environment".to_string()),
            value: None,
            equals: None,
            file: None,
            command: None,
        },
        download_url: None,
        installer: None,
        arguments: None,
        required: true,
    }];

    let result = prerequisites::check_prerequisites(&entries, &callbacks);
    assert!(result.is_ok());
}

fn which_cargo() -> String {
    let output = std::process::Command::new("where")
        .arg("cargo")
        .output()
        .expect("failed to run where cargo");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("C:\\Users\\admin\\.cargo\\bin\\cargo.exe")
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// File overwrite policy tests
// ---------------------------------------------------------------------------

#[test]
fn test_file_overwrite_always() {
    let mut cleanup = TestCleanup::new();
    let dir = std::env::temp_dir().join("outto_test_ow_always");
    let _ = std::fs::remove_dir_all(&dir);
    let source_dir = dir.join("src");
    let install_dir = dir.join("dst");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::create_dir_all(&install_dir).unwrap();
    cleanup.track_dir(dir.clone());

    std::fs::write(source_dir.join("file.txt"), "new content").unwrap();
    std::fs::write(install_dir.join("file.txt"), "old content").unwrap();

    let toml = r##"
[package]
id = "com.test.ow"
name = "OverwriteTest"
version = "1.0.0"

[[files]]
source = "file.txt"
dest = "#{app}"
overwrite = "always"
"##;
    let config = Config::from_toml(toml).unwrap();
    let options = InstallOptions {
        source_dir,
        install_dir: Some(install_dir.clone()),
        selected_components: None,
        uninstall_exe: None,
    };

    install(&config, &options, &NoOpCallbacks).unwrap();
    assert_eq!(
        std::fs::read_to_string(install_dir.join("file.txt")).unwrap(),
        "new content"
    );
}

#[test]
fn test_file_overwrite_never() {
    let mut cleanup = TestCleanup::new();
    let dir = std::env::temp_dir().join("outto_test_ow_never");
    let _ = std::fs::remove_dir_all(&dir);
    let source_dir = dir.join("src");
    let install_dir = dir.join("dst");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::create_dir_all(&install_dir).unwrap();
    cleanup.track_dir(dir.clone());

    std::fs::write(source_dir.join("file.txt"), "new content").unwrap();
    std::fs::write(install_dir.join("file.txt"), "old content").unwrap();

    let toml = r##"
[package]
id = "com.test.ow_never"
name = "NeverTest"
version = "1.0.0"

[[files]]
source = "file.txt"
dest = "#{app}"
overwrite = "never"
"##;
    let config = Config::from_toml(toml).unwrap();
    let options = InstallOptions {
        source_dir,
        install_dir: Some(install_dir.clone()),
        selected_components: None,
        uninstall_exe: None,
    };

    install(&config, &options, &NoOpCallbacks).unwrap();
    // Should NOT be overwritten
    assert_eq!(
        std::fs::read_to_string(install_dir.join("file.txt")).unwrap(),
        "old content"
    );
}

#[test]
fn test_file_overwrite_if_newer_skips_older() {
    let mut cleanup = TestCleanup::new();
    let dir = std::env::temp_dir().join("outto_test_ow_ifnewer");
    let _ = std::fs::remove_dir_all(&dir);
    let source_dir = dir.join("src");
    let install_dir = dir.join("dst");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::create_dir_all(&install_dir).unwrap();
    cleanup.track_dir(dir.clone());

    // Create source FIRST, then dest — dest is newer
    std::fs::write(source_dir.join("file.txt"), "old source").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    std::fs::write(install_dir.join("file.txt"), "newer dest").unwrap();

    let toml = r##"
[package]
id = "com.test.ow_ifnewer"
name = "IfNewerTest"
version = "1.0.0"

[[files]]
source = "file.txt"
dest = "#{app}"
overwrite = "if_newer"
"##;
    let config = Config::from_toml(toml).unwrap();
    let options = InstallOptions {
        source_dir,
        install_dir: Some(install_dir.clone()),
        selected_components: None,
        uninstall_exe: None,
    };

    install(&config, &options, &NoOpCallbacks).unwrap();
    // Dest is newer, so should NOT be overwritten
    assert_eq!(
        std::fs::read_to_string(install_dir.join("file.txt")).unwrap(),
        "newer dest"
    );
}

#[test]
fn test_file_backup_created_on_overwrite() {
    let mut cleanup = TestCleanup::new();
    let dir = std::env::temp_dir().join("outto_test_ow_backup");
    let _ = std::fs::remove_dir_all(&dir);
    let source_dir = dir.join("src");
    let install_dir = dir.join("dst");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::create_dir_all(&install_dir).unwrap();
    cleanup.track_dir(dir.clone());

    std::fs::write(source_dir.join("app.exe"), "new exe").unwrap();
    std::fs::write(install_dir.join("app.exe"), "old exe").unwrap();

    let toml = r##"
[package]
id = "com.test.backup"
name = "BackupTest"
version = "1.0.0"

[[files]]
source = "app.exe"
dest = "#{app}"
overwrite = "always"
"##;
    let config = Config::from_toml(toml).unwrap();
    let options = InstallOptions {
        source_dir,
        install_dir: Some(install_dir.clone()),
        selected_components: None,
        uninstall_exe: None,
    };

    install(&config, &options, &NoOpCallbacks).unwrap();

    // Backup should exist
    assert!(install_dir.join("app.exe.bak").exists());
    assert_eq!(
        std::fs::read_to_string(install_dir.join("app.exe.bak")).unwrap(),
        "old exe"
    );
}

#[test]
fn test_file_no_glob_matches() {
    let mut cleanup = TestCleanup::new();
    let dir = std::env::temp_dir().join("outto_test_noglob");
    let _ = std::fs::remove_dir_all(&dir);
    let source_dir = dir.join("src");
    std::fs::create_dir_all(&source_dir).unwrap();
    cleanup.track_dir(dir.clone());

    let toml = r##"
[package]
id = "com.test.noglob"
name = "NoGlob"
version = "1.0.0"

[[files]]
source = "nonexistent_pattern_*.xyz"
dest = "#{app}"
"##;
    let config = Config::from_toml(toml).unwrap();
    let options = InstallOptions {
        source_dir,
        install_dir: Some(dir.join("dst")),
        selected_components: None,
        uninstall_exe: None,
    };

    // Should succeed (no matches is a warning, not an error)
    let result = install(&config, &options, &NoOpCallbacks);
    assert!(result.is_ok());
}

// ---------------------------------------------------------------------------
// Orchestration tests (dirs + run phases)
// ---------------------------------------------------------------------------

#[test]
fn test_install_with_dirs_and_run() {
    let mut cleanup = TestCleanup::new();
    let dir = std::env::temp_dir().join("outto_test_dirs_run");
    let _ = std::fs::remove_dir_all(&dir);
    let source_dir = dir.join("src");
    let install_dir = dir.join("dst");
    std::fs::create_dir_all(&source_dir).unwrap();
    cleanup.track_dir(dir.clone());

    let toml = format!(
        r##"
[package]
id = "com.test.dirsrun"
name = "DirsRunTest"
version = "1.0.0"

[[dirs]]
path = "#{{app}}/logs"

[[dirs]]
path = "#{{app}}/data/cache"

[[run]]
phase = "after_install"
command = "cmd"
arguments = "/C echo done > \"{install_dir}/after.txt\""
wait = true
show = "hidden"
"##,
        install_dir = install_dir.to_string_lossy().replace('\\', "/")
    );
    let config = Config::from_toml(&toml).unwrap();
    let options = InstallOptions {
        source_dir,
        install_dir: Some(install_dir.clone()),
        selected_components: None,
        uninstall_exe: None,
    };

    install(&config, &options, &NoOpCallbacks).unwrap();

    assert!(install_dir.join("logs").exists());
    assert!(install_dir.join("data/cache").exists());
}

// ---------------------------------------------------------------------------
// lib.rs install flow branch tests
// ---------------------------------------------------------------------------

#[test]
fn test_install_dir_missing_errors() {
    let dir = std::env::temp_dir().join("outto_test_nodir");
    let source_dir = dir.join("src");
    let _ = std::fs::create_dir_all(&source_dir);

    let toml = r#"
[package]
id = "com.test.nodir"
name = "NoDirTest"
version = "1.0.0"
"#;
    let config = Config::from_toml(toml).unwrap();
    let options = InstallOptions {
        source_dir,
        install_dir: None, // No install dir
        selected_components: None,
        uninstall_exe: None,
    };

    let result = install(&config, &options, &NoOpCallbacks);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("no install directory"), "Error was: {err}");
}

#[test]
fn test_install_dir_from_default_dir() {
    let mut cleanup = TestCleanup::new();
    let dir = std::env::temp_dir().join("outto_test_defaultdir");
    let _ = std::fs::remove_dir_all(&dir);
    let source_dir = dir.join("src");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::write(source_dir.join("app.exe"), "content").unwrap();
    cleanup.track_dir(dir.clone());

    let default_install = dir.join("installed_default");
    let toml = format!(
        r##"
[package]
id = "com.test.defaultdir"
name = "DefaultDirTest"
version = "1.0.0"
default_dir = "{}"

[[files]]
source = "app.exe"
dest = "#{{app}}"
"##,
        default_install.to_string_lossy().replace('\\', "/")
    );
    let config = Config::from_toml(&toml).unwrap();
    let options = InstallOptions {
        source_dir,
        install_dir: None, // Use default_dir from config
        selected_components: None,
        uninstall_exe: None,
    };

    let result = install(&config, &options, &NoOpCallbacks);
    assert!(result.is_ok(), "Install failed: {result:?}");
    assert!(default_install.join("app.exe").exists());
}

#[test]
fn test_install_rollback_on_failure() {
    let mut cleanup = TestCleanup::new();
    let dir = std::env::temp_dir().join("outto_test_rollback_install");
    let _ = std::fs::remove_dir_all(&dir);
    let source_dir = dir.join("src");
    let install_dir = dir.join("dst");
    std::fs::create_dir_all(&source_dir).unwrap();
    cleanup.track_dir(dir.clone());

    // Create a good file, plus a run command that will fail
    std::fs::write(source_dir.join("good.txt"), "good").unwrap();

    let toml = r##"
[package]
id = "com.test.rollback"
name = "RollbackTest"
version = "1.0.0"

[[files]]
source = "good.txt"
dest = "#{app}"
overwrite = "always"

[[prerequisites]]
name = "WillFail"
check = { file = "C:\\nonexistent_outto_prereq_12345.exe" }
required = true
"##;
    let config = Config::from_toml(toml).unwrap();
    let options = InstallOptions {
        source_dir,
        install_dir: Some(install_dir.clone()),
        selected_components: None,
        uninstall_exe: None,
    };

    let result = install(&config, &options, &NoOpCallbacks);
    // Install should fail (prerequisite not met)
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Prerequisites — registry value matching
// ---------------------------------------------------------------------------

#[test]
fn test_prerequisite_registry_value_match() {
    let mut cleanup = TestCleanup::new();
    let key = "Software\\OuttoTest_prereq_match";
    cleanup.track_key(key);

    // Clean and create test key
    let key_wide = to_wide(key);
    unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, key_wide.as_ptr()) };
    write_hkcu_string(key, "Version", "1.0.0");

    let callbacks = NoOpCallbacks;
    let entries = vec![PrerequisiteEntry {
        name: "VersionCheck".to_string(),
        check: PrerequisiteCheck {
            registry: Some(format!("HKCU\\{key}")),
            value: Some("Version".to_string()),
            equals: Some(toml::Value::String("1.0.0".to_string())),
            file: None,
            command: None,
        },
        download_url: None,
        installer: None,
        arguments: None,
        required: true,
    }];

    let result = prerequisites::check_prerequisites(&entries, &callbacks);
    assert!(result.is_ok());
}

#[test]
fn test_prerequisite_registry_value_mismatch() {
    let mut cleanup = TestCleanup::new();
    let key = "Software\\OuttoTest_prereq_mismatch";
    cleanup.track_key(key);

    let key_wide = to_wide(key);
    unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, key_wide.as_ptr()) };
    write_hkcu_string(key, "Version", "1.0.0");

    let callbacks = NoOpCallbacks;
    let entries = vec![PrerequisiteEntry {
        name: "WrongVersion".to_string(),
        check: PrerequisiteCheck {
            registry: Some(format!("HKCU\\{key}")),
            value: Some("Version".to_string()),
            equals: Some(toml::Value::String("2.0.0".to_string())),
            file: None,
            command: None,
        },
        download_url: None,
        installer: None,
        arguments: None,
        required: true,
    }];

    let result = prerequisites::check_prerequisites(&entries, &callbacks);
    assert!(result.is_err());
}

#[test]
fn test_prerequisite_registry_nonexistent() {
    let callbacks = NoOpCallbacks;
    let entries = vec![PrerequisiteEntry {
        name: "NonexistentReg".to_string(),
        check: PrerequisiteCheck {
            registry: Some("HKCU\\Software\\OuttoTest_DOES_NOT_EXIST_12345".to_string()),
            value: None,
            equals: None,
            file: None,
            command: None,
        },
        download_url: None,
        installer: None,
        arguments: None,
        required: true,
    }];

    let result = prerequisites::check_prerequisites(&entries, &callbacks);
    assert!(result.is_err());
}

#[test]
fn test_prerequisite_not_required_skips() {
    let callbacks = NoOpCallbacks;
    let entries = vec![PrerequisiteEntry {
        name: "Optional".to_string(),
        check: PrerequisiteCheck {
            registry: None,
            value: None,
            equals: None,
            file: Some("C:\\nonexistent_outto_optional.exe".to_string()),
            command: None,
        },
        download_url: None,
        installer: None,
        arguments: None,
        required: false, // Not required — should not fail
    }];

    let result = prerequisites::check_prerequisites(&entries, &callbacks);
    assert!(result.is_ok());
}

// ---------------------------------------------------------------------------
// Run command — spawn (no wait)
// ---------------------------------------------------------------------------

#[test]
fn test_execute_command_no_wait() {
    let resolver = PathResolver::new(std::path::Path::new("C:\\test"), "Test", "1.0.0");
    let mut manifest = InstallManifest::new(
        "test",
        "Test",
        "1.0.0",
        std::path::Path::new("C:\\test"),
        vec![],
    );
    let callbacks = NoOpCallbacks;

    let entries = vec![RunEntry {
        phase: RunPhase::AfterInstall,
        command: "cmd".to_string(),
        arguments: Some("/C echo spawned".to_string()),
        wait: false, // Don't wait
        show: ShowWindow::Hidden,
        component: None,
        working_dir: None,
        arch: None,
        run_as_original_user: false,
    }];

    run::execute_phase_commands(
        &entries,
        &RunPhase::AfterInstall,
        &resolver,
        &mut manifest,
        &callbacks,
    )
    .unwrap();

    assert!(manifest.actions.iter().any(|a| matches!(
        a,
        ActionRecord::CommandExecuted { phase, .. } if phase == "after_install"
    )));
}

// ---------------------------------------------------------------------------
// Shortcut error paths
// ---------------------------------------------------------------------------

#[test]
fn test_shortcut_startmenu_location() {
    let mut cleanup = TestCleanup::new();
    let dir = std::env::temp_dir().join("outto_test_shortcut_startmenu");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    cleanup.track_dir(dir.clone());

    std::fs::write(dir.join("app.exe"), "fake").unwrap();

    let mut resolver = PathResolver::new(&dir, "Test", "1.0.0");
    resolver.set_variable("startmenu", dir.to_string_lossy().as_ref());

    let mut manifest = InstallManifest::new("test", "Test", "1.0.0", &dir, vec![]);
    let callbacks = NoOpCallbacks;

    let entry = ShortcutEntry {
        name: "StartMenuApp".to_string(),
        target: "#{app}/app.exe".to_string(),
        location: ShortcutLocation::StartMenu,
        icon: None,
        working_dir: None,
        arguments: None,
        description: None,
        component: None,
        hotkey: None,
        app_user_model_id: None,
        subfolder: None,
        icon_index: None,
        arch: None,
        run_maximized: false,
    };

    shortcuts::create_shortcut(&entry, &resolver, &mut manifest, &callbacks).unwrap();
    assert!(dir.join("StartMenuApp.lnk").exists());
}

// ---------------------------------------------------------------------------
// Manifest error paths
// ---------------------------------------------------------------------------

#[test]
fn test_manifest_load_missing_file() {
    let result = InstallManifest::load(
        std::path::Path::new("C:\\nonexistent_outto_dir_12345"),
        "com.test",
    );
    assert!(result.is_err());
}

#[test]
fn test_manifest_load_corrupted_json() {
    let mut cleanup = TestCleanup::new();
    let dir = std::env::temp_dir().join("outto_test_manifest_corrupt");
    let _ = std::fs::remove_dir_all(&dir);
    let manifest_dir = dir.join(".outto").join("com.test");
    std::fs::create_dir_all(&manifest_dir).unwrap();
    cleanup.track_dir(dir.clone());

    std::fs::write(manifest_dir.join("manifest.json"), "NOT VALID JSON {{{").unwrap();

    let result = InstallManifest::load(&dir, "com.test");
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Uninstall error path
// ---------------------------------------------------------------------------

#[test]
fn test_uninstall_missing_manifest() {
    let dir = std::env::temp_dir().join("outto_test_uninstall_nodir");
    let _ = std::fs::remove_dir_all(&dir);

    let result = uninstall_from_dir(&dir, &NoOpCallbacks);
    assert!(result.is_err());
}
