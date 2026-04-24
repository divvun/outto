# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`outto` is a cross-platform installer/uninstaller framework — a declarative replacement for Inno Setup. Manifests are TOML; output is a single self-extracting signed binary with rollback and a proper uninstaller. **Windows** produces a `.exe`; **macOS** produces a `.app` bundle (SFX `.app` wrapping an inner installer `.app` with the payload in a Mach-O `__OUTTO` segment).

## Workspace layout

All code under `crates/`; the root crate is retired.

- `crates/core` → `outto-core`. Platform-neutral framework: config scaffolding, the `.box` archive packer, neutral action primitives (file copy, directory create, command exec, prereq checks, signing), the generic `InstallManifest<A>` with the `RollbackAction` trait, callbacks. No `windows-sys` at the crate surface.
- `crates/windows` → `outto-windows` (`#![cfg(windows)]`). Windows Config schema, install/uninstall pipelines, registry/COM/services/shortcuts/fonts/associations/environment actions, UAC elevation, ARP registration, PE section embedding, `WindowsAction` enum with `RollbackAction` impl.
- `crates/macos` → `outto-macos` (`#![cfg(target_os = "macos")]`). macOS Config schema, install/uninstall pipelines, launchd/plist/symlinks/fonts/shell-rc actions, `osascript` self-elevation, Mach-O `__OUTTO` segment embedding, receipt-file detection, `MacosAction` enum with `RollbackAction` impl.
- `crates/cli` → binary `outto`. Build-time tool. On Windows: packs to `.box`, embeds as PE section in `outto-gui.exe`, optionally wraps in SFX. On macOS: packs to `.box`, embeds as Mach-O `__OUTTO` segment in `outto-gui`, builds inner installer `.app`, optionally tars + zstd's it and wraps in an SFX `.app`.
- `crates/gui` → binary `outto-gui` (iced). Cross-platform installer GUI. On both Windows and macOS doubles as the installer template — when invoked without args it extracts the embedded payload (via PE section or Mach-O segment) and runs the install.
- `crates/uninstall` → binary `outto-uninstall` (iced). Cross-platform uninstaller GUI.
- `crates/sfx` → `outto-sfx` (Windows-only). PE self-extractor.
- `crates/sfx-macos` → `outto-sfx-macos` (macOS-only). Mach-O self-extractor: reads `__OUTTO` segment, zstd-decompresses the tarballed installer `.app` to `$TMPDIR`, execs it.

`box-format` is a path dependency at `../../../box` (`divvun/box/`).

## Build / test

Dev iteration:

```
cargo +nightly check --workspace
cargo +nightly test -p outto-core
cargo +nightly test -p outto-macos      # 49 tests; runs on macOS host
```

Release builds need nightly + `-Zbuild-std=std` because `[profile.release]` sets `panic = "immediate-abort"`.

- **Windows**: `./scripts/build-release.ps1` (PowerShell) → `target/release/outto-setup.exe`.
- **macOS**: `./scripts/build-release.sh` → `target/release/outto-setup.app`. Pass `--sign "codesign --sign 'Developer ID Application: XYZ (TEAMID)' --deep --options runtime #{file}"` to sign each layer, and `--notarize --keychain-profile <name>` to submit to Apple's notary service and staple.

Admin-only Windows integration tests live in `crates/windows/tests/admin_integration.rs` (need an elevated shell + `cargo nextest run -P admin --run-ignored ignored-only`).

## Core ↔ platform contract

1. **Generic manifest**: `outto_core::InstallManifest<A>` where `A: RollbackAction`. Each platform defines its own `Action` enum with a `From<CoreAction>` impl (for recording neutral actions like `FileCopied`, `DirectoryCreated`, `CommandExecuted`) and a `RollbackAction` impl (which owns the OS-specific reverse semantics — registry rollback on Windows, launchctl bootout on macOS, plist value restoration, etc.).
2. **Platform install entry points**: each platform crate exports `pub fn install()` / `pub fn uninstall_package()`. Binaries select via `#[cfg(windows)] use outto_windows as platform; #[cfg(target_os = "macos")] use outto_macos as platform;`. Core has no top-level install fn.
3. **`Config` type**: platform-specific. `outto_windows::Config` has registry/services/COM/shortcuts sections; `outto_macos::Config` has launchd/plist/symlinks/bundle sections. Binaries type-alias via `platform::Config`.

## Transactionality

Every mutation is recorded as an `Action` variant *before* it completes. Install failure triggers `rollback_actions` which walks the recorded list in reverse and calls `RollbackAction::rollback(&self, restore_backups: bool, callbacks)` on each. Uninstall uses the same mechanism with `restore_backups = false`.

To add a new action type:
1. Add a variant to the platform's `Action` enum (data-only — no runtime behaviour yet).
2. Record it from the relevant action module *after* the mutation succeeds, capturing whatever previous-state info is needed to undo.
3. Extend `impl RollbackAction for <PlatformAction>` with the reverse.
4. If admin-only, add an `#[ignore]` integration test.

## Receipt tracking

Where we stash per-install metadata (the "Add/Remove Programs" analog):

- **Windows**: `{install_dir}/.outto/{package_id}/manifest.json` + ARP registry entry at `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{package_id}` with `ManagedBy = "outto"`.
- **macOS**: `~/Library/no.divvun.install/packages/<pkg-id>/` (user scope) or `/Library/no.divvun.install/packages/<pkg-id>/` (system scope). Contains `manifest.json`, `receipt.json` (compact metadata), and `uninstall.app/`. Enumeration walks both bases; `detect::detect_existing_install` checks user first.

## TOML schemas

The two platforms have **separate** schemas, in separate files:

- **`outto.toml`**: Windows schema. Has `[[registry]]`, `[[shortcuts]]`, `[[services]]`, `[[com]]`, `[[associations]]`, `[[fonts]]`, `[[environment]]`, `[reboot]`, etc. Defined in `outto_core::config`.
- **`outto.macos.toml`**: macOS schema. Has `[[launchd]]`, `[[plist]]`, `[[symlinks]]`, `[[fonts]]` (simpler), `[[environment]]` (shell rc blocks), `[[associations]]` (lsregister wrapper), `[privileges]`. Defined in `outto_macos::config`.

Common sections on both: `[package]`, `[[files]]`, `[[dirs]]`, `[[run]]`, `[[prerequisites]]`, `[[components]]`, `[upgrade]`, `[uninstall]`, `[logging]`.

### macOS path variables

Added by `outto_macos::paths::with_macos_env()`:

| Variable | Value |
|---|---|
| `#{app}` | resolved install dir (typically `/Applications/MyApp.app`) |
| `#{applications}` | `/Applications` |
| `#{user_applications}` | `~/Applications` |
| `#{home}`, `#{user_library}`, `#{library}` | the obvious paths |
| `#{local}`, `#{local_bin}` | `/usr/local`, `/usr/local/bin` |
| `#{launch_agents_user}` / `#{launch_agents_system}` / `#{launch_daemons}` | launchd dirs |
| `#{fonts_user}` / `#{fonts_system}` | `~/Library/Fonts` / `/Library/Fonts` |
| `#{prefs_user}` / `#{prefs_system}` | Preferences dirs |
| `#{app_support_user}` / `#{app_support_system}` | Application Support dirs |
| `#{tmp}` | `$TMPDIR` |
| `#{package.name}`, `#{package.version}` | shared |

### macOS elevation

For actions that touch `/Library`, `/usr/local`, `/Library/LaunchDaemons`, etc. (when `[privileges] required = "admin"` or `"auto"` with an install path under one of those roots), `outto_macos::elevation::elevate_self` re-launches the current process through `osascript 'do shell script "..." with administrator privileges'`. The user sees a standard macOS password prompt.

## Conventions / gotchas

- Edition 2021 on all crates.
- `.cargo/config.toml` forces `+crt-static` on `x86_64-pc-windows-msvc`.
- Windows API calls go through `windows-sys` (not `windows`).
- macOS .app bundles copied via `ditto` — never `fs::copy` / `cp -r`, which lose xattrs and resource forks.
- Mach-O payload embedding requires the binary to be **thin** (not fat/universal). Cargo defaults to thin; fat support would need a `lipo` step upstream.
- Codesign *after* embedding: `outto build` embeds first, then invokes the `--sign` command so the signature covers the new segment.
- macOS tests run on any macOS host without needing sudo — the elevation path is only exercised manually (`sudo cargo run` or double-click a real installer).
