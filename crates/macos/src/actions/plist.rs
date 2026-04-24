//! Write and roll back plist value entries declared in `[[plist]]`.
//!
//! Uses the `plist` crate for actual plist serialisation. Key paths are dotted
//! (`"Window.Size.Width"`); nested dicts are created on demand during install
//! and collapsed during uninstall when they become empty.

use std::collections::BTreeMap;
use std::path::Path;

use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::VariableResolver;
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::InstallManifest;

use crate::config::{PlistEntry, PlistValue, PlistValueType};
use crate::manifest::{Action, PlistValueJson};

pub fn apply_plist_entry(
    entry: &PlistEntry,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let path_str = resolver.resolve(&entry.path)?;
    let path = std::path::PathBuf::from(path_str);

    callbacks.on_log(
        LogLevel::Info,
        &format!("Plist: writing {}", path.display()),
    );

    // Ensure parent dir exists.
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| InstallerError::DirOp {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
    }

    // Load existing root, or default to an empty dict if the file didn't exist.
    let (mut root, file_existed) = load_root(&path)?;
    if !file_existed {
        manifest.record(Action::PlistFileCreated { path: path.clone() });
    }

    for v in &entry.values {
        let resolved_data = resolve_plist_data(&v.data, resolver)?;
        let new_value = coerce_value(&v.value_type, &resolved_data)?;

        let previous = get_by_key_path(&root, &v.key)
            .map(value_to_json)
            .unwrap_or(PlistValueJson::Absent);

        set_by_key_path(&mut root, &v.key, new_value);

        manifest.record(Action::PlistValueSet {
            path: path.clone(),
            key_path: v.key.clone(),
            previous_value: previous,
        });
    }

    save_root(&path, &root)?;

    Ok(())
}

/// Undo a single PlistValueSet: restore the previous value, or delete the key
/// if there was none before our install.
pub fn rollback_value(
    path: &Path,
    key_path: &str,
    previous: &PlistValueJson,
) -> InstallerResult<()> {
    if !path.exists() {
        // File was already removed (probably by a PlistFileCreated rollback).
        return Ok(());
    }

    let mut root = plist::Value::from_file(path).map_err(|e| {
        InstallerError::Other(format!(
            "plist: failed to read {} for rollback: {e}",
            path.display()
        ))
    })?;

    match previous {
        PlistValueJson::Absent => {
            remove_by_key_path(&mut root, key_path);
        }
        other => {
            set_by_key_path(&mut root, key_path, json_to_value(other));
        }
    }

    save_root(path, &root)?;
    Ok(())
}

// --- Internal helpers ---

fn load_root(path: &Path) -> InstallerResult<(plist::Value, bool)> {
    if path.exists() {
        let v = plist::Value::from_file(path).map_err(|e| {
            InstallerError::Other(format!("plist: failed to read {}: {e}", path.display()))
        })?;
        Ok((v, true))
    } else {
        Ok((plist::Value::Dictionary(plist::Dictionary::new()), false))
    }
}

fn save_root(path: &Path, root: &plist::Value) -> InstallerResult<()> {
    root.to_file_xml(path).map_err(|e| {
        InstallerError::Other(format!("plist: failed to write {}: {e}", path.display()))
    })
}

fn resolve_plist_data(
    data: &toml::Value,
    resolver: &VariableResolver,
) -> InstallerResult<toml::Value> {
    // Only string values need resolving; everything else passes through.
    match data {
        toml::Value::String(s) => Ok(toml::Value::String(resolver.resolve(s)?)),
        other => Ok(other.clone()),
    }
}

fn coerce_value(ty: &PlistValueType, data: &toml::Value) -> InstallerResult<plist::Value> {
    match (ty, data) {
        (PlistValueType::String, toml::Value::String(s)) => Ok(plist::Value::String(s.clone())),
        (PlistValueType::Integer, toml::Value::Integer(i)) => {
            Ok(plist::Value::Integer((*i).into()))
        }
        (PlistValueType::Integer, toml::Value::String(s)) => {
            let v: i64 = s.parse().map_err(|_| {
                InstallerError::Validation(format!("plist: can't parse '{s}' as integer"))
            })?;
            Ok(plist::Value::Integer(v.into()))
        }
        (PlistValueType::Real, toml::Value::Float(f)) => Ok(plist::Value::Real(*f)),
        (PlistValueType::Bool, toml::Value::Boolean(b)) => Ok(plist::Value::Boolean(*b)),
        (PlistValueType::Data, toml::Value::String(s)) => {
            // Hex-encoded bytes
            let bytes = hex_to_bytes(s).map_err(|e| {
                InstallerError::Validation(format!("plist: invalid data (expected hex): {e}"))
            })?;
            Ok(plist::Value::Data(bytes))
        }
        (ty, v) => Err(InstallerError::Validation(format!(
            "plist: type {ty:?} doesn't match TOML value {v:?}"
        ))),
    }
}

fn hex_to_bytes(s: &str) -> Result<Vec<u8>, String> {
    let s = s.replace([' ', '-', ':'], "");
    if !s.len().is_multiple_of(2) {
        return Err("odd-length hex".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Walk a dotted key_path into a plist::Value tree and return the leaf (if present).
fn get_by_key_path<'a>(root: &'a plist::Value, key_path: &str) -> Option<&'a plist::Value> {
    let mut cur = root;
    for segment in key_path.split('.') {
        match cur {
            plist::Value::Dictionary(d) => {
                cur = d.get(segment)?;
            }
            _ => return None,
        }
    }
    Some(cur)
}

/// Walk a dotted key_path, creating intermediate dicts as needed, and set the leaf.
fn set_by_key_path(root: &mut plist::Value, key_path: &str, value: plist::Value) {
    let segments: Vec<&str> = key_path.split('.').collect();
    if segments.is_empty() {
        return;
    }

    // Coerce root to a dict if it isn't already.
    if !matches!(root, plist::Value::Dictionary(_)) {
        *root = plist::Value::Dictionary(plist::Dictionary::new());
    }

    let mut cur = root;
    for segment in &segments[..segments.len() - 1] {
        let dict = match cur {
            plist::Value::Dictionary(d) => d,
            _ => unreachable!("coerced above"),
        };
        if !matches!(dict.get(*segment), Some(plist::Value::Dictionary(_))) {
            dict.insert(
                (*segment).to_string(),
                plist::Value::Dictionary(plist::Dictionary::new()),
            );
        }
        cur = dict.get_mut(*segment).unwrap();
    }

    let last = segments[segments.len() - 1];
    if let plist::Value::Dictionary(d) = cur {
        d.insert(last.to_string(), value);
    }
}

/// Remove a leaf by dotted key_path. Leaves empty intermediate dicts in place
/// (conservative; most apps tolerate empty dicts better than unexpected restructuring).
fn remove_by_key_path(root: &mut plist::Value, key_path: &str) {
    let segments: Vec<&str> = key_path.split('.').collect();
    if segments.is_empty() {
        return;
    }

    let mut cur = root;
    for segment in &segments[..segments.len() - 1] {
        match cur {
            plist::Value::Dictionary(d) => {
                if let Some(v) = d.get_mut(*segment) {
                    cur = v;
                } else {
                    return;
                }
            }
            _ => return,
        }
    }

    if let plist::Value::Dictionary(d) = cur {
        d.remove(segments[segments.len() - 1]);
    }
}

fn value_to_json(v: &plist::Value) -> PlistValueJson {
    match v {
        plist::Value::String(s) => PlistValueJson::String(s.clone()),
        plist::Value::Integer(i) => PlistValueJson::Integer(i.as_signed().unwrap_or(0)),
        plist::Value::Real(f) => PlistValueJson::Real(*f),
        plist::Value::Boolean(b) => PlistValueJson::Bool(*b),
        plist::Value::Data(d) => PlistValueJson::Data(d.clone()),
        plist::Value::Array(a) => PlistValueJson::Array(a.iter().map(value_to_json).collect()),
        plist::Value::Dictionary(d) => {
            let mut m = BTreeMap::new();
            for (k, v) in d {
                m.insert(k.clone(), value_to_json(v));
            }
            PlistValueJson::Dict(m)
        }
        _ => PlistValueJson::Absent,
    }
}

fn json_to_value(v: &PlistValueJson) -> plist::Value {
    match v {
        PlistValueJson::String(s) => plist::Value::String(s.clone()),
        PlistValueJson::Integer(i) => plist::Value::Integer((*i).into()),
        PlistValueJson::Real(f) => plist::Value::Real(*f),
        PlistValueJson::Bool(b) => plist::Value::Boolean(*b),
        PlistValueJson::Data(d) => plist::Value::Data(d.clone()),
        PlistValueJson::Array(a) => plist::Value::Array(a.iter().map(json_to_value).collect()),
        PlistValueJson::Dict(d) => {
            let mut dict = plist::Dictionary::new();
            for (k, v) in d {
                dict.insert(k.clone(), json_to_value(v));
            }
            plist::Value::Dictionary(dict)
        }
        PlistValueJson::Absent => plist::Value::Dictionary(plist::Dictionary::new()),
    }
}

// Avoid re-importing PlistValue just for a doc link.
#[allow(dead_code)]
const _: () = {
    let _: Option<PlistValue> = None;
};

#[cfg(test)]
mod tests {
    use super::*;
    use outto_core::callbacks::NoOpCallbacks;

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("outto-plist-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn test_set_and_rollback_single_value() {
        let dir = tmp_dir("single");
        let plist_path = dir.join("test.plist");

        let mut resolver = VariableResolver::new().with_windows_paths(false);
        resolver.set_variable("base", dir.to_string_lossy());

        let entry = PlistEntry {
            path: "#{base}/test.plist".to_string(),
            uninstall: crate::config::PlistUninstall::RemoveKeys,
            component: None,
            values: vec![PlistValue {
                key: "AppVersion".to_string(),
                value_type: PlistValueType::String,
                data: toml::Value::String("1.0.0".to_string()),
            }],
        };

        let mut manifest = InstallManifest::<Action>::new("t", "T", "1.0.0", &dir, vec![]);
        apply_plist_entry(&entry, &resolver, &mut manifest, &NoOpCallbacks).unwrap();

        let loaded = plist::Value::from_file(&plist_path).unwrap();
        assert_eq!(
            get_by_key_path(&loaded, "AppVersion").and_then(|v| v.as_string()),
            Some("1.0.0")
        );

        // Rollback the value with Absent (as if the plist was newly created).
        rollback_value(&plist_path, "AppVersion", &PlistValueJson::Absent).unwrap();
        let reloaded = plist::Value::from_file(&plist_path).unwrap();
        assert!(get_by_key_path(&reloaded, "AppVersion").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_nested_key_path_creates_dicts() {
        let dir = tmp_dir("nested");
        let plist_path = dir.join("nested.plist");

        let mut resolver = VariableResolver::new().with_windows_paths(false);
        resolver.set_variable("base", dir.to_string_lossy());

        let entry = PlistEntry {
            path: "#{base}/nested.plist".to_string(),
            uninstall: crate::config::PlistUninstall::RemoveFile,
            component: None,
            values: vec![PlistValue {
                key: "Window.Size.Width".to_string(),
                value_type: PlistValueType::Integer,
                data: toml::Value::Integer(1024),
            }],
        };

        let mut manifest = InstallManifest::<Action>::new("t", "T", "1.0.0", &dir, vec![]);
        apply_plist_entry(&entry, &resolver, &mut manifest, &NoOpCallbacks).unwrap();

        let loaded = plist::Value::from_file(&plist_path).unwrap();
        assert_eq!(
            get_by_key_path(&loaded, "Window.Size.Width").and_then(|v| v.as_signed_integer()),
            Some(1024)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rollback_restores_previous_string_value() {
        let dir = tmp_dir("restore");
        let plist_path = dir.join("restore.plist");

        // Pre-seed an existing plist with AppVersion = "0.9.0"
        let mut seed = plist::Dictionary::new();
        seed.insert(
            "AppVersion".to_string(),
            plist::Value::String("0.9.0".to_string()),
        );
        plist::Value::Dictionary(seed)
            .to_file_xml(&plist_path)
            .unwrap();

        // Overwrite with a new install
        let mut resolver = VariableResolver::new().with_windows_paths(false);
        resolver.set_variable("base", dir.to_string_lossy());
        let entry = PlistEntry {
            path: "#{base}/restore.plist".to_string(),
            uninstall: crate::config::PlistUninstall::RemoveKeys,
            component: None,
            values: vec![PlistValue {
                key: "AppVersion".to_string(),
                value_type: PlistValueType::String,
                data: toml::Value::String("1.0.0".to_string()),
            }],
        };

        let mut manifest = InstallManifest::<Action>::new("t", "T", "1.0.0", &dir, vec![]);
        apply_plist_entry(&entry, &resolver, &mut manifest, &NoOpCallbacks).unwrap();

        // The install recorded a PlistValueSet with previous="0.9.0"
        let prev = match &manifest.actions[0] {
            Action::PlistValueSet { previous_value, .. } => previous_value.clone(),
            other => panic!("unexpected first action: {other:?}"),
        };
        assert!(matches!(prev, PlistValueJson::String(ref s) if s == "0.9.0"));

        // Rollback
        rollback_value(&plist_path, "AppVersion", &prev).unwrap();

        let restored = plist::Value::from_file(&plist_path).unwrap();
        assert_eq!(
            get_by_key_path(&restored, "AppVersion").and_then(|v| v.as_string()),
            Some("0.9.0")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_hex_to_bytes_plist() {
        assert_eq!(hex_to_bytes("ff00ab").unwrap(), vec![0xff, 0x00, 0xab]);
        assert_eq!(hex_to_bytes("FF 00 AB").unwrap(), vec![0xff, 0x00, 0xab]);
        assert!(hex_to_bytes("FFF").is_err());
    }
}
