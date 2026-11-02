//! `--pkg`: wrap the inner installer `.app` in a bootstrap `.pkg`, for
//! system-scope installs by `installer -pkg`, MDM, or an older updater.
//!
//! The package installs nothing itself. Its payload is empty, there only so
//! that installd writes a receipt under the package's identifier and
//! version; `installer.app` travels in the Scripts archive beside the
//! `postinstall` that runs it. installd extracts that archive into a
//! directory of its own, root's and randomly named, so no local user can
//! put another program where the script looks, as they could at a fixed
//! path in `/private/tmp`; and since the payload holds no bundle, installd
//! has nothing to relocate to wherever LaunchServices last saw one.
//!
//! A package is for one architecture, that of the outto build it is made
//! from, and its distribution says so.

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use outto_macos::macho::{self, Cpu, MacosVersion, Slice};

pub struct PkgSpec<'a> {
    pub identifier: &'a str,
    pub version: &'a str,
    /// Shown by Installer.app.
    pub title: &'a str,
    pub cpu: Cpu,
}

/// Runs the installer headlessly as root (installd runs package scripts as
/// root, so outto skips elevation and installs at system scope).
///
/// outto exits 3010 for "installed, restart to finish", which a Unix exit
/// status, keeping only its low 8 bits, turns into 194. That is a success:
/// `--no-restart` leaves the restart to whoever runs the package. Any other
/// failure fails the package, so installd writes no receipt.
pub const POSTINSTALL: &str = r#"#!/bin/sh
# outto bootstrap: run the installer.app that installd extracted beside this
# script, in its own private directory, as root.
if [ "$3" != "/" ]; then
    echo "outto: this package installs on the running system only, not on $3" >&2
    exit 1
fi
"$(dirname "$0")/installer.app/Contents/MacOS/installer" --very-silent --suppress-msgboxes --no-restart
status=$?
case "$status" in
    0) ;;
    194) echo "outto: installed; a restart is needed to finish" >&2 ;;
    *) echo "outto: install failed with exit status $status" >&2
       exit "$status" ;;
esac
exit 0
"#;

/// Lists no bundle, so pkgbuild treats nothing in the payload as one and
/// installd relocates nothing. pkgbuild would infer the same from the empty
/// root; saying it keeps that true if the root ever gains a bundle.
const COMPONENT_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<array/>
</plist>
"#;

/// The single architecture a `--pkg` build is for: that of the installer
/// template, which must be thin, and which the uninstaller must share. The
/// minimum macOS is the later of the two binaries'.
pub fn pkg_slice(gui: &Path, uninstall: &Path) -> Result<Slice, Box<dyn Error>> {
    let thin = |path: &Path| -> Result<Slice, String> {
        let slices = macho::slices(path).map_err(|e| format!("{}: {e}", path.display()))?;
        match slices[..] {
            [slice] => Ok(slice),
            _ => Err(format!(
                "{} is universal ({}); --pkg builds one package per architecture, from an outto built for that architecture alone (cargo build --target <triple>)",
                path.display(),
                slices
                    .iter()
                    .map(|s| s.cpu.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    };
    let gui_slice = thin(gui)?;
    let uninstall_slice = thin(uninstall)?;
    if let Cpu::Other(_) = gui_slice.cpu {
        return Err(format!("{} is for {}", gui.display(), gui_slice.cpu).into());
    }
    if gui_slice.cpu != uninstall_slice.cpu {
        return Err(format!(
            "{} is for {} but {} is for {}",
            gui.display(),
            gui_slice.cpu,
            uninstall.display(),
            uninstall_slice.cpu
        )
        .into());
    }
    Ok(Slice {
        cpu: gui_slice.cpu,
        min_macos: gui_slice.min_macos.max(uninstall_slice.min_macos),
    })
}

/// Wrap a self-contained installer `.app` in a bootstrap `.pkg` at `output`.
pub fn build_pkg(
    inner_app: &Path,
    output: &Path,
    spec: &PkgSpec,
    min_macos: Option<MacosVersion>,
    sign_identity: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    let scratch = tempfile::tempdir()?;
    let layout = Layout::new(scratch.path(), spec.identifier);
    // The payload's one entry, ".", lands on "/" as root:wheel with this
    // mode, which "/" already has.
    std::fs::create_dir_all(&layout.root)?;
    std::fs::set_permissions(&layout.root, std::fs::Permissions::from_mode(0o755))?;
    std::fs::create_dir_all(&layout.scripts)?;
    std::fs::create_dir_all(&layout.packages)?;

    eprintln!("Staging installer.app into the pkg's scripts...");
    super::ditto(inner_app, &layout.scripts.join("installer.app"))?;
    let postinstall = layout.scripts.join("postinstall");
    std::fs::write(&postinstall, POSTINSTALL)?;
    std::fs::set_permissions(&postinstall, std::fs::Permissions::from_mode(0o755))?;
    std::fs::write(&layout.component_plist, COMPONENT_PLIST)?;
    std::fs::write(
        &layout.distribution,
        distribution(spec, &layout.component_name),
    )?;

    eprintln!("Running pkgbuild...");
    run(Command::new("pkgbuild").args(pkgbuild_args(&layout, spec)))?;

    // productbuild refuses to overwrite, so clear any stale output first
    // (file or a leftover .app directory).
    if output.is_dir() {
        std::fs::remove_dir_all(output).ok();
    } else if output.exists() {
        std::fs::remove_file(output).ok();
    }
    eprintln!("Running productbuild...");
    run(Command::new("productbuild").args(productbuild_args(&layout, sign_identity, output)))?;

    let final_size = std::fs::metadata(output)?.len();
    eprintln!(
        "Done! {} ({:.1} MB): {} {} for {}, installing on macOS {} and later",
        output.display(),
        final_size as f64 / 1_048_576.0,
        spec.identifier,
        spec.version,
        spec.cpu,
        min_macos.map_or_else(|| "(any)".to_string(), |v| v.to_string()),
    );
    Ok(())
}

/// Where `build_pkg` puts things in its scratch directory.
struct Layout {
    root: PathBuf,
    scripts: PathBuf,
    component_plist: PathBuf,
    packages: PathBuf,
    component_name: String,
    distribution: PathBuf,
}

impl Layout {
    fn new(scratch: &Path, identifier: &str) -> Self {
        Self {
            root: scratch.join("root"),
            scripts: scratch.join("scripts"),
            component_plist: scratch.join("component.plist"),
            packages: scratch.join("packages"),
            component_name: format!("{identifier}.pkg"),
            distribution: scratch.join("distribution.xml"),
        }
    }
}

fn pkgbuild_args(layout: &Layout, spec: &PkgSpec) -> Vec<OsString> {
    let component = layout.packages.join(&layout.component_name);
    [
        OsStr::new("--root"),
        layout.root.as_os_str(),
        OsStr::new("--component-plist"),
        layout.component_plist.as_os_str(),
        OsStr::new("--install-location"),
        OsStr::new("/"),
        OsStr::new("--ownership"),
        OsStr::new("recommended"),
        OsStr::new("--scripts"),
        layout.scripts.as_os_str(),
        OsStr::new("--identifier"),
        OsStr::new(spec.identifier),
        OsStr::new("--version"),
        OsStr::new(spec.version),
        component.as_os_str(),
    ]
    .map(OsStr::to_owned)
    .to_vec()
}

fn productbuild_args(layout: &Layout, sign_identity: Option<&str>, output: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "--distribution".into(),
        layout.distribution.clone().into(),
        "--package-path".into(),
        layout.packages.clone().into(),
    ];
    if let Some(identity) = sign_identity {
        args.push("--sign".into());
        args.push(identity.into());
    }
    args.push(output.into());
    args
}

/// The distribution `productbuild --synthesize` writes for one component,
/// with a title and with `hostArchitectures` naming only the package's
/// architecture, where the synthesized one claims both.
fn distribution(spec: &PkgSpec, component_name: &str) -> String {
    let id = xml_escape(spec.identifier);
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="1">
    <title>{title}</title>
    <pkg-ref id="{id}"/>
    <options customize="never" require-scripts="false" hostArchitectures="{cpu}"/>
    <choices-outline>
        <line choice="default">
            <line choice="{id}"/>
        </line>
    </choices-outline>
    <choice id="default"/>
    <choice id="{id}" visible="false">
        <pkg-ref id="{id}"/>
    </choice>
    <pkg-ref id="{id}" version="{version}" onConclusion="none">{file}</pkg-ref>
</installer-gui-script>
"#,
        title = xml_escape(spec.title),
        cpu = spec.cpu,
        version = xml_escape(spec.version),
        file = xml_escape(component_name),
    )
}

pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

fn run(command: &mut Command) -> Result<(), Box<dyn Error>> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command.status()?;
    if !status.success() {
        return Err(format!("{program} failed ({status})").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(cpu: Cpu) -> PkgSpec<'static> {
        PkgSpec {
            identifier: "no.divvun.pahkat",
            version: "3.0.0",
            title: "Pahkat & co",
            cpu,
        }
    }

    #[test]
    fn distribution_names_one_architecture() {
        for (cpu, name) in [(Cpu::Arm64, "arm64"), (Cpu::X86_64, "x86_64")] {
            let xml = distribution(&spec(cpu), "no.divvun.pahkat.pkg");
            assert!(
                xml.contains(&format!(r#"hostArchitectures="{name}""#)),
                "{xml}"
            );
            assert!(xml.contains("<title>Pahkat &amp; co</title>"), "{xml}");
            assert!(
                xml.contains(r#"<pkg-ref id="no.divvun.pahkat" version="3.0.0" onConclusion="none">no.divvun.pahkat.pkg</pkg-ref>"#),
                "{xml}"
            );
        }
    }

    #[test]
    fn pkgbuild_gets_an_empty_root_scripts_and_no_relocation() {
        let layout = Layout::new(Path::new("/s"), "no.divvun.pahkat");
        assert_eq!(
            pkgbuild_args(&layout, &spec(Cpu::Arm64)),
            [
                "--root",
                "/s/root",
                "--component-plist",
                "/s/component.plist",
                "--install-location",
                "/",
                "--ownership",
                "recommended",
                "--scripts",
                "/s/scripts",
                "--identifier",
                "no.divvun.pahkat",
                "--version",
                "3.0.0",
                "/s/packages/no.divvun.pahkat.pkg",
            ]
        );
        let output = Path::new("/out/pahkat.pkg");
        assert_eq!(
            productbuild_args(&layout, None, output),
            [
                "--distribution",
                "/s/distribution.xml",
                "--package-path",
                "/s/packages",
                "/out/pahkat.pkg"
            ]
        );
        let signed = productbuild_args(&layout, Some("Developer ID Installer: X (T)"), output);
        assert_eq!(signed[4..6], ["--sign", "Developer ID Installer: X (T)"]);
    }

    /// Run [`POSTINSTALL`] for target volume `target`, beside a stand-in
    /// installer that records its arguments and exits with `installer_exit`
    /// (or none at all); give the script's exit status and those arguments.
    fn postinstall_with(installer_exit: Option<i32>, target: &str) -> (i32, String) {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let scripts = tmp.path().join("Scripts dir");
        let macos = scripts.join("installer.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let argv = tmp.path().join("argv");
        if let Some(code) = installer_exit {
            let installer = macos.join("installer");
            std::fs::write(
                &installer,
                format!(
                    "#!/bin/sh\necho \"$@\" > '{}'\nexit {code}\n",
                    argv.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&installer, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let script = scripts.join("postinstall");
        std::fs::write(&script, POSTINSTALL).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let status = Command::new(&script)
            .args([&script.to_string_lossy(), "/", target, "/"])
            .current_dir(tmp.path())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        (
            status.code().unwrap(),
            std::fs::read_to_string(argv).unwrap_or_default(),
        )
    }

    #[test]
    fn postinstall_passes_success_and_restart_and_fails_the_rest() {
        let (status, argv) = postinstall_with(Some(0), "/");
        assert_eq!(
            (status, argv.trim()),
            (0, "--very-silent --suppress-msgboxes --no-restart")
        );
        assert_eq!(postinstall_with(Some(3010), "/").0, 0);
        for failed in [1, 2, 3, 4, 5, 255] {
            assert_eq!(postinstall_with(Some(failed), "/").0, failed);
        }
        let (status, argv) = postinstall_with(None, "/");
        assert_ne!(status, 0);
        assert!(argv.is_empty());
        let (status, argv) = postinstall_with(Some(0), "/Volumes/Other");
        assert_eq!(status, 1);
        assert!(argv.is_empty());
    }

    /// The whole package, from a stand-in installer.app, read back with
    /// pkgutil and lsbom: an empty payload for "/", installer.app and the
    /// postinstall in Scripts, nothing relocatable.
    #[test]
    fn builds_a_bootstrap_pkg() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("installer-inner.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
        std::fs::write(app.join("Contents/MacOS/installer"), "stand-in").unwrap();
        std::fs::write(app.join("Contents/Resources/payload.box"), "box").unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            super::super::info_plist(
                "Pahkat",
                "no.divvun.pahkat.installer",
                "installer",
                "3.0.0",
                None,
            ),
        )
        .unwrap();
        let output = tmp.path().join("pahkat.pkg");
        build_pkg(&app, &output, &spec(Cpu::X86_64), None, None).unwrap();

        let expanded = tmp.path().join("expanded");
        assert!(
            Command::new("pkgutil")
                .arg("--expand")
                .arg(&output)
                .arg(&expanded)
                .status()
                .unwrap()
                .success()
        );
        let component = expanded.join("no.divvun.pahkat.pkg");
        let bom = Command::new("lsbom")
            .arg(component.join("Bom"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&bom.stdout), ".\t40755\t0/0\n");

        let info = std::fs::read_to_string(component.join("PackageInfo")).unwrap();
        for expected in [
            r#"relocatable="false""#,
            r#"identifier="no.divvun.pahkat""#,
            r#"version="3.0.0""#,
            r#"install-location="/""#,
            "<relocate/>",
            r#"<postinstall file="./postinstall""#,
        ] {
            assert!(info.contains(expected), "{expected} not in {info}");
        }
        assert!(!info.contains("<bundle "), "{info}");

        let scripts = component.join("Scripts");
        assert_eq!(
            std::fs::read_to_string(scripts.join("postinstall")).unwrap(),
            POSTINSTALL
        );
        assert_eq!(
            std::fs::read_to_string(scripts.join("installer.app/Contents/MacOS/installer"))
                .unwrap(),
            "stand-in"
        );
        assert!(
            scripts
                .join("installer.app/Contents/Resources/payload.box")
                .is_file()
        );

        let distribution = std::fs::read_to_string(expanded.join("Distribution")).unwrap();
        assert!(
            distribution.contains(r#"hostArchitectures="x86_64""#),
            "{distribution}"
        );
    }
}
