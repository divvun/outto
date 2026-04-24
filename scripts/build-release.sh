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

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

SIGN_CMD=""
NOTARIZE=false
KEYCHAIN_PROFILE=""
COMPRESSION_LEVEL=19

while [[ $# -gt 0 ]]; do
    case "$1" in
        --sign) SIGN_CMD="$2"; shift 2 ;;
        --notarize) NOTARIZE=true; shift ;;
        --keychain-profile) KEYCHAIN_PROFILE="$2"; shift 2 ;;
        --compression-level) COMPRESSION_LEVEL="$2"; shift 2 ;;
        *) echo "Unknown argument: $1" >&2; exit 2 ;;
    esac
done

if $NOTARIZE && [[ -z "$KEYCHAIN_PROFILE" ]]; then
    echo "--notarize requires --keychain-profile <name>" >&2
    echo "  (create with: xcrun notarytool store-credentials <name> --apple-id ... --team-id ... --password ...)" >&2
    exit 2
fi

cd "$ROOT"

echo "==> cargo build --release (nightly + build-std)"
cargo +nightly build --release --workspace -Zbuild-std=std

STAGE="$ROOT/target/installer-stage"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/libexec"

RELEASE="$ROOT/target/release"
cp "$RELEASE/outto" "$STAGE/bin/outto"
cp "$RELEASE/outto-gui" "$STAGE/libexec/outto-gui"
cp "$RELEASE/outto-uninstall" "$STAGE/libexec/outto-uninstall"
cp "$RELEASE/outto-sfx-macos" "$STAGE/libexec/outto-sfx-macos"

echo "==> Staged layout:"
(cd "$STAGE" && find . -type f -exec ls -la {} \;)

OUTPUT="$RELEASE/outto-setup.app"
rm -rf "$OUTPUT"

BUILD_ARGS=(
    build
    --config "$ROOT/outto.macos.toml"
    --source "$STAGE"
    --output "$OUTPUT"
    --compress
    --compression-level "$COMPRESSION_LEVEL"
)
if [[ -n "$SIGN_CMD" ]]; then
    BUILD_ARGS+=(--sign "$SIGN_CMD")
fi

echo "==> Packaging installer..."
"$STAGE/bin/outto" "${BUILD_ARGS[@]}"

if $NOTARIZE; then
    echo "==> Submitting to Apple notarytool..."
    # notarytool requires a .zip or .dmg, not a raw .app. Zip it.
    ZIP="$RELEASE/outto-setup.zip"
    rm -f "$ZIP"
    (cd "$(dirname "$OUTPUT")" && ditto -c -k --keepParent "$(basename "$OUTPUT")" "$ZIP")

    xcrun notarytool submit "$ZIP" \
        --keychain-profile "$KEYCHAIN_PROFILE" \
        --wait

    echo "==> Stapling..."
    xcrun stapler staple "$OUTPUT"

    echo "==> Verifying..."
    codesign --verify --deep --strict "$OUTPUT"
    spctl --assess --type exec --verbose "$OUTPUT" || true
fi

echo "==> Done: $OUTPUT"
du -sh "$OUTPUT"
