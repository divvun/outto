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

## Exit codes

The installer and the uninstaller exit with the same codes, on Windows and macOS:

| Code | Meaning |
|---|---|
| 0 | Succeeded. |
| 1 | Failed, and the machine is as it was before. |
| 2 | Refused: an `[[uninstall.refuse_while]]` condition holds and nothing changed. Retrying once it passes can succeed. |
| 3 | Failed, and outto could not undo everything it had done. |
| 4 | The command line, configuration or payload is unusable, and nothing changed. |
| 3010 | Succeeded, but the machine must restart, and outto did not restart it (with `/NORESTART`, or when the restart could not be started). This is Windows Installer's `ERROR_SUCCESS_REBOOT_REQUIRED`. |

A silent install restarts the machine itself when a restart is needed, unless `/NORESTART` is passed. Callers that schedule restarts themselves pass `/NORESTART` and act on 3010.

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
| `#{startmenu}`, `#{desktop}`, `#{startup}` | The current user's Start Menu programs, desktop and Startup folders |
| `#{commonstartmenu}`, `#{commondesktop}`, `#{commonstartup}` | All users' (`C:\ProgramData\Microsoft\Windows\Start Menu\Programs`, `C:\Users\Public\Desktop`, `...\Programs\Startup`) |
| `#{package.name}` | From `[package].name` |
| `#{package.version}` | From `[package].version` |

### Passing `/LOG` on to another installer

A `[[run]]` command that runs another installer can hand it the current log with `#{log_arg:NAME}`, an argument of its own in `arguments`:

```toml
[[run]]
phase = "after_install"
command = "#{app}/dependencies/tip-setup.exe"
arguments = "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART #{log_arg:text-service}"
wait = true
```

When the installer or uninstaller runs with `/LOG`, `#{log_arg:NAME}` becomes the single argument `/LOG=<path>`, a log next to the current one with `.NAME` added before the extension: with `/LOG=C:\logs\install.log` the command above gets `/LOG=C:\logs\install.text-service.log`. Without `/LOG` it becomes no argument at all. It is expanded when the command runs, so a `before_uninstall` or `after_uninstall` command passes on the uninstaller's log. `NAME` is letters, digits, `-`, `_` and `.`; `#{log_arg:…}` can't be used anywhere else, or inside a longer argument.

### Shortcuts

A `[[shortcuts]]` entry puts a `.lnk` in the Start Menu, on the desktop or in the Startup folder (`location = "start_menu"`, `"desktop"` or `"startup"`), in its `subfolder` if it has one, which is created if missing and removed on uninstall once empty:

```toml
[[shortcuts]]
name = "My Application"
target = "#{app}/bin/myapp.exe"
location = "start_menu"
subfolder = "#{package.name}"
# scope = "current_user"
```

As with Inno Setup, the folder is all users' when the package has `privileges = "admin"` (`#{commonstartmenu}`, `#{commondesktop}`, `#{commonstartup}`), and the current user's otherwise (`#{startmenu}`, `#{desktop}`, `#{startup}`). `scope = "all_users"` or `scope = "current_user"` overrides that for one shortcut; `all_users` needs an elevated install, so a package with `privileges = "user"` can't use it. The current user is the account the installer runs as: an install elevated with another administrator's credentials puts a `current_user` shortcut in that administrator's Start Menu, not in the signed-in user's.

Uninstall removes each shortcut from where it was created. An upgrade removes the ones the installed version created that the new version doesn't create again, such as one that moved to another scope, and the folders created for them once they are empty.

## Configuration sections

| Section | Purpose |
|---|---|
| `[package]` | id, name, version, publisher, default_dir, privileges, architecture (`x86`, `x64`, `arm64`, `any`), `legacy_inno_app_id` and `legacy_inno_mode` (`uninstall` or `adopt`) |
| `[[files]]` | Copy files/globs into `dest`, per-file `overwrite` policy |
| `[[dirs]]` | Create directories (optionally preserved on uninstall) |
| `[[registry]]` | HKLM/HKCU/HKCR keys and values |
| `[[shortcuts]]` | Start Menu, desktop or Startup shortcuts, for all users or the current user (see above) |
| `[[environment]]` | System or user environment variables (`set` / `append` / `prepend`) |
| `[[services]]` | Windows service install/start |
| `[[associations]]` | File-type associations |
| `[[com]]` | COM server registration with regsvr32; `host = "x86"` uses SysWOW64's, `arch` limits it to one architecture |
| `[[fonts]]` | Font installation |
| `[[prerequisites]]` | Detect and require prior installs |
| `[[run]]` | Run commands in `before_install` / `after_install` / `before_uninstall` / `after_uninstall` phases; `#{log_arg:NAME}` passes `/LOG` on (see above) |
| `[[components]]` | Optional/required component groups; each entry may set `component = "foo"` |
| `[upgrade]` | `policy = "overwrite" \| "side_by_side" \| "fail"`; `downgrade = "refuse"` with an optional `[upgrade.installed_version]` registry probe; `prune`, directories emptied of everything the install did not record |
| `[uninstall]` | Display icon, whether to remove `#{app}` on uninstall, extra dirs; `[[uninstall.refuse_while]]` registry keys (with `*` segments) that make the uninstaller refuse, exiting 2 |
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

`outto --version`, `outto-gui --version` and `outto-uninstall --version` print the build's version, which is also logged at start-up and is the ProductVersion of the Windows binaries (their FileVersion stays numeric). It is the crate version unless the build sets `OUTTO_BUILD_VERSION`, as CI does (`0.1.0-dev.20261020T143317Z+build.12`); that has to be semver with the crate's `major.minor.patch`.

Admin-scoped integration tests must be run from an elevated shell:

```
./scripts/test-admin.ps1
```

## License

MIT.
