# Build hosts and local release builds

Hosted GitHub runners are a last resort (`workflow_dispatch` only). The maintainer's machines can build every release target locally.

| Target | Host | Command |
|---|---|---|
| Linux x86_64 gnu/musl, aarch64 musl | Linux workstation | `scripts/build-release-assets.sh <tag> [--upload] [--dos]` |
| Windows x64 | Linux workstation, libvirt Windows 11 VM (`win-devvm`) | `WIN_DEVVM_DIR=~/src/infra/win-devvm scripts/build-windows-vm.sh <tag> --arch x64 --test` |
| Windows ARM64 (native build + smoke test) | Mac over SSH, Parallels `win-devvm` (Windows 11 ARM64) | see below |
| macOS Apple Silicon and Intel | Mac over SSH (`cargo` with `aarch64-apple-darwin` and `x86_64-apple-darwin` targets) | see below |

The Linux x64 VM can cross-build Windows ARM64 but cannot run it, and the cross link currently fails there (Git's `link` shadows MSVC `link.exe`). Use the Mac for ARM64.

## Mac over SSH

Host: `ssh bill@bigmac.local` (the bare name `bigmac` does not resolve). Non-interactive shells need `PATH=$PATH:/usr/local/bin:/opt/homebrew/bin`. win-devvm lives at `~/src/tools/win-devvm` on the Mac.

Do not write to the Mac's own checkout; use a worktree:

```bash
ssh bill@bigmac.local 'cd ~/src/ai/bilterm && git fetch -q origin && git worktree add --detach ~/worktrees/bilterm-winarm origin/main && cd ~/worktrees/bilterm-winarm && git tag v-local-arm'
ssh bill@bigmac.local 'cd ~/src/tools/win-devvm && ./vm.sh start; WAIT_TIMEOUT=600 ./vm.sh wait'
ssh bill@bigmac.local 'export PATH=$PATH:/usr/local/bin; cd ~/worktrees/bilterm-winarm && WIN_DEVVM_DIR=$HOME/src/tools/win-devvm ./scripts/build-windows-vm.sh v-local-arm --arch arm64 --test'
scp bill@bigmac.local:worktrees/bilterm-winarm/target/release-dist/v-local-arm/bilterm-aarch64-pc-windows-msvc.zip .
```

Then delete the local tag (never push it) and `./vm.sh stop`. `build-windows-vm.sh` needs the tag to exist locally and builds the worktree's committed HEAD. macOS archives are built on the Mac with `cargo build --release --target <triple>` and packaged like `.github/workflows/release-binaries.yml`.

Verified 2026-11-11 on main `79407bb`: Windows x64 and ARM64 each pass `scripts/windows-smoke-test.ps1` (headless daemon included).

The Linux script clears only its own outputs, so archives from the Windows and macOS scripts can share one `target/release-dist/<tag>/` directory in any order.

## DOS-mode builds

Pass `--dos` (or set `BILTERM_FEATURES=dos`) to `build-release-assets.sh`, `build-release-mac-win.sh` or `build-windows-vm.sh` to build with the `dos` feature. Archives are named `bilterm-<tag>-dos-<triple>` and the unversioned `bilterm-<triple>` aliases are skipped, so installers' `latest` assets stay non-DOS. `--dry-run` on `build-release-assets.sh` prints the cargo commands and archive names without building. Tested by `scripts/tests/release_dos_names_test.sh`.

## DOS-mode release archives

Add `--dos` (or `BILTERM_FEATURES=dos`; extra features in that variable are kept and `dos` is appended) to `build-release-assets.sh`, `build-release-mac-win.sh` or `build-windows-vm.sh`. The build adds the `dos` cargo feature, names archives `bilterm-<tag>-dos-<triple>.tar.gz|zip`, and deliberately skips the unversioned `bilterm-<triple>.*` aliases so a DOS archive never replaces the normal `latest` assets. Checksums for DOS runs go to `SHA256SUMS.linux-dos` locally and are merged into the release `SHA256SUMS` on `--upload`; normal and DOS runs never pick up each other's archives. `--dry-run` prints the cargo command and names without building; `BILTERM_RELEASE_TRIPLES="x86_64-unknown-linux-gnu"` narrows the Linux triple set. Name logic lives in `scripts/lib/release-dos.sh`, tested by `scripts/tests/release_dos_names_test.sh`.
