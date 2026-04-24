//! Install fonts to `~/Library/Fonts` or `/Library/Fonts`.
//!
//! macOS activates fonts on file placement — no registry step like Windows.

use std::path::Path;

use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::InstallManifest;

use crate::config::{FontEntry, FontScope};
use crate::manifest::Action;

pub fn install_font(
    entry: &FontEntry,
    source_dir: &Path,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let source = source_dir.join(&entry.source);
    let file_name = source.file_name().ok_or_else(|| InstallerError::Font {
        file: entry.source.clone(),
        message: "no filename".into(),
    })?;

    let fonts_dir = match entry.scope {
        FontScope::User => user_fonts_dir()?,
        FontScope::System => std::path::PathBuf::from("/Library/Fonts"),
    };

    std::fs::create_dir_all(&fonts_dir).map_err(|e| InstallerError::DirOp {
        path: fonts_dir.clone(),
        source: e,
    })?;

    let dest = fonts_dir.join(file_name);

    callbacks.on_log(
        LogLevel::Info,
        &format!("Fonts: installing {}", dest.display()),
    );

    std::fs::copy(&source, &dest).map_err(|e| InstallerError::Font {
        file: entry.source.clone(),
        message: format!("failed to copy to {}: {e}", dest.display()),
    })?;

    manifest.record(Action::FontInstalled {
        path: dest,
        scope: match entry.scope {
            FontScope::User => "user".to_string(),
            FontScope::System => "system".to_string(),
        },
    });

    Ok(())
}

fn user_fonts_dir() -> InstallerResult<std::path::PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        InstallerError::Other("HOME not set; can't determine ~/Library/Fonts".into())
    })?;
    Ok(std::path::PathBuf::from(home).join("Library/Fonts"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use outto_core::callbacks::NoOpCallbacks;

    #[test]
    fn test_install_font_to_user_dir_mock() {
        // We don't touch the real ~/Library/Fonts in tests; we just verify the
        // code path up to the copy step by pointing HOME at a temp dir.
        let dir = std::env::temp_dir().join(format!("outto-font-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let source_dir = dir.join("src");
        let fake_home = dir.join("home");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&fake_home).unwrap();

        // Minimal fake font file (content doesn't matter for copy).
        std::fs::write(source_dir.join("TestFont.ttf"), b"fake-ttf").unwrap();

        let old_home = std::env::var_os("HOME");
        // SAFETY: test process, sequential test runner assumed.
        unsafe { std::env::set_var("HOME", &fake_home) };

        let entry = FontEntry {
            source: "TestFont.ttf".to_string(),
            scope: FontScope::User,
            component: None,
        };
        let mut manifest = InstallManifest::<Action>::new("t", "T", "1.0.0", &dir, vec![]);
        install_font(&entry, &source_dir, &mut manifest, &NoOpCallbacks).unwrap();

        let expected = fake_home.join("Library/Fonts/TestFont.ttf");
        assert!(expected.exists());
        assert_eq!(manifest.actions.len(), 1);

        if let Some(h) = old_home {
            unsafe { std::env::set_var("HOME", h) };
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
