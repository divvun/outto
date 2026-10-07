//! Decide which files a previous install recorded that the new one no longer
//! ships, so an upgrade can remove them.
//!
//! Manifest paths are not in any canonical form: older outto versions recorded
//! whatever the config's `dest` produced (`C:/Windows/SysWOW64\kbdvro.dll`), the
//! current one records `C:\WINDOWS\SysWOW64\kbdvro.dll`. Compared as raw
//! strings, an upgrade would delete a file it had just installed. Paths are
//! therefore compared by [`path_key`], and a path the new install wrote is never
//! reported as orphaned.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::config::VariableResolver;

/// Normalise `path` lexically for identity comparison.
///
/// With `windows`: `/` and `\` are both separators, `\\?\` and `\\?\UNC\`
/// prefixes are dropped, and the drive or UNC prefix is kept. On both styles
/// repeated separators and `.` components are removed, `..` is applied, a
/// trailing separator is dropped and the result is case-folded — Windows
/// paths are case-insensitive, and so is the default macOS volume; folding can
/// only make two paths compare equal, which errs towards keeping a file.
pub fn normalize_lexical(path: &str, windows: bool) -> String {
    let sep = if windows { '\\' } else { '/' };
    let mut s = if windows {
        path.replace('/', "\\")
    } else {
        path.to_string()
    };

    let mut prefix = String::new();
    if windows {
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            s = format!(r"\\{rest}");
        } else if let Some(rest) = s.strip_prefix(r"\\?\") {
            s = rest.to_string();
        }
        if let Some(rest) = s.strip_prefix(r"\\") {
            prefix.push_str(r"\\");
            s = rest.to_string();
        } else if s.len() >= 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic() {
            prefix.push_str(&s[..2]);
            s = s[2..].to_string();
        }
    }

    let absolute = prefix == r"\\" || s.starts_with(sep);
    let mut parts: Vec<&str> = Vec::new();
    for part in s.split(sep) {
        match part {
            "" | "." => {}
            ".." => match parts.last() {
                Some(&last) if last != ".." => {
                    parts.pop();
                }
                _ if absolute => {}
                _ => parts.push(".."),
            },
            _ => parts.push(part),
        }
    }

    let mut out = prefix;
    if absolute && !out.ends_with(sep) {
        out.push(sep);
    }
    out.push_str(&parts.join(&sep.to_string()));
    out.to_lowercase()
}

/// The identity of a recorded path: `#{...}` variables expanded with
/// `resolver` (if it can), the real path on disk if it exists (resolving
/// symlinks, junctions and 8.3 names), then [`normalize_lexical`].
pub fn path_key(path: &Path, resolver: Option<&VariableResolver>, windows: bool) -> String {
    let raw = path.to_string_lossy();
    let expanded = match resolver {
        Some(r) if raw.contains("#{") => r.resolve(&raw).unwrap_or_else(|_| raw.to_string()),
        _ => raw.to_string(),
    };
    let real = dunce::canonicalize(&expanded)
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    normalize_lexical(real.as_deref().unwrap_or(&expanded), windows)
}

/// The paths in `old` that are not, after [`path_key`], any path in
/// `written` — what the new install wrote, so never deleted.
pub fn orphaned_files<'a>(
    old: impl IntoIterator<Item = &'a Path>,
    written: impl IntoIterator<Item = &'a Path>,
    resolver: Option<&VariableResolver>,
    windows: bool,
) -> Vec<PathBuf> {
    let keep: HashSet<String> = written
        .into_iter()
        .map(|p| path_key(p, resolver, windows))
        .collect();
    let mut seen = HashSet::new();
    old.into_iter()
        .filter(|p| {
            let key = path_key(p, resolver, windows);
            !keep.contains(&key) && seen.insert(key)
        })
        .map(Path::to_path_buf)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(p: &str) -> String {
        normalize_lexical(p, true)
    }

    #[test]
    fn windows_separators_and_case() {
        let want = r"c:\windows\syswow64\kbdvro.dll";
        assert_eq!(win("C:/Windows/SysWOW64/kbdvro.dll"), want);
        assert_eq!(win(r"C:/Windows/SysWOW64\kbdvro.dll"), want);
        assert_eq!(win(r"C:\WINDOWS\SysWOW64\kbdvro.dll"), want);
        assert_eq!(win(r"c:\windows\syswow64\KBDVRO.DLL"), want);
    }

    #[test]
    fn windows_redundant_components() {
        let want = r"c:\windows\syswow64\kbdvro.dll";
        assert_eq!(win(r"C:\Windows\\SysWOW64\.\kbdvro.dll"), want);
        assert_eq!(win(r"C:\Windows\System32\..\SysWOW64\kbdvro.dll"), want);
        assert_eq!(win(r"C:\..\Windows\SysWOW64\kbdvro.dll"), want);
        assert_eq!(win(r"C:\Windows\SysWOW64\"), r"c:\windows\syswow64");
    }

    #[test]
    fn windows_prefixes() {
        assert_eq!(
            win(r"\\?\C:\Windows\SysWOW64\kbdvro.dll"),
            r"c:\windows\syswow64\kbdvro.dll"
        );
        assert_eq!(win(r"\\?\UNC\srv\share\a.txt"), r"\\srv\share\a.txt");
        assert_eq!(win("//srv/share/a.txt"), r"\\srv\share\a.txt");
        assert_ne!(win(r"C:\a.txt"), win(r"D:\a.txt"));
        assert_eq!(win("C:"), "c:");
        assert_eq!(win("C:/"), r"c:\");
    }

    #[test]
    fn windows_relative() {
        assert_eq!(win(r"a\..\..\b"), r"..\b");
        assert_eq!(win(r".\a\b\"), r"a\b");
    }

    #[test]
    fn posix_paths() {
        assert_eq!(
            normalize_lexical("/usr//local/./bin/", false),
            "/usr/local/bin"
        );
        assert_eq!(normalize_lexical("/usr/local/../bin", false), "/usr/bin");
        // A backslash is an ordinary character on POSIX.
        assert_eq!(normalize_lexical(r"/a\b", false), r"/a\b");
    }

    fn resolver() -> VariableResolver {
        let mut r = VariableResolver::new().with_windows_paths(true);
        r.set_variable("win", r"C:\WINDOWS");
        r
    }

    /// The upgrade that deleted kbdvro.dll: the old manifest recorded the
    /// destination from a literal `C:/Windows/SysWOW64`, the new install
    /// resolved `#{win}/SysWOW64`.
    #[test]
    fn syswow64_upgrade_is_not_an_orphan() {
        let new = [Path::new(r"C:\WINDOWS\SysWOW64\kbdvro.dll")];
        for old in [
            "C:/Windows/SysWOW64/kbdvro.dll",
            r"C:/Windows/SysWOW64\kbdvro.dll",
            r"C:\Windows\SysWOW64\kbdvro.dll",
            "#{win}/SysWOW64/kbdvro.dll",
        ] {
            let orphans = orphaned_files([Path::new(old)], new, Some(&resolver()), true);
            assert!(orphans.is_empty(), "{old} treated as orphaned: {orphans:?}");
        }
    }

    #[test]
    fn new_dest_recorded_unresolved_is_matched() {
        let orphans = orphaned_files(
            [Path::new(r"C:\WINDOWS\SysWOW64\kbdvro.dll")],
            [Path::new("#{win}/SysWOW64/kbdvro.dll")],
            Some(&resolver()),
            true,
        );
        assert!(orphans.is_empty(), "{orphans:?}");
    }

    #[test]
    fn real_orphans_are_reported_once() {
        let old = [
            Path::new("C:/Windows/SysWOW64/kbdvro.dll"),
            Path::new("C:/Windows/SysWOW64/kbdold.dll"),
            Path::new(r"C:\WINDOWS\SysWOW64\KBDOLD.DLL"),
            Path::new(r"C:\Windows\System32\kbdvro.dll"),
        ];
        let new = [Path::new(r"C:\WINDOWS\SysWOW64\kbdvro.dll")];
        let orphans = orphaned_files(old, new, Some(&resolver()), true);
        assert_eq!(
            orphans,
            vec![
                PathBuf::from("C:/Windows/SysWOW64/kbdold.dll"),
                PathBuf::from(r"C:\Windows\System32\kbdvro.dll"),
            ]
        );
    }

    #[test]
    fn unknown_variable_is_compared_verbatim() {
        let orphans = orphaned_files(
            [Path::new("#{nope}/a.dll")],
            [Path::new("#{nope}/A.DLL")],
            Some(&resolver()),
            true,
        );
        assert!(orphans.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn existing_paths_compare_by_real_path() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("f.txt"), "x").unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let old = link.join("f.txt");
        let new = real.join("f.txt");
        let orphans = orphaned_files([old.as_path()], [new.as_path()], None, false);
        assert!(orphans.is_empty(), "{orphans:?}");
    }
}
