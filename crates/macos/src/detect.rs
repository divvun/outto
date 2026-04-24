//! Enumerate outto-managed packages via the receipt filesystem layout.
//!
//! Receipts live at:
//! - `~/Library/no.divvun.install/packages/<pkg-id>/` for user-scope installs
//! - `/Library/no.divvun.install/packages/<pkg-id>/` for system-scope installs
//!
//! Each directory carries `manifest.json`, `receipt.json`, and (optionally)
//! `uninstall.app/`. The `receipt.json` is our "Add/Remove Programs" analog —
//! compact metadata sufficient to render uninstall UI or detect existing
//! installs during upgrades.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use outto_core::error::{InstallerError, InstallerResult};

/// User-scope receipt root under `$HOME`.
pub const USER_RECEIPT_BASE: &str = "Library/no.divvun.install/packages";

/// System-scope receipt root (absolute).
pub const SYSTEM_RECEIPT_BASE: &str = "/Library/no.divvun.install/packages";

/// Metadata sufficient to render an "installed packages" list without loading
/// the full manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub package_id: String,
    pub display_name: String,
    pub version: String,
    pub install_dir: PathBuf,
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// "user" | "system".
    pub scope: String,
}

/// Return the user-scope receipt base (`$HOME/Library/no.divvun.install/packages`).
pub fn user_receipt_base() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(USER_RECEIPT_BASE))
}

/// Return the system-scope receipt base.
pub fn system_receipt_base() -> PathBuf {
    PathBuf::from(SYSTEM_RECEIPT_BASE)
}

/// Matches the `ExistingInstall` type the Windows backend exposes so host code
/// can treat both the same way in upgrade-detection logic.
#[derive(Debug, Clone)]
pub struct ExistingInstall {
    pub install_dir: PathBuf,
    pub version: Option<String>,
    pub display_name: Option<String>,
    /// "user" | "system".
    pub scope: String,
}

/// Look up an existing install by package id. Checks user scope first, then system.
pub fn detect_existing_install(package_id: &str) -> InstallerResult<Option<ExistingInstall>> {
    if let Some(base) = user_receipt_base() {
        if let Some(r) = read_receipt(&base.join(package_id))? {
            return Ok(Some(ExistingInstall {
                install_dir: r.install_dir,
                version: Some(r.version),
                display_name: Some(r.display_name),
                scope: r.scope,
            }));
        }
    }
    let sys = system_receipt_base().join(package_id);
    if let Some(r) = read_receipt(&sys)? {
        return Ok(Some(ExistingInstall {
            install_dir: r.install_dir,
            version: Some(r.version),
            display_name: Some(r.display_name),
            scope: r.scope,
        }));
    }
    Ok(None)
}

/// Information about an installed outto package, used for dependency cascades.
#[derive(Debug, Clone)]
pub struct InstalledPackageInfo {
    pub package_id: String,
    pub install_dir: PathBuf,
    pub depends_on: Vec<String>,
    pub scope: String,
}

/// Return every outto-managed package receipt found on disk (user + system),
/// de-duplicated on package id (user-scope takes precedence).
pub fn enumerate_outto_packages() -> Vec<InstalledPackageInfo> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();

    if let Some(base) = user_receipt_base() {
        collect_from(&base, "user", &mut out, &mut seen);
    }
    collect_from(&system_receipt_base(), "system", &mut out, &mut seen);

    out
}

fn collect_from(
    base: &Path,
    scope: &str,
    out: &mut Vec<InstalledPackageInfo>,
    seen: &mut std::collections::HashSet<String>,
) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for e in entries.flatten() {
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Some(name) = e.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Ok(Some(r)) = read_receipt(&e.path()) {
            out.push(InstalledPackageInfo {
                package_id: r.package_id,
                install_dir: r.install_dir,
                depends_on: r.depends_on,
                scope: scope.to_string(),
            });
        }
    }
}

fn read_receipt(dir: &Path) -> InstallerResult<Option<Receipt>> {
    let receipt_path = dir.join("receipt.json");
    if !receipt_path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(&receipt_path).map_err(|e| InstallerError::FileOp {
        path: receipt_path.clone(),
        source: e,
    })?;
    let r: Receipt = serde_json::from_str(&data)?;
    Ok(Some(r))
}

/// Write or overwrite the receipt file for a package.
pub fn write_receipt(base: &Path, receipt: &Receipt) -> InstallerResult<()> {
    let dir = base.join(&receipt.package_id);
    std::fs::create_dir_all(&dir).map_err(|e| InstallerError::DirOp {
        path: dir.clone(),
        source: e,
    })?;
    let path = dir.join("receipt.json");
    let json = serde_json::to_string_pretty(receipt)?;
    std::fs::write(&path, json).map_err(|e| InstallerError::FileOp { path, source: e })?;
    Ok(())
}

/// Remove the entire receipt directory for a package (`~/.../packages/<pkg-id>/`).
pub fn remove_receipt(base: &Path, package_id: &str) -> InstallerResult<()> {
    let dir = base.join(package_id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| InstallerError::DirOp {
            path: dir.clone(),
            source: e,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("outto-detect-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn test_roundtrip_receipt() {
        let base = tmp("roundtrip");
        let r = Receipt {
            package_id: "no.divvun.test".to_string(),
            display_name: "Test".to_string(),
            version: "1.0.0".to_string(),
            install_dir: PathBuf::from("/Applications/Test.app"),
            depends_on: vec!["no.divvun.runtime".to_string()],
            scope: "user".to_string(),
        };
        write_receipt(&base, &r).unwrap();

        let loaded = read_receipt(&base.join(&r.package_id)).unwrap().unwrap();
        assert_eq!(loaded.package_id, "no.divvun.test");
        assert_eq!(loaded.depends_on, vec!["no.divvun.runtime"]);

        remove_receipt(&base, &r.package_id).unwrap();
        assert!(!base.join(&r.package_id).exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_enumerate_skips_non_receipt_dirs() {
        let base = tmp("enum");
        // A dir without a receipt.json — should be skipped silently.
        std::fs::create_dir_all(base.join("bogus")).unwrap();
        // A real receipt.
        let r = Receipt {
            package_id: "no.divvun.real".to_string(),
            display_name: "Real".to_string(),
            version: "1.0.0".to_string(),
            install_dir: PathBuf::from("/Applications/Real.app"),
            depends_on: vec![],
            scope: "user".to_string(),
        };
        write_receipt(&base, &r).unwrap();

        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        collect_from(&base, "user", &mut out, &mut seen);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].package_id, "no.divvun.real");

        let _ = std::fs::remove_dir_all(&base);
    }
}
