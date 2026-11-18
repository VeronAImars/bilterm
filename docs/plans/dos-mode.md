# Plan: DOS Mode (FreeDOS in a bilterm pane)

Status: PLAN ONLY (docs, no code). Research date: 2026-11-17. Not legal advice; the owner should confirm the verdicts in section 2 before any bundling.
Baseline: `main` (bilterm 3.3.1, `Cargo.toml` license `Apache-2.0`, repo `LICENSE` is Apache License 2.0).

Conventions: **VERIFIED** = observed first-hand on 2026-11-17 (command run or primary source fetched; source named). **UNVERIFIED** = not confirmed; the PR that depends on it must check. Nothing here is asserted from memory.

## 1. Summary and recommendation

Goal: `:dos` opens a pane running FreeDOS (a "retro PC in a pane"), using an emulator that renders VGA text mode into the pane's PTY so the existing VT parser, themes, recording and RPC all work unchanged.

Headlines:
1. **Licensing verdict: do not bundle by default; download on demand.** FreeDOS 1.4 is a *mix* of licenses, mostly GPL but including freeware-only packages (VERIFIED, section 2.1). Redistribution of the whole ISO inside bilterm releases would make bilterm responsible for per-package notices and source offers. Downloading at runtime from the official host and launching a separate process creates no combination with bilterm's Apache-2.0 code.
2. **Recommended emulator: QEMU (`qemu-system-i386 -display curses`)**. VERIFIED on this machine: FreeDOS 1.4 boots, text appears in a PTY, and keystrokes injected through the PTY reach DOS (section 4.1). Packaged by apt/brew/winget (apt/brew VERIFIED; winget UNVERIFIED).
3. **Fallback: Bochs `display_library: term`** (LGPL-2.1, text-only ncurses front end, documented) for a lighter, easier-to-license option; **dosemu2 `-t`** on Linux only; **DOSBox-X / Staging** for games, as a separate window (no text-mode pane bridge known).
4. **Biggest risk:** the bridge is only text mode. Any graphics-mode program (most games, Windows 3.1) shows nothing useful in a curses pane. Second risk: Windows hosts (curses displays are likely absent from Windows emulator builds; UNVERIFIED).
5. **Plumbing is cheap:** a pinned manifest (URL + SHA256 + size) plus `bilterm dos setup|run|doctor|update|remove`; FreeDOS publishes SHA256 sums, and we verified two zips against them.

## 2. Licensing

### 2.1 FreeDOS (primary-source findings)

| Fact | Evidence |
|---|---|
| FreeDOS 1.4 released 2025-04-05; Live CD 316,557,312 B (301 MB), Bonus CD 411,643,904 B | https://freedos.org/download/announce.html (fetched 2026-11-17) |
| Kernel prints "free software ... GNU General Public License ... version 2, or (at your option) any later version" | VERIFIED: boot banner of FreeDOS kernel 2043 in the 1.4 floppy image, run under QEMU, 2026-11-17 |
| Website *content* aims for CC BY-SA 4.0; "FreeDOS" is a trademark of Jim Hall | https://www.freedos.org/about/ (2026-11-17). Software licenses are NOT stated there. |
| **Per-package licenses in the 1.4 LiveCD**: I unpacked `FD14LIVE.iso` and read the `Copying-policy` field of each package's `.LSM` (190 packages with the field). Counts: GPL (all spellings) ~120, BSD (2/3-clause, 0BSD) ~16, Public Domain/Unlicense ~11, MIT ~8, LGPL 2.1/3 3, MPL-2.0 2, EUPL 2, zlib 1, Open Watcom PL 1, NetHack GPL 1, plus loose "Open source, see license" ~7 | VERIFIED, local scan of the ISO downloaded from https://download.freedos.org/1.4/FD14-LiveCD.zip |
| **Non-OSI / restrictive-looking entries in the same scan**: `doslfn` "Freeware w/sources", `rdisk`/`udvd2`/`uide` "Free w/sources", `usbdos` and `slowdown` "Freeware, see documentation", `shsufdrv` "Freeware / source code available", `cal` "Freeware with source" | VERIFIED (same scan). Terms of each are unread; treat as not-clearly-redistributable until read. |
| Older errata: DEBUG mislabelled GPL (is MIT), DOSLFN "open source" not GPL; 1.1 disk license calls the disc a "mere aggregation" with per-package terms | https://sunsite2.icm.edu.pl/packages/freedos/distributions/1.1/errata.txt (via search 2026-11-17; the aggregation text itself UNVERIFIED for 1.4) |
| No project-wide redistribution policy page found (https://www.freedos.org/license/ is HTTP 404) | VERIFIED 404 on 2026-11-17. A policy statement was not located: UNVERIFIED. |

Official downloads (fetched https://www.freedos.org/download/ 2026-11-17): `https://download.freedos.org/1.4/FD14-{LiveCD,BonusCD,FullUSB,LiteUSB,FloppyEdition}.zip`. The page does not list sizes.

Checksums: `https://download.freedos.org/1.4/verify.txt` publishes md5, sha256 and sha512 for every zip (no GPG signature seen). It is on the same origin as the zips, so it detects corruption but not a compromised host; bilterm therefore pins hashes in its own repo (section 5). VERIFIED by hashing two downloads:

| File | Bytes | SHA256 (matches verify.txt) |
|---|---|---|
| `FD14-LiveCD.zip` | 293,950,337 | `2020ff6bb681967fd6eff8f51ad2e5cd5ab4421165948cef4246e4f7fcaf6339` |
| `FD14-FloppyEdition.zip` | 23,624,258 | `45b1fa7c52dd996c3bfa5e352ffcd410781b952a6ad629f15a4c9ec4bbaefc5a` |

Images: the LiveCD zip contains `FD14LIVE.iso` (bootable El Torito/ISOLINUX; its menu offers "Use FreeDOS 1.4 in Live Environment mode", boot hard disk, install) and `FD14BOOT.img` (1.44M installer boot floppy). The Floppy Edition zip holds `720k/144m/120m` disk-image sets (`x86BOOT.img`, `x86DSK01..06.img`). There is **no official pre-installed bootable hard-disk image** (UNVERIFIED beyond the download page and zip listings read above); the Live Environment mode is the closest, and needs no install step.

### 2.2 Emulators and BIOS

| Component | License | Source |
|---|---|---|
| QEMU | GPL-2.0-only (Homebrew metadata); repo has `COPYING` | `brew info qemu` 2026-11-17 (11.1.3); GitHub API reports NOASSERTION because of mixed files |
| SeaBIOS (QEMU's default x86 BIOS) | LGPLv3 per its docs; QEMU list opines it is "mere aggregation" with GPLv2 QEMU | mail.coreboot.org / github.com/qemu/seabios (search 2026-11-17); non-lawyer opinion, treat as unsettled |
| Bochs | LGPL-2.1 "as a whole", some parts with compatible licenses | https://raw.githubusercontent.com/bochs-emu/Bochs/master/bochs/LICENSE (fetched 2026-11-17); GitHub API: LGPL-2.1 |
| Bochs BIOS / VGABIOS | Ship in Bochs source as the "-lgpl" VGABIOS and Bochs BIOS; per-file headers not read: UNVERIFIED |  |
| DOSBox-X | GPL-2.0 (GitHub API); Homebrew: GPL-2.0-or-later | 2026-11-17 |
| DOSBox Staging | GPL-2.0-or-later, with some BSD-3 third-party parts (MAME audio, Voodoo) | https://raw.githubusercontent.com/dosbox-staging/dosbox-staging/main/LICENSE (2026-11-17) |
| dosemu2 | GPL-2.0 (GitHub API) | 2026-11-17 |
| 86Box | GPL-2.0 (GitHub API); needs separately-sourced ROMs (UNVERIFIED) | 2026-11-17 |
| v86 | BSD-2-Clause (GitHub API) | 2026-11-17 |

### 2.3 Mechanics and verdict

- **Bilterm license is Apache-2.0** (repo `LICENSE`, `Cargo.toml`). Apache-2.0 and GPL-2.0-only are not compatible for *combining into one work*; the plan never links, embeds or static-includes emulator code. (a) **Bundling binaries** next to bilterm in a release archive is generally "aggregation", but then we owe: the license texts, a written offer or copy of corresponding source for each GPL/LGPL binary and for every FreeDOS package (GPLv2 section 3), and notices for the freeware entries. That is a standing compliance burden per release, for ~300 MB of mixed-license content. (b) **Runtime download + separate process** (`Command::spawn`) is plain use: the user obtains software from its authors and bilterm only orchestrates it; no source-offer duty falls on bilterm. (c) **Trademark**: do not name the feature "FreeDOS (TM)" in branding beyond nominative use ("runs FreeDOS").
- No MS-DOS, Windows or other proprietary software is bundled, fetched or referenced by default. Users may point the mount feature at their own legally held files.

| Item | Verdict |
|---|---|
| FreeDOS ISO/zips inside bilterm release | **Do not bundle** (mixed licenses incl. freeware-only packages; compliance burden) |
| FreeDOS Floppy Edition (23 MB) | Download only. Per-package scan of the floppy set not done: UNVERIFIED |
| QEMU / Bochs / dosemu2 / DOSBox binaries | **Do not bundle**; prefer user's package manager (apt/brew/winget); else download only |
| BIOS blobs (SeaBIOS, Bochs BIOS) | Come with the emulator package; never repackage |
| Emulator code linked into bilterm (crate or lib) | **No** (licence incompatibility; also unnecessary) |
| Manifest (URLs, hashes, notices) in repo | Yes (Apache-2.0, our own data) |
| Bilterm launching the emulator as a child process | OK |
| `NOTICE`/docs listing third-party licenses and where sources live | Yes (D5) |

## 3. Emulator comparison

| Emulator | License | Text-mode bridge into a PTY | Keyboard in | Platforms / packaging | Health (2026-11-17) |
|---|---|---|---|---|---|
| **QEMU** `-display curses` | GPL-2.0 | **VERIFIED**: VGA text rendered as curses output on the PTY; boxes and block chars arrive as UTF-8 | **VERIFIED**: bytes written to the PTY reached DOS (echoed `N` at the installer prompt) | Linux apt (10.2.1 installed here), brew 11.1.3; winget UNVERIFIED; curses display on Windows/macOS builds UNVERIFIED | very active (pushed today) |
| **Bochs** `display_library: term` | LGPL-2.1 | Documented as "text only, uses curses/ncurses, cross platform" (https://bochs.sourceforge.io/doc/docbook/user/bochsrc.html, 2026-11-17). Not run here: UNVERIFIED. Distro builds may omit `term` | via ncurses, same mechanism | apt 2.8, brew 3.1; Win32 builds exist (term on Windows UNVERIFIED) | active (3.1 final 2026-08-23) |
| **dosemu2** `-t` | GPL-2.0 | `-t` terminal mode is the upstream design for text-mode DOS in a tty; not run: UNVERIFIED | tty | Linux only; no apt candidate on this host | active, small (last push 2026-10-25) |
| **DOSBox-X** | GPL-2.0 | No tty/curses front end known: UNVERIFIED. SDL window only | window | apt 2026.01.02 (candidate), brew 2026.10.01 | very active |
| **DOSBox Staging** | GPL-2.0+ | SDL window; headless text-mode dump not known: UNVERIFIED | window | brew 0.83.1 | very active |
| **86Box** | GPL-2.0 | GUI only; ROM files needed | window | brew cask 6.0 | very active |
| **v86** | BSD-2 | Browser/JS; could be driven headless via Node (not evaluated) | JS | npm | active |

**Pick: QEMU curses (primary), Bochs term (fallback).** QEMU is the only option demonstrated end to end here. Bochs is the cleaner licence (LGPL) and lighter, and is what the owner named; keep it as `--backend bochs`. DOSBox-X/Staging are "launch in its own window" backends for games (D4), not pane backends.

## 4. Architecture

```
bilterm pane (CompositePane, PTY) -> child: qemu-system-i386 -display curses -> FreeDOS
   keys ->PTY->qemu curses->guest KBC     VGA text -> curses escapes -> PTY -> VtParser -> grid
```

- A DOS pane is an ordinary PTY pane whose command is the emulator argv built by `src/dos/launch.rs`. Rendering, scrollback, themes, recording (`.cast`), RPC `pane.read` and `pane.send_keys` come free (`src/vt`, `src/api`).
- Terminal size: curses draws the 80x25 guest screen in whatever the PTY size is; below 80x25 the emulator clips. D2 enforces a minimum pane size or offers a scaled-down note. Resize forwards `SIGWINCH` via the existing PTY path.
- Keys: terminals cannot send key-up/scancodes, so guest input is cooked by the emulator's curses layer. Function keys, Ctrl combos and Alt are best-effort; document the leaks (e.g. bilterm's own prefix keys). Escape hatch: bilterm's normal pane focus keys stay in bilterm.
- **Graphics modes: out of scope for the pane.** The guest switches VGA to a graphic mode and curses shows blank ("VGA Blank mode" was seen during early boot). Options in D4: (a) detect and tell the user, (b) QEMU monitor `screendump` -> PNG -> existing Kitty/Sixel path (`src/vt/graphics.rs`, caps: 64 placements, 512 px max dimension; UNVERIFIED that 640x480 fits the cap), (c) hand off to a windowed backend (DOSBox-X). 
- Serial console: FreeDOS has no serial console by default and QEMU `-serial` only carries a COM port, not the screen; not used for display. (Used later optionally for a `bilterm dos exec` channel; UNVERIFIED.)
- Audio: disabled by default (`-audio none` / `-soundhw` omitted); no host devices.

### 4.1 D0 spike result (VERIFIED, 2026-11-17, Linux, no sudo)

Setup: downloaded `FD14-FloppyEdition.zip` and `FD14-LiveCD.zip` into `/datanapps/dos-spike`, SHA256-matched `verify.txt`. Used system `qemu-system-i386` 10.2.1 (SeaBIOS 1.17.0) launched under a Python `pty.fork` with a 100x30 window and `TERM=xterm`:

- `qemu-system-i386 -display curses -m 32 -fda x86BOOT.img -boot a`: SeaBIOS banner, "Booting from Floppy...", the FreeDOS kernel GPL banner, language menu (auto-selected after countdown), FreeCom 0.86, then the installer ("Do you want to proceed [Y,N]?") all appeared as text in the PTY.
- Injection: writing `N` to the PTY master after boot echoed `N` at that prompt.
- `-cdrom FD14LIVE.iso -boot d`: ISOLINUX menu with box drawing rendered; default entry is Live Environment after a 50 s countdown (the spike ended before DOS loaded; so the Live-mode-to-prompt path is UNVERIFIED).
- Not tested: macOS, Windows, Bochs, dosemu2, `file=fat:` host-folder mount, resize, mouse, long runs, CPU use while idle.

## 5. Installer / config plumbing

Commands (all behind `src/dos/`, each lowering to an `Action`, section 6):

| Command | Behavior |
|---|---|
| `bilterm dos setup [--backend qemu\|bochs] [--yes]` | detect emulator (`doctor`), print licence notice and ask consent, download manifest entries, verify, unpack, create the data dir |
| `bilterm dos run [--drive-c DIR] [--image FILE] [--name N]` | spawn a DOS pane (or print the argv with `--print-argv`) |
| `bilterm dos doctor` | emulator found and version, `-display curses` supported (`qemu-system-i386 -display help`), image hashes, free disk, pane size, which backends |
| `bilterm dos update` | re-read the pinned manifest shipped with this bilterm, fetch only changed entries |
| `bilterm dos remove [--all]` | delete downloaded images and per-session disks (confirm) |

- **Data dir**: `<data>/bilterm/dos/` created with `secure_fs::ensure_private_dir` (0700); files via `write_private_file_atomic` (0600); names through `validate_safe_name`. Layout: `images/FD14LIVE.iso`, `downloads/*.part`, `sessions/<name>/disk.img`, `manifest.lock.toml`.
- **Manifest** `src/dos/manifest.toml`, embedded with `include_str!`: per entry `id, version, url, sha256, bytes, license_note, source_url`. Pinned to FreeDOS 1.4 (sha256 above). Never "latest". An entry is the zip; we verify the zip, then extract only `FD14LIVE.iso` (or chosen member).
- **Download**: HTTPS only (rustls), host allowlist (`download.freedos.org`), size cap = manifest bytes + 1%, resume via HTTP `Range` into `.part` then rename, SHA256 over the final file before it is visible. No network after setup: `run` fails clearly if images are missing rather than fetching.
- **Archive safety**: extract by explicit member name (not whole-tree unzip); reject absolute paths, `..`, symlinks; cap uncompressed size.
- **Testable seams** (`designing-testable-seams`): `trait Downloader { fn fetch(&self, url, range) -> Result<Stream> }`, `trait EmulatorProbe` (find binary, run `-version`/`-display help`), `trait Spawner` (build `CommandSpec`, no process in tests). Mock mode `BILTERM_DOS_OFFLINE_FIXTURE=<dir>` makes setup read files from a directory and `run` spawn `cat` or a script that prints a fake 80x25 screen, so headless WM tests (`WindowManager::new_headless`) can assert pane content without QEMU.
- **Resources**: reuse the P4 limits plan (`docs/plans/session-host-p4-limits.md`): emulator memory `-m 32` default, cap in config, `RLIMIT_AS`/cgroup where P4 provides it, CPU niceness; one emulator per pane, max N panes (default 4).
- **Safety defaults**: never run as root (refuse if euid 0); `-nic none` (no network), no USB, no host audio, no `-snapshot` write-through to the downloaded ISO (images opened read-only; guest disk is a per-session copy-on-write overlay or `-snapshot`).

## 6. Uniform surface (every capability, every surface)

Per `CLAUDE.md`, every user-triggerable command lowers to `crate::action::Action` and runs through `WindowManager::apply` (`src/colon_command/execute_wm.rs`).

- New actions: `Action::DosRun{name, drive_c, image, backend}`, `DosSetup{..}`, `DosDoctor`, `DosRemove{..}`, `DosStatus`. `DosSetup` is long-running, so it is an async job whose progress is a status line plus RPC events, not a blocking `apply`.
- Parity matrix (all UNVERIFIED until D3 builds them):

| Capability | Colon | CLI | RPC `ctl` | MCP | Key |
|---|---|---|---|---|---|
| open DOS pane | `:dos run [--drive-c DIR]` | `bilterm dos run` | `dos.run` | `dos_run` | palette entry |
| setup/update | `:dos setup` | `bilterm dos setup` | `dos.setup` (+ progress notifications) | `dos_setup` | n/a |
| doctor | `:dos doctor` | `bilterm dos doctor` | `dos.doctor` | `dos_doctor` | n/a |
| remove | `:dos remove` | `bilterm dos remove` | `dos.remove` | `dos_remove` | n/a |
| status | `:dos status` | `bilterm dos status` | `dos.status` | `dos_status` | status bar |
| type into DOS | n/a | `ctl pane.send_keys` | existing `pane.send_keys` | existing | pane focus |

- CLI-only commands run without a WM (setup/doctor/remove touch only the data dir) but call the same `src/dos/` core functions the Action handlers call; no logic in thin wrappers.
- Mirror existing patterns: command registry (`src/command_registry.rs`), RPC dispatch (`src/api/rpc_dispatch.rs`), MCP console (`src/mcp_console`). New module files stay under the ARCH-R04 ~800-1000 line target.

## 7. Fun features (D4, each optional)

1. **Host folder as C:**: QEMU `-drive file=fat:rw:DIR,format=raw` (vvfat) exposes a host directory as a FAT disk; read-write vvfat is documented as experimental, so default to read-only plus a "sync back" copy step. UNVERIFIED here.
2. **Shareable disk images**: `bilterm dos image export/import` of per-session `disk.img` (zstd); import validates size and the MBR/FAT signature before accepting.
3. **DOS game launcher pane**: a TUI list of `.EXE/.COM/.BAT` from the mounted folder; pick one and bilterm injects `GAME.EXE\r`. Only user-supplied games; no bundled catalog (some FreeDOS games are freeware only, see 2.1).
4. **Retro CRT tie-in**: when a pane is a DOS pane, apply the existing 3270 phosphor themes and the CRT shader (`src/wm/shaders.rs`, `src/theme`) with a "DOS" preset (CGA/VGA 16 colors, amber/green modes).
5. **Screenshot / record**: text screenshot via `pane.read`; session recording via `WorkspaceRecorder`/`SessionRecorder` `.cast` (already applies to any pane). Graphics screenshot via QMP `screendump` (UNVERIFIED).
6. **Copy/paste**: pane selection already works on the grid; paste is `pane.send` with cooked keycodes (limit paste rate; the guest keyboard buffer is tiny).
7. **Per-session drives**: each bilterm session gets its own D: scratch disk under `sessions/<name>/`.
8. **Drag a file onto a pane**: copies the file into the mounted folder (`validate_safe_name`, 8.3 name mangling note) and types the name; terminal drag-and-drop support varies (UNVERIFIED).

## 8. Security

- Integrity: pinned SHA256 in repo, TLS via rustls, host allowlist, size cap, hash-before-use, atomic rename. `verify.txt` is cross-check only.
- Archives: named-member extract, no traversal, no symlinks, bounded size (section 5).
- Process: no root, no network (`-nic none`), no USB passthrough, argv built as a vector (no shell), data dir 0700, `0600` files, environment scrubbed except `TERM`/`LANG`.
- Guest is untrusted: emulator escapes are the emulator's problem, so require users to keep it updated via their package manager, and `doctor` warns on old versions (min version table in manifest).
- Resource limits: reuse P4 memory/CPU caps; the guest screen is bounded by 80x25 text, so existing VT limits apply. Host-folder mount is explicit opt-in; reject `/`, `$HOME`, and `..` mounts without `--force`.
- Untrusted text from the guest passes through the normal `VtParser` (OSC 52 and graphics caps already bounded).

## 9. Testing plan

- **Unit** (no network, no emulator): manifest parse and schema; hash verification (good, bad, truncated); resume logic with a mock `Downloader`; zip member extraction with traversal and symlink fixtures; argv builder snapshot tests (`insta`) for each backend; `doctor` against a fake probe; refusal as root; path validation.
- **Headless WM**: `WindowManager::new_headless` + offline-fixture spawner printing a canned 80x25 screen; assert grid text, `pane.read`, `send_keys` echo, Action/colon/RPC parity (a table-driven test that every row of section 6 exists).
- **Ignored real smoke** (`#[ignore]`, `BILTERM_DOS_SMOKE=1`): requires installed QEMU and cached floppy edition; boots, waits for "FreeCom" in the grid, sends a command, asserts echo. Mirrors the D0 spike. Never in `scripts/verify.sh` by default.
- Local CI only (owner preference); no hosted-CI downloads of 24-300 MB images.

## 10. PR breakdown

| PR | Scope | Exit criteria |
|---|---|---|
| D0 | Spike script `scripts/dos-spike.sh` (not wired into the build): QEMU curses boot, key injection; add Bochs `term` and dosemu2 `-t` runs; record results in this doc. Already done for QEMU on Linux (4.1). Remaining: Bochs, dosemu2, macOS (`ssh bill@bigmac.local`), Windows ARM64 | Results table filled; backend choice confirmed |
| D1 | `src/dos/{manifest,download,verify,paths}.rs`, `manifest.toml`, mock downloader, `bilterm dos setup/remove/doctor` (CLI) | unit tests pass; real setup verified manually on Linux |
| D2 | `src/dos/launch.rs`, `bilterm dos run`, DOS pane spawn through `WindowManager::apply`, min-size check, `-nic none` defaults | headless fixture test + ignored real smoke |
| D3 | `Action::Dos*`, `:dos ...`, RPC `dos.*`, MCP tools, status line, parity table test | parity test green, `ctl` e2e |
| D4 | Fun features (7.1 to 7.8), each its own sub-PR | per-feature tests |
| D5 | docs: `docs/DEVELOPER_AND_AGENT_GUIDE.md` section, `AGENTS.md` pointer, third-party notices page, `--help` text | `keeping-docs-in-sync` check |

## 11. Open questions for the owner

1. Primary backend: QEMU (verified, GPL, heavy ~ tens of MB package) vs Bochs (LGPL, light, owner's suggestion, text bridge unverified). Accept QEMU-first with Bochs as `--backend`?
2. Is the Live Environment (no install, RAM-based) enough, or should `setup` also drive the FreeDOS installer to a persistent C: image (needs unattended install research, UNVERIFIED)?
3. Windows scope: ship DOS mode on Windows at all in v1, given curses displays there are unverified?
4. Are graphics-mode programs in scope (window hand-off to DOSBox-X, or Sixel/Kitty screendump), or text-only for v1?
5. Do you want a counsel review before any bundled artifact is ever considered (current recommendation: never bundle)?
6. Should the feature be a Cargo feature (`dos`), default-on or off, to keep `bilterm` lean?
7. Branding: ok to call it "DOS mode" and say "runs FreeDOS" (nominative use of Jim Hall's trademark)?

## Sources (fetched 2026-11-17)

- https://www.freedos.org/download/ , https://www.freedos.org/about/ , https://freedos.org/download/announce.html
- https://download.freedos.org/1.4/verify.txt , https://download.freedos.org/1.4/readme.txt (inside zips)
- https://bochs.sourceforge.io/doc/docbook/user/bochsrc.html , https://raw.githubusercontent.com/bochs-emu/Bochs/master/bochs/LICENSE
- https://raw.githubusercontent.com/dosbox-staging/dosbox-staging/main/LICENSE
- GitHub API repo metadata for dosbox-x, dosbox-staging, dosemu2, 86Box, v86, qemu, Bochs; `brew info` for bochs, qemu, dosbox-x, dosbox-staging
- https://sunsite2.icm.edu.pl/packages/freedos/distributions/1.1/errata.txt , SeaBIOS licensing threads at mail.coreboot.org (search results only)
