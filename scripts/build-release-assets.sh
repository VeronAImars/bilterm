#!/usr/bin/env bash
# ==============================================================================
# Bilterm — Linux release binary builder & packager (run on a Linux host)
# ==============================================================================
# Cross-compiles (cargo-zigbuild) and packages the Linux release archives:
#   - x86_64-unknown-linux-gnu      (glibc >= 2.28)
#   - x86_64-unknown-linux-musl     (static Linux x86_64)
#   - aarch64-unknown-linux-musl    (static Linux ARM64)
#
# macOS and Windows archives are NOT built here: macOS needs the Apple SDK and
# Windows ships native MSVC builds. Both come from the `release-binaries`
# workflow (scripts/build-release-mac-win.sh).
#
# Usage:
#   ./scripts/build-release-assets.sh [vX.Y.Z] [--upload] [--no-build] [--dos] [--dry-run]
#
#   vX.Y.Z      Release tag (default: v<version in Cargo.toml>). Checked out clean
#               in a temporary git worktree so local changes never leak in.
#   --upload    Attach the Linux archives to the existing GitHub release and
#               merge their hashes into the release's SHA256SUMS. Run it AFTER
#               the release-binaries workflow finishes: both rewrite SHA256SUMS.
#   --no-build  Skip building; checksum/upload archives already in the dist dir.
#
#   --dos       DOS-mode variant: adds the `dos` cargo feature (appended to
#               $BILTERM_FEATURES; BILTERM_FEATURES=dos is equivalent), names archives
#               bilterm-<tag>-dos-<triple>.tar.gz and SKIPS the unversioned
#               `bilterm-<triple>` aliases so `latest` assets are never replaced.
#   --dry-run   Print the cargo commands and archive names; build nothing.
#
#   CARGO_TARGET_DIR is honoured (keeps the build cache between runs).
#
# Output: target/release-dist/<tag>/ with versioned (`bilterm-<tag>-<triple>.tar.gz`)
# and unversioned installer aliases (`bilterm-<triple>.tar.gz`) plus SHA256SUMS.
# ==============================================================================

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=lib/release-dos.sh
source "$REPO_ROOT/scripts/lib/release-dos.sh"
REPO="${BILTERM_GITHUB_REPO:-bsommers/bilterm}"
TAG=""; UPLOAD=0; BUILD=1; DRY=0
for a in "$@"; do
    case "$a" in
        --upload) UPLOAD=1 ;;
        --no-build) BUILD=0 ;;
        --dos) ;;
        --dry-run) DRY=1 ;;
        v*) TAG="$a" ;;
        *) echo "unknown argument: $a" >&2; exit 2 ;;
    esac
done
release_dos_init "$@"
TAG="${TAG:-v$(awk -F'"' '/^version = / {print $2; exit}' "$REPO_ROOT/Cargo.toml")}"
DIST="$REPO_ROOT/target/release-dist/$TAG"

# Rust triple -> cargo-zigbuild target (glibc floor pinned for the gnu build).
# BILTERM_RELEASE_TRIPLES (space separated) narrows the set, e.g. for a one-target DOS build.
read -r -a TRIPLES <<<"${BILTERM_RELEASE_TRIPLES:-x86_64-unknown-linux-gnu x86_64-unknown-linux-musl aarch64-unknown-linux-musl}"
zig_target() { if [[ "$1" == x86_64-unknown-linux-gnu ]]; then echo "$1.2.28"; else echo "$1"; fi; }

# Archive selection: DOS runs only touch/ship `-dos-` archives, normal runs never do.
if [[ $RELEASE_DOS -eq 1 ]]; then
    ARCH_GLOB='bilterm-*-dos-*linux*.tar.gz'; SUMS=SHA256SUMS.linux-dos
else
    ARCH_GLOB='bilterm-*linux*.tar.gz'; SUMS=SHA256SUMS.linux
fi
archive_is_mine() { [[ "$1" == *-dos-* ]] && [[ $RELEASE_DOS -eq 1 ]] || { [[ "$1" != *-dos-* ]] && [[ $RELEASE_DOS -eq 0 ]]; }; }

if [[ $DRY -eq 1 ]]; then
    for triple in "${TRIPLES[@]}"; do
        echo "cargo zigbuild --release --target $(zig_target "$triple") $(release_cargo_feature_args)"
        echo "  archive: $(release_archive_name "$TAG" "$triple").tar.gz"
        alias_n="$(release_alias_name "$triple")"
        if [[ -n "$alias_n" ]]; then echo "  alias:   $alias_n.tar.gz"; else echo "  alias:   (skipped for DOS)"; fi
    done
    exit 0
fi

if [[ $BUILD -eq 1 ]]; then
    [[ "$(uname -s)" == Linux ]] || { echo "error: run this on Linux (macOS/Windows: release-binaries workflow)" >&2; exit 1; }

    # zig is the cross-linker for cargo-zigbuild. Resolution: PATH, then $ZIG_DIR, then ~/.local/bin.
    if ! command -v zig >/dev/null 2>&1; then
        for candidate_zig in "${ZIG_DIR:-}" "$HOME/.local/bin"; do
            if [[ -n "$candidate_zig" && -x "${candidate_zig}/zig" ]]; then
                export PATH="${candidate_zig}:$PATH"
                break
            fi
        done
    fi
    command -v zig >/dev/null 2>&1 || {
        echo "error: zig not found; install it on PATH or set ZIG_DIR=<dir containing zig>" >&2
        exit 1
    }
    command -v cargo-zigbuild >/dev/null 2>&1 || cargo install --locked cargo-zigbuild

    WT="$(mktemp -d)/src"
    trap 'git -C "$REPO_ROOT" worktree remove --force "$WT" 2>/dev/null || true' EXIT
    git -C "$REPO_ROOT" fetch --tags --quiet
    git -C "$REPO_ROOT" worktree add --detach "$WT" "$TAG" >/dev/null
    TGT="${CARGO_TARGET_DIR:-$WT/target}"

    rustup target add "${TRIPLES[@]}"
    # Clear only our own (Linux) outputs: macOS/Windows archives built into the
    # same dist dir by build-windows-vm.sh / build-release-mac-win.sh must survive.
    mkdir -p "$DIST"
    rm -rf "$DIST/prev" "${DIST:?}/$SUMS"
    for old in "$DIST"/$ARCH_GLOB; do
        [[ -e "$old" ]] && archive_is_mine "$(basename "$old")" && rm -f "$old"
    done
    for triple in "${TRIPLES[@]}"; do
        echo "[bilterm-release] Building ${triple}..."
        (cd "$WT" && cargo zigbuild --release --target "$(zig_target "$triple")" ${RELEASE_FEATURES:+--features "$RELEASE_FEATURES"})
        name="$(release_archive_name "$TAG" "$triple")"
        mkdir -p "$DIST/$name"
        cp "$TGT/$triple/release/bilterm" "$DIST/$name/bilterm"
        chmod 0755 "$DIST/$name/bilterm"
        cp "$WT"/{README.md,LICENSE,CHANGELOG.md} "$DIST/$name/"
        tar -czf "$DIST/$name.tar.gz" -C "$DIST" "$name"
        alias_n="$(release_alias_name "$triple")"
        if [[ -n "$alias_n" ]]; then cp "$DIST/$name.tar.gz" "$DIST/$alias_n.tar.gz"; fi
        rm -rf "${DIST:?}/$name"
    done
fi

[[ -d "$DIST" ]] || { echo "error: $DIST does not exist (build first)" >&2; exit 1; }
new=$(cd "$DIST" && shopt -s nullglob && for f in $ARCH_GLOB; do archive_is_mine "$f" && echo "$f"; done)
[[ -n "$new" ]] || { echo "error: no Linux archives in $DIST" >&2; exit 1; }
# shellcheck disable=SC2086 # $new is a newline-separated list of plain file names
(cd "$DIST" && sha256sum $new > "$SUMS")

if [[ $UPLOAD -eq 1 ]]; then
    # Merge with the checksums already on the release: drop stale lines for our
    # files (exact name match; '*' binary markers normalized), then append ours.
    rm -rf "$DIST/prev"
    gh release download "$TAG" -R "$REPO" -p SHA256SUMS -D "$DIST/prev" 2>/dev/null || true
    {
        if [[ -f "$DIST/prev/SHA256SUMS" ]]; then
            awk 'NR==FNR { mine[$2]=1; next } { f=$2; sub(/^\*/, "", f); if (!(f in mine)) print $1 "  " f }' \
                "$DIST/$SUMS" "$DIST/prev/SHA256SUMS"
        fi
        cat "$DIST/$SUMS"
    } | sort -k2 > "$DIST/SHA256SUMS"
    # shellcheck disable=SC2086
    (cd "$DIST" && gh release upload "$TAG" -R "$REPO" --clobber $new SHA256SUMS)
    echo "Uploaded to https://github.com/$REPO/releases/tag/$TAG"
else
    # DOS runs leave the normal SHA256SUMS alone; the merge happens at --upload.
    [[ $RELEASE_DOS -eq 1 ]] || cp "$DIST/$SUMS" "$DIST/SHA256SUMS"
    echo "Built in $DIST (re-run with --upload --no-build to attach to the release)"
fi
ls -lh "$DIST"
