# outto

A Windows installer/uninstaller framework — a modern, declarative replacement for Inno Setup. Describe your installer in TOML; `outto` packages it into a single self-extracting `.exe` with rollback, a proper Add/Remove Programs entry, and a real uninstaller.

> **Status:** 0.1.0, early. Windows only. The TOML schema may still shift.

## What it does

- **Declarative manifests.** One `outto.toml` describes files, directories, registry entries, shortcuts, environment variables, services, file associations, COM registrations, font installs, and before/after commands.
- **Self-extracting installers.** `outto build` stages your source tree, packs it into a zstd-compressed `box_format` archive, and embeds it into an SFX stub — the result is a single signed `.exe`.
- **Transactional installs.** Every action is recorded; any failure rolls everything back. Successful installs write a manifest to `{install_dir}/.outto/{package_id}/` that the bundled uninstaller replays in reverse.
- **Upgrade-aware.** Detects existing installs via the registry. Choose `overwrite`, `side_by_side`, or `fail`.
- **Dependency cascade.** Packages may declare `depends_on`; uninstalling a dependency cascades to its dependents so nothing is left dangling.
- **Admin or per-user.** `privileges = "admin" | "user" | "auto"` — outto elevates only when required.

## Quick start

1. Install Rust nightly and add the MSVC target:
   ```
   rustup toolchain install nightly
   rustup component add rust-src --toolchain nightly
   rustup target add x86_64-pc-windows-msvc --toolchain nightly
   ```

2. Build outto itself (see **Building from source** below).

3. Write an `outto.toml` next to your app's staged files:
   ```toml
   [package]
   id = "com.example.myapp"
   name = "My Application"
   version = "1.0.0"
   publisher = "Example Corp"
   default_dir = "#{pf}/My Application"
   privileges = "admin"

   [uninstall]
   display_icon = "#{app}/bin/myapp.exe"
   remove_app_dir = true

   [[files]]
   source = "bin/*"
   dest = "#{app}/bin"
   overwrite = "always"

   [[shortcuts]]
   name = "My Application"
   target = "#{app}/bin/myapp.exe"
   location = "start_menu"

   [[environment]]
   name = "PATH"
   value = "#{app}/bin"
   scope = "system"
   action = "append"
   ```

4. Stage your files in a source directory, then build the installer:
   ```
   outto build --config outto.toml --source ./staged --output myapp-setup.exe --compress --compression-level 22
   ```

   Add `--sign <command>` to code-sign each produced binary (the uninstaller, the installer, and the SFX wrapper are each signed in turn).

The resulting `myapp-setup.exe` is self-contained — double-click to run the GUI installer, or call it with `/S` for a silent install.

## Path variables

TOML values accept `#{…}` placeholders that are resolved at install time:

| Variable | Value |
|---|---|
| `#{app}` | Resolved install directory |
| `#{pf}` | `%ProgramFiles%` |
| `#{windir}` | `%WINDIR%` |
| `#{sys}` | `%WINDIR%\System32` |
| `#{userappdata}` | Current user's `%APPDATA%` |
| `#{commonappdata}` | `%PROGRAMDATA%` |
| `#{package.name}` | From `[package].name` |
| `#{package.version}` | From `[package].version` |

## Configuration sections

| Section | Purpose |
|---|---|
| `[package]` | id, name, version, publisher, default_dir, privileges, architecture |
| `[[files]]` | Copy files/globs into `dest`, per-file `overwrite` policy |
| `[[dirs]]` | Create directories (optionally preserved on uninstall) |
| `[[registry]]` | HKLM/HKCU/HKCR keys and values |
| `[[shortcuts]]` | Start Menu / Desktop / Quick Launch shortcuts |
| `[[environment]]` | System or user environment variables (`set` / `append` / `prepend`) |
| `[[services]]` | Windows service install/start |
| `[[associations]]` | File-type associations |
| `[[com]]` | COM server registration |
| `[[fonts]]` | Font installation |
| `[[prerequisites]]` | Detect and require prior installs |
| `[[run]]` | Run commands in `before_install` / `after_install` / `before_uninstall` phases |
| `[[components]]` | Optional/required component groups; each entry may set `component = "foo"` |
| `[upgrade]` | `policy = "overwrite" \| "side_by_side" \| "fail"` |
| `[uninstall]` | Display icon, whether to remove `#{app}` on uninstall, extra dirs |
| `[reboot]` | `never` / `if_needed` / `always`, Restart Manager integration |
| `[logging]` | Install log path |

See `outto.toml` in the repo root for the manifest used to package outto itself.

## Building from source

outto is a Cargo workspace: a library crate plus `outto`, `outto-gui`, `outto-sfx`, and `outto-uninstall` binaries. Release builds require nightly + a rebuilt std (the release profile opts into `panic = "immediate-abort"` for a smaller installer).

```
./scripts/build-release.ps1
```

This builds every crate, stages them into `target/installer-stage/{bin,libexec}`, and invokes the CLI to produce `target/release/outto-setup.exe`. Pass `-Sign <command>` to sign each artifact.

For development iteration:

```
cargo +nightly build --workspace
cargo +nightly test
```

Admin-scoped integration tests must be run from an elevated shell:

```
./scripts/test-admin.ps1
```

## License

MIT.
