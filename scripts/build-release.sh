#!/usr/bin/env bash
# macOS release build — mirror of build-release.ps1.
#
# Produces target/release/outto-setup.app:
#   1. cargo build --release --workspace (nightly + build-std, panic=immediate-abort)
#   2. Stage bin/libexec layout under target/installer-stage/
#   3. Invoke `outto build --config outto.macos.toml --source <stage> --output outto-setup.app`
#      which assembles the signed .app bundle (SFX → inner installer → payload)
#   4. Optionally notarize + staple via xcrun notarytool
#
# Usage:
#   scripts/build-release.sh                        # unsigned, no notarization
#   scripts/build-release.sh --sign "<codesign cmd>"
#   scripts/build-release.sh --sign "..." --notarize --keychain-profile <profile-name>
#
# Bootstrap .pkg (system-scope installs / MDM) instead of an .app:
#   scripts/build-release.sh --pkg --pkg-sign "Developer ID Installer: XYZ (TEAMID)" \
#       --sign "codesign --sign 'Developer ID Application: XYZ (TEAMID)' --options runtime #{file}"
#   (a notarizable .pkg needs BOTH: --sign signs the inner app's Mach-O with the
#    Developer ID Application cert + hardened runtime, --pkg-sign signs the .pkg
#    with the Developer ID Installer cert.)
#
# A .pkg is for one architecture, never universal: that of the binaries it is
# built from. For one per architecture, build once per target:
#   scripts/build-release.sh --target aarch64-apple-darwin --pkg ...
#   scripts/build-release.sh --target x86_64-apple-darwin --pkg ...
# giving target/<triple>/release/outto-setup.{app,pkg} for each. Without
# --target it builds for this Mac's architecture into target/release/.
# The installer then starts on Rust's default minimum macOS for the target
# (11.0 for arm64, 10.12 for x86_64), which outto writes into its Info.plist
# as LSMinimumSystemVersion; a MACOSX_DEPLOYMENT_TARGET in the environment
# raises it, so leave it unset for packages meant for older Macs.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

SIGN_CMD=""
NOTARIZE=false
KEYCHAIN_PROFILE=""
COMPRESSION_LEVEL=19
PKG=false
PKG_SIGN=""
TARGET=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --sign) SIGN_CMD="$2"; shift 2 ;;
        --notarize) NOTARIZE=true; shift ;;
        --keychain-profile) KEYCHAIN_PROFILE="$2"; shift 2 ;;
        --compression-level) COMPRESSION_LEVEL="$2"; shift 2 ;;
        --pkg) PKG=true; shift ;;
        --pkg-sign) PKG_SIGN="$2"; shift 2 ;;
        --target) TARGET="$2"; shift 2 ;;
        *) echo "Unknown argument: $1" >&2; exit 2 ;;
    esac
done

if $NOTARIZE && [[ -z "$KEYCHAIN_PROFILE" ]]; then
    echo "--notarize requires --keychain-profile <name>" >&2
    echo "  (create with: xcrun notarytool store-credentials <name> --apple-id ... --team-id ... --password ...)" >&2
    exit 2
fi

cd "$ROOT"

RELEASE="$ROOT/target/release"
if [[ -n "$TARGET" ]]; then
    RELEASE="$ROOT/target/$TARGET/release"
fi

echo "==> cargo build --release ${TARGET:+--target $TARGET }(nightly + build-std)"
cargo +nightly build --release --workspace ${TARGET:+--target "$TARGET"} -Zbuild-std=std \
    -Zpanic-immediate-abort --config 'profile.release.panic="immediate-abort"'

# The packaging runs on this Mac, so it needs an outto built for it, which
# then packs the target's binaries from the libexec beside it.
OUTTO="$RELEASE/outto"
if [[ -n "$TARGET" ]]; then
    echo "==> cargo build --release -p outto-cli (for this Mac, to package with)"
    cargo +nightly build --release -p outto-cli
    OUTTO="$ROOT/target/release/outto"
fi

STAGE="$ROOT/target/installer-stage"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/libexec"

cp "$RELEASE/outto" "$STAGE/bin/outto"
cp "$RELEASE/outto-gui" "$STAGE/libexec/outto-gui"
cp "$RELEASE/outto-uninstall" "$STAGE/libexec/outto-uninstall"
cp "$RELEASE/outto-sfx-macos" "$STAGE/libexec/outto-sfx-macos"

echo "==> Staged layout:"
(cd "$STAGE" && find . -type f -exec ls -la {} \;)

# --pkg is additive: the .app is always produced; the .pkg (which embeds the
# same inner installer app) is written alongside it at outto-setup.pkg.
OUTPUT="$RELEASE/outto-setup.app"
PKG_OUTPUT="$RELEASE/outto-setup.pkg"
rm -rf "$OUTPUT"
$PKG && rm -f "$PKG_OUTPUT"

BUILD_ARGS=(
    build
    --config "$ROOT/outto.macos.toml"
    --source "$STAGE"
    --output "$OUTPUT"
    --compress
    --compression-level "$COMPRESSION_LEVEL"
)
if $PKG; then
    BUILD_ARGS+=(--pkg)
    if [[ -n "$PKG_SIGN" ]]; then
        BUILD_ARGS+=(--pkg-sign "$PKG_SIGN")
    fi
fi
if [[ -n "$SIGN_CMD" ]]; then
    BUILD_ARGS+=(--sign "$SIGN_CMD")
fi

TOOLS="$ROOT/target/installer-tools"
rm -rf "$TOOLS"
mkdir -p "$TOOLS/bin"
cp "$OUTTO" "$TOOLS/bin/outto"
ln -s "$STAGE/libexec" "$TOOLS/libexec"

echo "==> Packaging installer..."
"$TOOLS/bin/outto" "${BUILD_ARGS[@]}"

if $NOTARIZE; then
    echo "==> Submitting .app to Apple notarytool..."
    # notarytool requires a .zip or .dmg, not a raw .app. Zip it.
    ZIP="$RELEASE/outto-setup.zip"
    rm -f "$ZIP"
    (cd "$(dirname "$OUTPUT")" && ditto -c -k --keepParent "$(basename "$OUTPUT")" "$ZIP")

    xcrun notarytool submit "$ZIP" \
        --keychain-profile "$KEYCHAIN_PROFILE" \
        --wait

    echo "==> Stapling .app..."
    xcrun stapler staple "$OUTPUT"

    echo "==> Verifying .app..."
    codesign --verify --deep --strict "$OUTPUT"
    spctl --assess --type exec --verbose "$OUTPUT" || true

    if $PKG; then
        echo "==> Submitting .pkg to Apple notarytool..."
        # A .pkg is itself a valid notarization container — submit directly.
        xcrun notarytool submit "$PKG_OUTPUT" \
            --keychain-profile "$KEYCHAIN_PROFILE" \
            --wait

        echo "==> Stapling .pkg..."
        xcrun stapler staple "$PKG_OUTPUT"

        echo "==> Verifying .pkg..."
        pkgutil --check-signature "$PKG_OUTPUT"
        spctl --assess --type install --verbose "$PKG_OUTPUT" || true
    fi
fi

echo "==> Done: $OUTPUT"
du -sh "$OUTPUT"
if $PKG; then
    echo "==> Done: $PKG_OUTPUT"
    du -sh "$PKG_OUTPUT"
fi
