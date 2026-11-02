//! `[[retire]]`: remove what other installers left, once this package's own
//! install has succeeded.
//!
//! Entries run in file order, each logged under its `name`. Within one,
//! paths go first, then package receipts, then launchd jobs. Booting out a
//! job ends every process in it, which can include whatever is waiting on
//! this install, such as an old updater's `installer -pkg`, so nothing may
//! depend on running after that. A job's plist is deleted before the job is
//! booted out, by label: should whatever ran this install go down with the
//! job and take this process along, launchd still has no plist to start the
//! job again from, against files that are gone, at the next boot. Each step
//! is logged; one that fails is logged and the rest still run. Nothing is
//! recorded in the manifest, so neither a rollback nor an uninstall brings
//! anything back.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::VariableResolver;
use outto_core::error::InstallerResult;

use super::launchd;
use crate::config::RetireEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    RemovePath(PathBuf),
    ForgetReceipt(String),
    /// Delete `plist`, then boot the job out of `target` and wait until it
    /// is gone.
    RetireJob {
        target: String,
        label: String,
        plist: PathBuf,
    },
}

/// The steps that act beyond the filesystem. They need root and change the
/// real system, so tests stand in for them.
pub(crate) trait Host {
    fn forget_receipt(&self, id: &str) -> Result<(), String>;
    fn bootout(
        &self,
        target: &str,
        label: &str,
        callbacks: &dyn InstallerCallbacks,
    ) -> InstallerResult<()>;
}

struct Live;

impl Host for Live {
    fn forget_receipt(&self, id: &str) -> Result<(), String> {
        let output = forget_command(id)
            .output()
            .map_err(|e| format!("pkgutil failed to launch: {e}"))?;
        if output.status.success() {
            return Ok(());
        }
        Err(format!(
            "pkgutil --forget exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }

    fn bootout(
        &self,
        target: &str,
        label: &str,
        callbacks: &dyn InstallerCallbacks,
    ) -> InstallerResult<()> {
        launchd::bootout(target, label, callbacks)
    }
}

fn forget_command(id: &str) -> Command {
    let mut command = Command::new("/usr/sbin/pkgutil");
    command.args(["--forget", id]);
    command
}

/// Retire everything the `[[retire]]` entries list. Only for after the
/// install has succeeded; `install_dir` is the directory it installed to,
/// which no step may touch.
pub fn retire(
    entries: &[RetireEntry],
    resolver: &VariableResolver,
    install_dir: &Path,
    callbacks: &dyn InstallerCallbacks,
) {
    retire_with(entries, resolver, install_dir, &Live, callbacks);
}

fn retire_with(
    entries: &[RetireEntry],
    resolver: &VariableResolver,
    install_dir: &Path,
    host: &dyn Host,
    callbacks: &dyn InstallerCallbacks,
) {
    for entry in entries {
        let steps = plan(entry, resolver, install_dir, callbacks);
        run(&entry.name, &steps, host, callbacks);
    }
}

/// One entry's steps in the order they run. A path that does not resolve to
/// one [`removable`] accepts is logged and left out.
pub(crate) fn plan(
    entry: &RetireEntry,
    resolver: &VariableResolver,
    install_dir: &Path,
    callbacks: &dyn InstallerCallbacks,
) -> Vec<Step> {
    let name = &entry.name;
    let path = |what: &str, raw: &str| {
        let checked = resolver
            .resolve_path(raw)
            .map_err(|e| e.to_string())
            .and_then(|p| removable(&p, install_dir).map(|()| p));
        checked
            .inspect_err(|why| {
                callbacks.on_log(
                    LogLevel::Warn,
                    &format!("Retire {name}: skipping {what} '{raw}': {why}"),
                )
            })
            .ok()
    };

    let mut steps: Vec<Step> = entry
        .paths
        .iter()
        .filter_map(|raw| path("path", raw).map(Step::RemovePath))
        .collect();
    steps.extend(
        entry
            .pkg_receipts
            .iter()
            .map(|id| Step::ForgetReceipt(id.clone())),
    );
    steps.extend(entry.launchd.iter().filter_map(|job| {
        Some(Step::RetireJob {
            target: launchd::domain_for(&job.scope),
            label: job.label.clone(),
            plist: path(
                &format!("the plist of launchd job {}", job.label),
                &job.plist,
            )?,
        })
    }));
    steps
}

/// Refuse paths that would take far more than one program's files with
/// them: `/`, a top-level directory such as `/Applications`, anything going
/// through `..`, and the new install itself, inside or around it.
fn removable(path: &Path, install_dir: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("not an absolute path".into());
    }
    if path.components().any(|c| c == Component::ParentDir) {
        return Err("goes through ..".into());
    }
    let depth = path
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    if depth < 2 {
        return Err("is / or a top-level directory".into());
    }
    if path.starts_with(install_dir) || install_dir.starts_with(path) {
        return Err(format!(
            "overlaps the install directory {}",
            install_dir.display()
        ));
    }
    Ok(())
}

pub(crate) fn run(name: &str, steps: &[Step], host: &dyn Host, callbacks: &dyn InstallerCallbacks) {
    let log = |level: LogLevel, message: String| {
        callbacks.on_log(level, &format!("Retire {name}: {message}"))
    };
    for step in steps {
        match step {
            Step::RemovePath(path) => remove_path(path, &log),
            Step::ForgetReceipt(id) => {
                log(LogLevel::Info, format!("forgetting package receipt {id}"));
                if let Err(e) = host.forget_receipt(id) {
                    log(
                        LogLevel::Warn,
                        format!("could not forget package receipt {id}: {e}"),
                    );
                }
            }
            Step::RetireJob {
                target,
                label,
                plist,
            } => {
                remove_path(plist, &log);
                log(
                    LogLevel::Info,
                    format!("booting out launchd job {target}/{label}"),
                );
                if let Err(e) = host.bootout(target, label, callbacks) {
                    log(
                        LogLevel::Warn,
                        format!("could not boot out {target}/{label}: {e}"),
                    );
                }
            }
        }
    }
}

/// Delete a file, symlink (not what it points to), socket, or directory
/// with everything in it.
fn remove_path(path: &Path, log: &dyn Fn(LogLevel, String)) {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log(LogLevel::Info, format!("{} is not there", path.display()));
            return;
        }
        Err(e) => {
            log(
                LogLevel::Warn,
                format!("could not inspect {}: {e}", path.display()),
            );
            return;
        }
    };
    log(LogLevel::Info, format!("removing {}", path.display()));
    let removed = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    if let Err(e) = removed {
        log(
            LogLevel::Warn,
            format!("could not remove {}: {e}", path.display()),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use outto_core::callbacks::{Prompt, PromptResponse};
    use outto_core::error::{ErrorAction, InstallerError};

    use super::*;
    use crate::config::Config;

    /// Records, in one sequence, the log and what [`Host`] was asked to do.
    /// Forgetting `fail.*` receipts and booting out `fail.*` jobs fails.
    #[derive(Default)]
    struct Trace {
        events: RefCell<Vec<String>>,
        /// Paths whose existence each host call records, to show what was
        /// already gone by then.
        watch: Vec<PathBuf>,
    }

    impl Trace {
        fn present(&self) -> String {
            let present: Vec<String> = self
                .watch
                .iter()
                .filter(|p| p.symlink_metadata().is_ok())
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            format!("[{}]", present.join(","))
        }
    }

    impl Host for Trace {
        fn forget_receipt(&self, id: &str) -> Result<(), String> {
            self.events
                .borrow_mut()
                .push(format!("pkgutil --forget {id} {}", self.present()));
            match id.starts_with("fail.") {
                true => Err("No receipt".into()),
                false => Ok(()),
            }
        }

        fn bootout(
            &self,
            target: &str,
            label: &str,
            _: &dyn InstallerCallbacks,
        ) -> InstallerResult<()> {
            self.events.borrow_mut().push(format!(
                "launchctl bootout {target}/{label} {}",
                self.present()
            ));
            match label.starts_with("fail.") {
                true => Err(InstallerError::Other("did not exit".into())),
                false => Ok(()),
            }
        }
    }

    impl InstallerCallbacks for Trace {
        fn on_progress(&self, _: &str, _: u64, _: u64) {}
        fn on_prompt(&self, _: Prompt) -> PromptResponse {
            PromptResponse::Yes
        }
        fn on_log(&self, level: LogLevel, message: &str) {
            self.events
                .borrow_mut()
                .push(format!("{level:?}: {message}"));
        }
        fn on_error(&self, _: &InstallerError) -> ErrorAction {
            ErrorAction::Abort
        }
    }

    fn config(retire: &str) -> Config {
        Config::from_toml(&format!(
            "[package]\nid = \"no.divvun.pahkat\"\nname = \"Pahkat\"\nversion = \"3.0.0\"\n\n{retire}"
        ))
        .unwrap()
    }

    #[test]
    fn forget_runs_pkgutil() {
        let command = forget_command("no.divvun.Manager");
        assert_eq!(command.get_program(), "/usr/sbin/pkgutil");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["--forget", "no.divvun.Manager"]
        );
    }

    /// The old macOS Pahkat, as main/pahkat's manifest retires it: jobs come
    /// last whatever order the entry lists them in.
    #[test]
    fn plan_runs_paths_then_receipts_then_jobs() {
        let config = config(
            r##"
[[retire]]
name = "Divvun Manager"
launchd = [{ label = "no.divvun.pahkatd", scope = "daemon", plist = "#{launch_daemons}/no.divvun.pahkatd.plist" }]
paths = ["#{applications}/Divvun Manager.app", "/tmp/pahkat.sock", "#{library}/Caches/Pahkat", "/Library/Logs/Pahkat"]
pkg_receipts = ["no.divvun.Manager"]
"##,
        );
        let install_dir = PathBuf::from("/usr/local/pahkat");
        let resolver = crate::make_resolver(&config, Some(&install_dir));
        let trace = Trace::default();
        let steps = plan(&config.retire[0], &resolver, &install_dir, &trace);
        assert_eq!(
            steps,
            [
                Step::RemovePath("/Applications/Divvun Manager.app".into()),
                Step::RemovePath("/tmp/pahkat.sock".into()),
                Step::RemovePath("/Library/Caches/Pahkat".into()),
                Step::RemovePath("/Library/Logs/Pahkat".into()),
                Step::ForgetReceipt("no.divvun.Manager".into()),
                Step::RetireJob {
                    target: "system".into(),
                    label: "no.divvun.pahkatd".into(),
                    plist: "/Library/LaunchDaemons/no.divvun.pahkatd.plist".into(),
                },
            ]
        );
        assert!(trace.events.borrow().is_empty());
    }

    #[test]
    fn plan_leaves_out_what_would_take_too_much() {
        let config = config("");
        let install_dir = PathBuf::from("/usr/local/pahkat");
        let resolver = crate::make_resolver(&config, Some(&install_dir));
        let entry = RetireEntry {
            name: "Old".into(),
            paths: [
                "/",
                "/Applications",
                "#{applications}",
                "/Library/Caches/../../etc",
                "/usr/local",
                "#{app}",
                "/usr/local/pahkat/pahkat",
                "#{nonexistent}/x",
                "/Library/Logs/Pahkat",
            ]
            .map(String::from)
            .to_vec(),
            pkg_receipts: Vec::new(),
            launchd: vec![crate::config::RetireLaunchdEntry {
                label: "no.divvun.old".into(),
                scope: crate::config::LaunchdScope::Daemon,
                plist: "/Library".into(),
            }],
        };
        let trace = Trace::default();
        let steps = plan(&entry, &resolver, &install_dir, &trace);
        assert_eq!(steps, [Step::RemovePath("/Library/Logs/Pahkat".into())]);
        let events = trace.events.borrow();
        assert_eq!(events.len(), 9, "{events:#?}");
        assert!(
            events
                .iter()
                .all(|e| e.starts_with("Warn: Retire Old: skipping"))
        );
    }

    /// Everything listed goes, in order, past steps that fail; a job is
    /// booted out only once the paths, the receipts and its own plist are
    /// gone, and a failed bootout does not stop the next job.
    #[test]
    fn run_carries_on_past_failures_and_ends_with_jobs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let app = root.join("Old.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/MacOS/old"), "old").unwrap();
        let log = root.join("old.log");
        std::fs::write(&log, "log").unwrap();
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("keep"), "keep").unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        let socket_path = root.join("old.sock");
        let _socket = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
        let missing = root.join("missing");
        let plist = root.join("fail.old.plist");
        std::fs::write(&plist, "<plist/>").unwrap();
        let other_plist = root.join("no.divvun.older.plist");
        std::fs::write(&other_plist, "<plist/>").unwrap();

        let steps = [
            Step::RemovePath(app.clone()),
            Step::RemovePath(log.clone()),
            Step::RemovePath(link.clone()),
            Step::RemovePath(socket_path.clone()),
            Step::RemovePath(missing.clone()),
            Step::ForgetReceipt("fail.receipt".into()),
            Step::ForgetReceipt("no.divvun.Manager".into()),
            Step::RetireJob {
                target: "system".into(),
                label: "fail.old".into(),
                plist: plist.clone(),
            },
            Step::RetireJob {
                target: "system".into(),
                label: "no.divvun.older".into(),
                plist: other_plist.clone(),
            },
        ];
        let trace = Trace {
            watch: vec![
                app.clone(),
                log.clone(),
                link.clone(),
                socket_path.clone(),
                plist.clone(),
                other_plist.clone(),
            ],
            ..Trace::default()
        };
        run("Old", &steps, &trace, &trace);

        let shown = |p: &Path| p.display().to_string();
        let expected = vec![
            format!("Info: Retire Old: removing {}", shown(&app)),
            format!("Info: Retire Old: removing {}", shown(&log)),
            format!("Info: Retire Old: removing {}", shown(&link)),
            format!("Info: Retire Old: removing {}", shown(&socket_path)),
            format!("Info: Retire Old: {} is not there", shown(&missing)),
            "Info: Retire Old: forgetting package receipt fail.receipt".to_string(),
            "pkgutil --forget fail.receipt [fail.old.plist,no.divvun.older.plist]".to_string(),
            "Warn: Retire Old: could not forget package receipt fail.receipt: No receipt"
                .to_string(),
            "Info: Retire Old: forgetting package receipt no.divvun.Manager".to_string(),
            "pkgutil --forget no.divvun.Manager [fail.old.plist,no.divvun.older.plist]".to_string(),
            format!("Info: Retire Old: removing {}", shown(&plist)),
            "Info: Retire Old: booting out launchd job system/fail.old".to_string(),
            "launchctl bootout system/fail.old [no.divvun.older.plist]".to_string(),
            "Warn: Retire Old: could not boot out system/fail.old: did not exit".to_string(),
            format!("Info: Retire Old: removing {}", shown(&other_plist)),
            "Info: Retire Old: booting out launchd job system/no.divvun.older".to_string(),
            "launchctl bootout system/no.divvun.older []".to_string(),
        ];
        assert_eq!(*trace.events.borrow(), expected);
        for gone in [&app, &log, &link, &socket_path, &plist, &other_plist] {
            assert!(gone.symlink_metadata().is_err(), "{}", gone.display());
        }
        assert_eq!(
            std::fs::read_to_string(elsewhere.join("keep")).unwrap(),
            "keep"
        );
    }

    /// Entries run one after another in file order, each with its own
    /// paths, receipts and jobs, and each logged under its own name.
    #[test]
    fn entries_run_in_file_order() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let first_app = root.join("First.app");
        let second_app = root.join("Second.app");
        let first_plist = root.join("no.divvun.first.plist");
        for dir in [&first_app, &second_app] {
            std::fs::create_dir(dir).unwrap();
        }
        std::fs::write(&first_plist, "<plist/>").unwrap();
        let config = config(&format!(
            r##"
[[retire]]
name = "First"
launchd = [{{ label = "no.divvun.first", scope = "daemon", plist = "{first_plist}" }}]
paths = ["{first_app}"]

[[retire]]
name = "Second"
pkg_receipts = ["no.divvun.second"]
paths = ["{second_app}"]
"##,
            first_plist = first_plist.display(),
            first_app = first_app.display(),
            second_app = second_app.display(),
        ));
        let install_dir = root.join("install");
        let resolver = crate::make_resolver(&config, Some(&install_dir));
        let trace = Trace {
            watch: vec![first_app.clone(), second_app.clone()],
            ..Trace::default()
        };
        retire_with(&config.retire, &resolver, &install_dir, &trace, &trace);

        let shown = |p: &Path| p.display().to_string();
        assert_eq!(
            *trace.events.borrow(),
            [
                format!("Info: Retire First: removing {}", shown(&first_app)),
                format!("Info: Retire First: removing {}", shown(&first_plist)),
                "Info: Retire First: booting out launchd job system/no.divvun.first".to_string(),
                "launchctl bootout system/no.divvun.first [Second.app]".to_string(),
                format!("Info: Retire Second: removing {}", shown(&second_app)),
                "Info: Retire Second: forgetting package receipt no.divvun.second".to_string(),
                "pkgutil --forget no.divvun.second []".to_string(),
            ]
        );
    }
}
