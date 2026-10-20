//! Restart-time file operations that would undo what an install wrote.
//!
//! `MoveFileEx` with `MOVEFILE_DELAY_UNTIL_REBOOT` queues a deletion or a
//! rename for the Session Manager to carry out early in the next boot. The
//! queue is the `REG_MULTI_SZ` values `PendingFileRenameOperations` and
//! `PendingFileRenameOperations2` under
//! `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager`: pairs of a source
//! and a destination, the destination empty for a deletion. It names paths,
//! not files, so whatever is at the path at the restart is what goes. An
//! uninstaller that cannot delete a DLL in use queues its deletion (Inno
//! Setup does with `uninsrestartdelete`), the install that replaces it writes
//! the new DLL to the same path, and the restart deletes the new one.
//!
//! So once an install has succeeded, every queued operation that would delete
//! a file it wrote, move one away, or move another file onto one is cancelled,
//! and everything else stays as it was, in order. The replacements the install
//! queued itself, for files that were in use, stay too. Outto's own removal
//! of a file in use moves the file aside and queues the moved copy, which no
//! install writes, so that stays; only when it cannot move the file does it
//! queue the file's own path, and an install writing that path cancels it.
//!
//! This happens once, when the install can no longer be rolled back, rather
//! than as each file is written. Each value is then read once and rewritten
//! at most once, and a failed install never touches the queue: its rollback
//! restores the files it replaced together with whatever was queued for
//! them, so there is nothing for it to restore. After a successful install,
//! what a cancelled operation was queued for is gone, replaced by the
//! install's file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use outto_core::actions::files::staged_path;
use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::manifest::orphans::{normalize_lexical, path_key};

const SESSION_MANAGER: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager";
const VALUES: [&str; 2] = [
    "PendingFileRenameOperations",
    "PendingFileRenameOperations2",
];

/// One queued operation, as stored: `from` is renamed to `to` at the next
/// restart, or deleted when `to` is empty. Both keep their flag prefixes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operation {
    pub from: Vec<u16>,
    pub to: Vec<u16>,
}

impl Operation {
    pub fn new(from: &str, to: &str) -> Self {
        Self {
            from: from.encode_utf16().collect(),
            to: to.encode_utf16().collect(),
        }
    }

    fn source_path(&self) -> Option<String> {
        entry_path(&String::from_utf16_lossy(&self.from))
    }

    fn target_path(&self) -> Option<String> {
        entry_path(&String::from_utf16_lossy(&self.to))
    }
}

/// Split a queue value's strings into operations. `None` if it is not a
/// list of pairs, in which case it is best left alone.
pub fn parse(data: &[u16]) -> Option<Vec<Operation>> {
    let mut strings: Vec<&[u16]> = data.split(|&c| c == 0).collect();
    // What follows the last NUL: nothing, unless the data is unterminated.
    if strings.last().is_some_and(|s| s.is_empty()) {
        strings.pop();
    }
    let mut strings = strings.into_iter();
    let mut ops = Vec::new();
    while let Some(from) = strings.next() {
        if from.is_empty() {
            // The empty string that ends the list; only padding may follow.
            return strings.all(|s| s.is_empty()).then_some(ops);
        }
        let to = strings.next()?;
        ops.push(Operation {
            from: from.to_vec(),
            to: to.to_vec(),
        });
    }
    Some(ops)
}

/// The value data for `ops`, as `MoveFileEx` writes it.
pub fn encode(ops: &[Operation]) -> Vec<u16> {
    let mut data = Vec::new();
    for op in ops {
        data.extend_from_slice(&op.from);
        data.push(0);
        data.extend_from_slice(&op.to);
        data.push(0);
    }
    data.push(0);
    data
}

/// The Win32 path that a queued source or destination names, without its
/// flag prefix (`*1`, and `!` for replace-existing, as in `*1!\??\C:\x`) and
/// the NT `\??\` prefix. `None` for an empty destination: a deletion.
pub fn entry_path(name: &str) -> Option<String> {
    let name =
        name.trim_start_matches(|c: char| matches!(c, '*' | '!' | '@') || c.is_ascii_digit());
    if name.is_empty() {
        return None;
    }
    let strip = |prefix: &str| {
        name.get(..prefix.len())
            .filter(|p| p.eq_ignore_ascii_case(prefix))
            .map(|_| &name[prefix.len()..])
    };
    Some(if let Some(rest) = strip(r"\??\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = strip(r"\??\")
        .or_else(|| strip(r"\GLOBAL??\"))
        .or_else(|| strip(r"\DosDevices\"))
    {
        rest.to_string()
    } else {
        name.to_string()
    })
}

/// What a queued operation would do to a file the install wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clash {
    /// Delete it.
    Deletes,
    /// Move it elsewhere.
    MovesAway,
    /// Move another file onto it.
    Replaces,
}

/// A queued operation that was cancelled, and the written file it was about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cancelled {
    pub op: Operation,
    pub clash: Clash,
    /// The file the install wrote.
    pub path: String,
    /// The other path of a rename.
    pub other: Option<String>,
}

impl Cancelled {
    /// Whether this moved outto's own staged copy onto the file, queued by
    /// an earlier install that found the file in use. The install that has
    /// now written the file directly leaves that copy behind otherwise.
    pub fn is_stale_staged_copy(&self) -> bool {
        self.clash == Clash::Replaces
            && self.other.as_deref().is_some_and(|from| {
                normalize_lexical(from, true)
                    == normalize_lexical(
                        &staged_path(Path::new(&self.path)).to_string_lossy(),
                        true,
                    )
            })
    }
}

/// The files an install wrote, to match queued operations against, and the
/// copies it staged to replace those of them that were in use.
pub struct InstalledFiles {
    keys: HashSet<String>,
    names: HashSet<String>,
    staged: HashSet<String>,
}

impl InstalledFiles {
    pub fn new<'a>(
        files: impl IntoIterator<Item = &'a Path>,
        replaced_at_restart: &[(PathBuf, PathBuf)],
    ) -> Self {
        let mut keys = HashSet::new();
        let mut names = HashSet::new();
        for file in files {
            keys.insert(normalize_lexical(&file.to_string_lossy(), true));
            keys.insert(path_key(file, None, true));
            if let Some(name) = file.file_name() {
                names.insert(name.to_string_lossy().to_lowercase());
            }
        }
        let staged = replaced_at_restart
            .iter()
            .flat_map(|(staged, _)| {
                [
                    normalize_lexical(&staged.to_string_lossy(), true),
                    path_key(staged, None, true),
                ]
            })
            .collect();
        Self {
            keys,
            names,
            staged,
        }
    }

    /// Whether `path` names one of the files. Paths are compared by
    /// [`normalize_lexical`] first; one that might be an alias for a file
    /// (an 8.3 name, a path through a junction) is then resolved on disk.
    /// The queue can be long, so only one with a file's name or a `~` is.
    pub fn contains(&self, path: &str) -> bool {
        if self.keys.contains(&normalize_lexical(path, true)) {
            return true;
        }
        let name = path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or_default()
            .to_lowercase();
        (self.names.contains(&name) || path.contains('~'))
            && self.keys.contains(&path_key(Path::new(path), None, true))
    }

    /// Whether `path` is a copy the install staged, which only the
    /// replacement it queued moves. An earlier install that found the same
    /// file in use staged its copy at the same path and queued the same
    /// replacement; whichever of the two runs first moves the new copy.
    fn is_staged(&self, path: &str) -> bool {
        self.staged.contains(&normalize_lexical(path, true))
            || self.staged.contains(&path_key(Path::new(path), None, true))
    }

    /// What `op` would do to one of the files, if anything.
    pub fn clash(&self, op: &Operation) -> Option<(Clash, String, Option<String>)> {
        let from = op.source_path()?;
        let to = op.target_path();
        if self.contains(&from) {
            let clash = if to.is_some() {
                Clash::MovesAway
            } else {
                Clash::Deletes
            };
            return Some((clash, from, to));
        }
        let to = to?;
        (self.contains(&to) && !self.is_staged(&from)).then_some((Clash::Replaces, to, Some(from)))
    }

    /// Split `ops` into those to keep, in their order, and those to cancel.
    pub fn filter(&self, ops: Vec<Operation>) -> (Vec<Operation>, Vec<Cancelled>) {
        let mut kept = Vec::new();
        let mut cancelled = Vec::new();
        for op in ops {
            match self.clash(&op) {
                Some((clash, path, other)) => cancelled.push(Cancelled {
                    op,
                    clash,
                    path,
                    other,
                }),
                None => kept.push(op),
            }
        }
        (kept, cancelled)
    }
}

/// Cancel every operation queued for the next restart that would delete,
/// move away or replace one of `files`, which the install wrote; the
/// replacements in `replaced_at_restart` are the install's own and stay.
/// Each cancellation is logged. Failing to read or rewrite the queue (an
/// install that is not elevated cannot) is logged, not an error: the files
/// are installed either way.
pub fn cancel_clashing(
    files: &[&Path],
    replaced_at_restart: &[(PathBuf, PathBuf)],
    callbacks: &dyn InstallerCallbacks,
) {
    let mut queued = Vec::new();
    for value in VALUES {
        match registry::read(value) {
            Ok(Some(data)) => queued.push((value, data)),
            Ok(None) => {}
            Err(e) => callbacks.on_log(
                LogLevel::Warn,
                &format!("Could not read {value} to check what the next restart does: {e}"),
            ),
        }
    }
    if queued.is_empty() {
        return;
    }

    let installed = InstalledFiles::new(files.iter().copied(), replaced_at_restart);
    for (value, data) in queued {
        let Some(ops) = parse(&data.data) else {
            callbacks.on_log(
                LogLevel::Warn,
                &format!("{value} is not a list of renames; leaving it alone"),
            );
            continue;
        };
        if installed.filter(ops).1.is_empty() {
            continue;
        }
        match registry::rewrite(value, |ops| installed.filter(ops)) {
            Ok(cancelled) => {
                for c in &cancelled {
                    log_cancelled(value, c, callbacks);
                    if c.is_stale_staged_copy() {
                        remove_stale_staged_copy(c, callbacks);
                    }
                }
            }
            Err(e) => callbacks.on_log(
                LogLevel::Warn,
                &format!(
                    "Could not cancel what the next restart does to files this install wrote \
                     ({value}): {e}"
                ),
            ),
        }
    }
}

fn log_cancelled(value: &str, c: &Cancelled, callbacks: &dyn InstallerCallbacks) {
    let what = match (c.clash, c.other.as_deref()) {
        (Clash::Deletes, _) => format!("delete {}", c.path),
        (Clash::MovesAway, Some(to)) => format!("move {} to {to}", c.path),
        (Clash::Replaces, Some(from)) => format!("move {from} onto {}", c.path),
        (_, None) => format!("change {}", c.path),
    };
    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "Cancelled what {value} queued for the next restart, which would {what} \
             after this install wrote it: \"{}\" -> \"{}\"",
            String::from_utf16_lossy(&c.op.from),
            String::from_utf16_lossy(&c.op.to)
        ),
    );
}

fn remove_stale_staged_copy(c: &Cancelled, callbacks: &dyn InstallerCallbacks) {
    let Some(staged) = c.other.as_deref().map(Path::new) else {
        return;
    };
    match std::fs::remove_file(staged) {
        Ok(()) => callbacks.on_log(
            LogLevel::Info,
            &format!("Removed {}, staged by an earlier install", staged.display()),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => callbacks.on_log(
            LogLevel::Warn,
            &format!("Could not remove {}: {e}", staged.display()),
        ),
    }
}

/// The Session Manager key's queue values.
pub mod registry {
    use super::*;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA};
    use windows_sys::Win32::System::Registry::*;

    /// A value's type and data, as UTF-16 code units.
    pub struct Data {
        pub kind: REG_VALUE_TYPE,
        pub data: Vec<u16>,
    }

    struct Key(HKEY);

    impl Key {
        fn open(write: bool) -> std::io::Result<Option<Self>> {
            let name = wide(SESSION_MANAGER);
            let access = if write {
                KEY_QUERY_VALUE | KEY_SET_VALUE
            } else {
                KEY_QUERY_VALUE
            };
            let mut hkey: HKEY = std::ptr::null_mut();
            match unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, name.as_ptr(), 0, access, &mut hkey) }
            {
                0 => Ok(Some(Self(hkey))),
                ERROR_FILE_NOT_FOUND => Ok(None),
                e => Err(std::io::Error::from_raw_os_error(e as i32)),
            }
        }

        fn query(&self, value: &str) -> std::io::Result<Option<Data>> {
            let name = wide(value);
            let mut size = 0u32;
            loop {
                let mut kind: REG_VALUE_TYPE = 0;
                let mut buf = vec![0u8; size as usize];
                let r = unsafe {
                    RegQueryValueExW(
                        self.0,
                        name.as_ptr(),
                        std::ptr::null(),
                        &mut kind,
                        if buf.is_empty() {
                            std::ptr::null_mut()
                        } else {
                            buf.as_mut_ptr()
                        },
                        &mut size,
                    )
                };
                match r {
                    // With no buffer, success only reports the size.
                    0 if buf.is_empty() && size > 0 => continue,
                    0 => {
                        buf.truncate(size as usize);
                        let (units, []) = buf.as_chunks::<2>() else {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("{value} has an odd number of bytes"),
                            ));
                        };
                        let data = units.iter().map(|&u| u16::from_le_bytes(u)).collect();
                        return Ok(Some(Data { kind, data }));
                    }
                    ERROR_MORE_DATA => continue,
                    ERROR_FILE_NOT_FOUND => return Ok(None),
                    e => return Err(std::io::Error::from_raw_os_error(e as i32)),
                }
            }
        }

        fn set(&self, value: &str, kind: REG_VALUE_TYPE, data: &[u16]) -> std::io::Result<()> {
            let name = wide(value);
            let bytes: Vec<u8> = data.iter().flat_map(|c| c.to_le_bytes()).collect();
            match unsafe {
                RegSetValueExW(
                    self.0,
                    name.as_ptr(),
                    0,
                    kind,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                )
            } {
                0 => Ok(()),
                e => Err(std::io::Error::from_raw_os_error(e as i32)),
            }
        }

        fn delete(&self, value: &str) -> std::io::Result<()> {
            let name = wide(value);
            match unsafe { RegDeleteValueW(self.0, name.as_ptr()) } {
                0 | ERROR_FILE_NOT_FOUND => Ok(()),
                e => Err(std::io::Error::from_raw_os_error(e as i32)),
            }
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            unsafe { RegCloseKey(self.0) };
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The data of the queue value `value`, if it is set.
    pub fn read(value: &str) -> std::io::Result<Option<Data>> {
        match Key::open(false)? {
            Some(key) => key.query(value),
            None => Ok(None),
        }
    }

    /// Replace the operations queued in `value` by those `filter` keeps,
    /// returning what it took out. The value is read and written through
    /// one handle, with nothing in between, so that an operation queued
    /// meanwhile is not lost; it keeps its type, and is deleted when nothing
    /// is left in it.
    pub fn rewrite<T>(
        value: &str,
        filter: impl FnOnce(Vec<Operation>) -> (Vec<Operation>, Vec<T>),
    ) -> std::io::Result<Vec<T>> {
        let Some(key) = Key::open(true)? else {
            return Ok(Vec::new());
        };
        let Some(current) = key.query(value)? else {
            return Ok(Vec::new());
        };
        let ops = parse(&current.data).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{value} is not a list of renames"),
            )
        })?;
        let (kept, removed) = filter(ops);
        if removed.is_empty() {
            return Ok(removed);
        }
        if kept.is_empty() {
            key.delete(value)?;
        } else {
            key.set(value, current.kind, &encode(&kept))?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn multi_sz(strings: &[&str]) -> Vec<u16> {
        let mut data = Vec::new();
        for s in strings {
            data.extend(s.encode_utf16());
            data.push(0);
        }
        data.push(0);
        data
    }

    fn installed(files: &[&str]) -> InstalledFiles {
        InstalledFiles::new(files.iter().map(Path::new), &[])
    }

    fn kept_strings(ops: &[Operation]) -> Vec<(String, String)> {
        ops.iter()
            .map(|op| {
                (
                    String::from_utf16_lossy(&op.from),
                    String::from_utf16_lossy(&op.to),
                )
            })
            .collect()
    }

    #[test]
    fn parses_pairs_with_empty_destinations() {
        let data = multi_sz(&[
            r"*1\??\C:\WINDOWS\system32\kbdvro.dll",
            "",
            r"*1\??\C:\a.txt",
            r"*1!\??\C:\b.txt",
            r"\??\C:\c.txt",
            "",
        ]);
        let ops = parse(&data).unwrap();
        assert_eq!(
            ops,
            vec![
                Operation::new(r"*1\??\C:\WINDOWS\system32\kbdvro.dll", ""),
                Operation::new(r"*1\??\C:\a.txt", r"*1!\??\C:\b.txt"),
                Operation::new(r"\??\C:\c.txt", ""),
            ]
        );
        assert_eq!(encode(&ops), data);
    }

    #[test]
    fn parses_the_ends_of_lists() {
        assert_eq!(parse(&[]), Some(vec![]));
        assert_eq!(parse(&[0]), Some(vec![]));
        assert_eq!(parse(&[0, 0, 0]), Some(vec![]));
        // No list terminator after a deletion, or after a rename.
        let a: Vec<u16> = "a\0\0".encode_utf16().collect();
        assert_eq!(parse(&a), Some(vec![Operation::new("a", "")]));
        let ab: Vec<u16> = "a\0b".encode_utf16().collect();
        assert_eq!(parse(&ab), Some(vec![Operation::new("a", "b")]));
        // Padding after the terminator.
        let padded: Vec<u16> = "a\0b\0\0\0\0".encode_utf16().collect();
        assert_eq!(parse(&padded), Some(vec![Operation::new("a", "b")]));
    }

    #[test]
    fn rejects_what_is_not_a_list_of_pairs() {
        let unpaired: Vec<u16> = "a\0b\0c\0".encode_utf16().collect();
        assert_eq!(parse(&unpaired), None);
        let after_end: Vec<u16> = "a\0\0\0b\0\0\0".encode_utf16().collect();
        assert_eq!(parse(&after_end), None);
    }

    #[test]
    fn entry_paths() {
        let c = |s: &str| entry_path(s);
        assert_eq!(
            c(r"*1\??\C:\Windows\Temp\x.deleted").as_deref(),
            Some(r"C:\Windows\Temp\x.deleted")
        );
        assert_eq!(c(r"*1!\??\C:\x\c.txt").as_deref(), Some(r"C:\x\c.txt"));
        assert_eq!(c(r"!\??\C:\x").as_deref(), Some(r"C:\x"));
        assert_eq!(c(r"\??\c:\x").as_deref(), Some(r"c:\x"));
        assert_eq!(c(r"\??\UNC\srv\share\a").as_deref(), Some(r"\\srv\share\a"));
        assert_eq!(c(r"\DosDevices\C:\x").as_deref(), Some(r"C:\x"));
        assert_eq!(c(r"C:\x").as_deref(), Some(r"C:\x"));
        assert_eq!(c(""), None);
        assert_eq!(c("*1"), None);
    }

    /// The keyboard upgrade: the Inno Setup uninstaller could not delete the
    /// layout DLL in use and queued its deletion; the install then wrote the
    /// new DLL to the same path.
    #[test]
    fn cancels_the_deletion_of_a_dll_the_install_wrote() {
        let files = installed(&[
            r"C:\Windows\System32\kbdvro.dll",
            r"C:\Windows\SysWOW64\kbdvro.dll",
        ]);
        let ops = parse(&multi_sz(&[
            r"\??\C:\WINDOWS\system32\kbdvro.dll",
            "",
            r"*1\??\C:\Windows\Temp\outto-1-2-0-a.dll.deleted",
            "",
            r"*1\??\C:\WINDOWS\SysWOW64\KBDVRO.DLL",
            "",
            r"*1\??\C:\WINDOWS\system32\kbdsmi.dll",
            "",
        ]))
        .unwrap();
        let (kept, cancelled) = files.filter(ops);
        assert_eq!(
            kept_strings(&kept),
            vec![
                (
                    r"*1\??\C:\Windows\Temp\outto-1-2-0-a.dll.deleted".into(),
                    String::new()
                ),
                (
                    r"*1\??\C:\WINDOWS\system32\kbdsmi.dll".into(),
                    String::new()
                ),
            ]
        );
        assert_eq!(cancelled.len(), 2);
        assert!(cancelled.iter().all(|c| c.clash == Clash::Deletes));
        assert_eq!(cancelled[0].path, r"C:\WINDOWS\system32\kbdvro.dll");
        assert_eq!(cancelled[1].path, r"C:\WINDOWS\SysWOW64\KBDVRO.DLL");
    }

    #[test]
    fn cancels_renames_from_and_onto_written_files() {
        let files = installed(&[r"C:\App\a.dll", r"C:\App\b.dll"]);
        let ops = vec![
            Operation::new(r"*1\??\C:\App\a.dll", r"*1\??\C:\Elsewhere\a.dll"),
            Operation::new(r"*1\??\C:\Temp\x.tmp", r"*1!\??\C:\APP\B.DLL"),
            Operation::new(r"\??\C:\Temp\y.tmp", r"\??\C:/App/./sub/../b.dll"),
            Operation::new(r"*1\??\C:\Temp\z.tmp", r"*1!\??\C:\App\c.dll"),
        ];
        let (kept, cancelled) = files.filter(ops);
        assert_eq!(
            kept_strings(&kept),
            vec![(r"*1\??\C:\Temp\z.tmp".into(), r"*1!\??\C:\App\c.dll".into())]
        );
        let clashes: Vec<_> = cancelled
            .iter()
            .map(|c| (c.clash, c.path.as_str(), c.other.as_deref()))
            .collect();
        assert_eq!(
            clashes,
            vec![
                (
                    Clash::MovesAway,
                    r"C:\App\a.dll",
                    Some(r"C:\Elsewhere\a.dll")
                ),
                (Clash::Replaces, r"C:\APP\B.DLL", Some(r"C:\Temp\x.tmp")),
                (
                    Clash::Replaces,
                    r"C:/App/./sub/../b.dll",
                    Some(r"C:\Temp\y.tmp")
                ),
            ]
        );
    }

    #[test]
    fn keeps_the_replacements_the_install_queued() {
        let dest = PathBuf::from(r"C:\App\a.dll");
        let staged = staged_path(&dest);
        let files = InstalledFiles::new([dest.as_path()], &[(staged.clone(), dest.clone())]);
        let own = Operation::new(
            &format!(r"*1\??\{}", staged.display()),
            &format!(r"*1!\??\{}", dest.display()),
        );
        // The same replacement, queued by an earlier install that found the
        // file in use too.
        let earlier = own.clone();
        let ops = vec![
            earlier.clone(),
            Operation::new(r"*1\??\C:\App\a.dll", ""),
            own.clone(),
            Operation::new(r"*1\??\C:\Temp\x.tmp", r"*1!\??\C:\App\a.dll"),
        ];
        let (kept, cancelled) = files.filter(ops);
        assert_eq!(kept, vec![earlier, own]);
        assert_eq!(cancelled.len(), 2);
        assert!(cancelled.iter().all(|c| !c.is_stale_staged_copy()));
    }

    /// A replacement an earlier install queued for a file in use, which
    /// this install has since written directly.
    #[test]
    fn cancels_an_earlier_installs_replacement() {
        let dest = PathBuf::from(r"C:\App\a.dll");
        let staged = staged_path(&dest);
        let files = installed(&[r"C:\App\a.dll"]);
        let (kept, cancelled) = files.filter(vec![Operation::new(
            &format!(r"*1\??\{}", staged.display()),
            &format!(r"*1!\??\{}", dest.display()),
        )]);
        assert!(kept.is_empty());
        assert_eq!(cancelled.len(), 1);
        assert!(cancelled[0].is_stale_staged_copy());
        assert_eq!(
            cancelled[0].other.as_deref(),
            Some(&*staged.to_string_lossy())
        );
    }

    #[test]
    fn matches_unc_paths() {
        let files = installed(&[r"\\srv\share\App\a.dll"]);
        let (kept, cancelled) = files.filter(vec![
            Operation::new(r"\??\UNC\srv\share\app\A.DLL", ""),
            Operation::new(r"\??\UNC\srv\other\App\a.dll", ""),
        ]);
        assert_eq!(cancelled.len(), 1);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn keeps_what_does_not_name_a_written_file() {
        let files = installed(&[r"C:\App\a.dll"]);
        let ops = vec![
            Operation::new(r"*1\??\C:\App\a.dll.old", ""),
            Operation::new(r"*1\??\C:\App", ""),
            Operation::new(r"*1\??\D:\App\a.dll", ""),
            Operation::new(r"*1\??\C:\Temp\a.dll", r"*1\??\C:\Temp\b.dll"),
        ];
        let (kept, cancelled) = files.filter(ops.clone());
        assert_eq!(kept, ops);
        assert!(cancelled.is_empty());
    }

    /// An 8.3 name for a written file is found on disk. Skipped where the
    /// volume makes no short names.
    #[test]
    fn matches_short_names() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;

        let dir = std::env::temp_dir().join(format!("outto_test_pending_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let long = dir.join("a rather long file name.dll");
        std::fs::write(&long, b"x").unwrap();
        let wide: Vec<u16> = long
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut buf = vec![0u16; 1024];
        let n = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
        assert!(n > 0, "{}", std::io::Error::last_os_error());
        let short = String::from_utf16_lossy(&buf[..n as usize]);
        if !short.contains('~') {
            eprintln!("no 8.3 names on this volume; skipping");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let by_long = InstalledFiles::new([long.as_path()], &[]);
        assert!(by_long.contains(&short), "{short}");
        let op = Operation::new(&format!(r"*1\??\{short}"), "");
        assert_eq!(by_long.filter(vec![op]).1.len(), 1);

        let by_short = InstalledFiles::new([Path::new(&short)], &[]);
        assert!(by_short.contains(&long.to_string_lossy()));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
