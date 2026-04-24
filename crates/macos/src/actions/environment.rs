//! Shell rc file environment-variable management.
//!
//! Writes `export` lines into `~/.zshrc`, `~/.bashrc`, etc., wrapped in
//! `# BEGIN outto: <package-id>` / `# END outto: <package-id>` comments so
//! uninstall can locate and remove exactly our additions without clobbering
//! unrelated edits the user made by hand.

use std::path::{Path, PathBuf};

use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::VariableResolver;
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::InstallManifest;

use crate::config::{EnvAction, EnvironmentEntry, Shell};
use crate::manifest::Action;

pub fn apply_environment_entry(
    entry: &EnvironmentEntry,
    package_id: &str,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let value = resolver.resolve(&entry.value)?;

    for shell in &entry.shells {
        let rc_file = rc_file_for(shell)?;
        if !rc_file.exists() {
            if let Some(parent) = rc_file.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::File::create(&rc_file).map_err(|e| InstallerError::FileOp {
                path: rc_file.clone(),
                source: e,
            })?;
        }

        let contents = std::fs::read_to_string(&rc_file).map_err(|e| InstallerError::FileOp {
            path: rc_file.clone(),
            source: e,
        })?;
        let without = strip_guarded_block(&contents, package_id);
        let snippet = render_snippet(shell, &entry.name, &value, &entry.action);

        let mut new_contents = without;
        if !new_contents.is_empty() && !new_contents.ends_with('\n') {
            new_contents.push('\n');
        }
        new_contents.push_str(&begin_marker(package_id));
        new_contents.push('\n');
        new_contents.push_str(&snippet);
        new_contents.push_str(&end_marker(package_id));
        new_contents.push('\n');

        callbacks.on_log(
            LogLevel::Info,
            &format!(
                "env: updating {} ({})",
                rc_file.display(),
                shell_name(shell)
            ),
        );

        std::fs::write(&rc_file, new_contents).map_err(|e| InstallerError::FileOp {
            path: rc_file.clone(),
            source: e,
        })?;

        manifest.record(Action::ShellRcModified {
            rc_file,
            package_id: package_id.to_string(),
        });
    }

    Ok(())
}

/// Remove the outto-guarded block for `package_id` from `rc_file`. Idempotent.
pub fn remove_guarded_block(rc_file: &Path, package_id: &str) -> InstallerResult<()> {
    if !rc_file.exists() {
        return Ok(());
    }
    let contents = std::fs::read_to_string(rc_file).map_err(|e| InstallerError::FileOp {
        path: rc_file.to_path_buf(),
        source: e,
    })?;
    let stripped = strip_guarded_block(&contents, package_id);
    std::fs::write(rc_file, stripped).map_err(|e| InstallerError::FileOp {
        path: rc_file.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

fn begin_marker(package_id: &str) -> String {
    format!("# BEGIN outto: {package_id}")
}

fn end_marker(package_id: &str) -> String {
    format!("# END outto: {package_id}")
}

fn rc_file_for(shell: &Shell) -> InstallerResult<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| InstallerError::Other("HOME not set; can't locate shell rc file".into()))?;
    let base = PathBuf::from(home);
    Ok(match shell {
        Shell::Zsh => base.join(".zshrc"),
        Shell::Bash => base.join(".bashrc"),
        Shell::Fish => base.join(".config/fish/config.fish"),
    })
}

fn shell_name(shell: &Shell) -> &'static str {
    match shell {
        Shell::Zsh => "zsh",
        Shell::Bash => "bash",
        Shell::Fish => "fish",
    }
}

fn render_snippet(shell: &Shell, name: &str, value: &str, action: &EnvAction) -> String {
    match shell {
        Shell::Fish => render_fish(name, value, action),
        _ => render_posix(name, value, action),
    }
}

fn render_posix(name: &str, value: &str, action: &EnvAction) -> String {
    match action {
        EnvAction::Set => format!("export {name}={}\n", shell_quote(value)),
        EnvAction::Append => format!(
            "export {name}=\"${{{name}:+${{{name}}}:}}{}\"\n",
            shell_escape_double(value)
        ),
        EnvAction::Prepend => format!(
            "export {name}=\"{}${{{name}:+:${{{name}}}}}\"\n",
            shell_escape_double(value)
        ),
        EnvAction::Remove => "# (remove) intentionally empty\n".to_string(),
    }
}

fn render_fish(name: &str, value: &str, action: &EnvAction) -> String {
    match action {
        EnvAction::Set => format!("set -gx {name} {}\n", shell_quote(value)),
        EnvAction::Append => format!("set -gx {name} ${name} {}\n", shell_quote(value)),
        EnvAction::Prepend => format!("set -gx {name} {} ${name}\n", shell_quote(value)),
        EnvAction::Remove => "# (remove) intentionally empty\n".to_string(),
    }
}

fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn shell_escape_double(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '$' | '\\' | '"' | '`' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Remove every `# BEGIN outto: <pkg>` ... `# END outto: <pkg>` block (lines
/// inclusive) from `contents`. Handles multiple blocks gracefully.
fn strip_guarded_block(contents: &str, package_id: &str) -> String {
    let begin = begin_marker(package_id);
    let end = end_marker(package_id);
    let mut out = String::with_capacity(contents.len());
    let mut inside = false;

    for line in contents.lines() {
        if !inside {
            if line.trim_end() == begin {
                inside = true;
            } else {
                out.push_str(line);
                out.push('\n');
            }
        } else if line.trim_end() == end {
            inside = false;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_guarded_block_removes_exact_block() {
        let input =
            "first\n# BEGIN outto: no.divvun.test\nexport X=1\n# END outto: no.divvun.test\nlast\n";
        let out = strip_guarded_block(input, "no.divvun.test");
        assert_eq!(out, "first\nlast\n");
    }

    #[test]
    fn test_strip_guarded_block_leaves_other_blocks_alone() {
        let input = "# BEGIN outto: other.pkg\nexport Y=2\n# END outto: other.pkg\n# BEGIN outto: keep.me\nexport Z=3\n# END outto: keep.me\n";
        let out = strip_guarded_block(input, "other.pkg");
        assert_eq!(
            out,
            "# BEGIN outto: keep.me\nexport Z=3\n# END outto: keep.me\n"
        );
    }

    #[test]
    fn test_strip_guarded_block_on_empty_content() {
        let out = strip_guarded_block("", "no.divvun.test");
        assert_eq!(out, "");
    }

    #[test]
    fn test_render_posix_set() {
        let s = render_posix("FOO", "bar baz", &EnvAction::Set);
        assert_eq!(s, "export FOO='bar baz'\n");
    }

    #[test]
    fn test_render_posix_append() {
        let s = render_posix("PATH", "/opt/bin", &EnvAction::Append);
        assert!(s.contains("/opt/bin"));
        assert!(s.starts_with("export PATH"));
    }

    #[test]
    fn test_shell_quote_escapes_singles() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_apply_and_remove_roundtrip() {
        let dir = std::env::temp_dir().join(format!("outto-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let old_home = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", &dir) };
        std::fs::write(dir.join(".zshrc"), "# existing user config\n").unwrap();

        let mut resolver = VariableResolver::new().with_windows_paths(false);
        resolver.set_variable("base", "/opt/myapp");

        let entry = EnvironmentEntry {
            name: "MYAPP_HOME".to_string(),
            value: "#{base}".to_string(),
            action: EnvAction::Set,
            shells: vec![Shell::Zsh],
            component: None,
        };

        let mut manifest =
            InstallManifest::<Action>::new("no.divvun.test", "T", "1.0.0", &dir, vec![]);

        apply_environment_entry(
            &entry,
            "no.divvun.test",
            &resolver,
            &mut manifest,
            &outto_core::callbacks::NoOpCallbacks,
        )
        .unwrap();

        let after_install = std::fs::read_to_string(dir.join(".zshrc")).unwrap();
        assert!(after_install.contains("# BEGIN outto: no.divvun.test"));
        assert!(after_install.contains("MYAPP_HOME='/opt/myapp'"));
        assert!(after_install.contains("# existing user config"));

        remove_guarded_block(&dir.join(".zshrc"), "no.divvun.test").unwrap();
        let after_remove = std::fs::read_to_string(dir.join(".zshrc")).unwrap();
        assert!(!after_remove.contains("# BEGIN outto: no.divvun.test"));
        assert!(!after_remove.contains("MYAPP_HOME"));
        assert!(after_remove.contains("# existing user config"));

        if let Some(h) = old_home {
            unsafe { std::env::set_var("HOME", h) };
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
