//! Compiles in the version outto reports as `OUTTO_VERSION`: CI's full build
//! version from `OUTTO_BUILD_VERSION` (`0.1.0-dev.20261020T143317Z+build.12`),
//! or the crate version for any other build.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=OUTTO_BUILD_VERSION");

    let crate_version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let version = match std::env::var("OUTTO_BUILD_VERSION") {
        Ok(version) if !version.is_empty() => {
            check_build_version(&version, &crate_version);
            version
        }
        _ => crate_version,
    };
    println!("cargo:rustc-env=OUTTO_VERSION={version}");
}

/// The build version has to be semver for the crate's own release: the
/// Windows binaries' numeric FileVersion is the crate version's.
fn check_build_version(version: &str, crate_version: &str) {
    let parsed = semver::Version::parse(version)
        .unwrap_or_else(|e| panic!("OUTTO_BUILD_VERSION {version:?} is not semver: {e}"));
    let crate_version = semver::Version::parse(crate_version).unwrap();
    if (parsed.major, parsed.minor, parsed.patch)
        != (
            crate_version.major,
            crate_version.minor,
            crate_version.patch,
        )
    {
        panic!(
            "OUTTO_BUILD_VERSION {version:?} is not a build of the crate version {crate_version}"
        );
    }
}
