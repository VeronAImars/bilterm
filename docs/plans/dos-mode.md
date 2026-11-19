# Plan: DOS Mode (FreeDOS in a bilterm pane)

Status: PLAN ONLY (docs, no code). Research date: 2026-11-17; revised 2026-11-18 with owner decisions (sections 0 and 0.1), cross-OS research (4.2-4.4), persistent C: (5.1), Windows (5.2). Not legal advice; the owner should confirm the verdicts in section 2 before any bundling.
Baseline: `main` (bilterm 3.3.1, `Cargo.toml` license `Apache-2.0`, repo `LICENSE` is Apache License 2.0).

Conventions: **VERIFIED** = observed first-hand on 2026-11-17 (command run or primary source fetched; source named). **UNVERIFIED** = not confirmed; the PR that depends on it must check. Nothing here is asserted from memory.

## 0. Owner decisions 2026-11-18

Answers to the former section 11 (all now ANSWERED; remaining questions are in section 11):

1. **Backend**: QEMU first on Linux, but with a real cross-platform answer: a `DosBackend` trait (4.3) with per-OS support matrix (4.2), so QEMU, Bochs and a text-buffer screendump renderer are interchangeable. Bochs/other backends are sub-features.
2. **Persistence**: a persistent C: image, NOT Live-only. Unattended build recipe proven in spike D0b (5.1).
3. **Windows**: IN v1 scope (5.2). Graphics-mode programs: text-only for v1; the screendump route (4.4) can later feed bilterm's image protocols.
4. **Packaging/legal**: opt-in Cargo feature `dos` (off by default), NEVER bundle, download on demand with pinned SHA256, nominative branding ("DOS mode -- runs FreeDOS"). Counsel review is not required while nothing is bundled (revisit if that changes).

## 0.1 Owner decisions 2026-11-18 (2)

1. **Test machines exist for every OS; no "is there a VM" question.** Windows x64: `win-devvm` on the Linux dev box (`/home/bill/src/infra/win-devvm`, libvirt KVM, `./vm.sh start|wait|status|ssh|push|pull`). Windows ARM64: `win-devvm` under Parallels on `ssh bill@bigmac.local` (`~/src/tools/win-devvm`; non-interactive PATH needs `/usr/local/bin:/opt/homebrew/bin`; throwaway worktrees under `~/worktrees`). macOS arm64 and Intel x86_64: bigmac (both rust targets installed; Rosetta running). Repo has `scripts/build-windows-vm.sh` and `scripts/windows-smoke-test.ps1`. Every OS gets concrete spikes/tests (4.2, 9, 10); results are VERIFIED or UNVERIFIED by running, with the exact command for anything not yet run.
2. **Image defaults accepted**: 128 MiB, 2 GiB cap, sparse raw. **Package set (decided here)**: default image = FreeDOS BASE only (kernel, FreeCom, core utils; fits 128 MiB, fast and deterministic setup). `setup --full` opt-in installs the full Live package set into a larger image (default 512 MiB, UNVERIFIED size; needs `mcopy`/pure-Rust unzip of each pinned package). Rationale: size, setup time, and licence hygiene: the Live ISO contains packages under mixed terms (some freeware-only, not OSI); those are fetched at download time from the pinned ISO on the user's machine and never bundled or redistributed by bilterm, and BASE-only keeps the default path to GPL/permissive core packages.
3. **macOS Intel and Windows ARM64: SUPPORTED** (not best-effort); each has a verification task in section 10.
4. **Pure-Rust FAT writer** (`fatfs`-style) under the `dos` feature is accepted; fall back to shelling out to mtools on Unix if the crate disappoints in D0c.
5. **Emulator version policy**: manifest has a `min_version` floor for system-installed emulators (we only check `-version`), and an exact pinned version plus SHA256 for anything bilterm downloads. Versions found working 2026-11-18: QEMU 10.2.1 (Linux), 10.2.0 and 11.1.3 (macOS Homebrew), 11.1.0 (Windows x64 via winget). **Floor: QEMU >= 10.2.0** (lowest version verified; every verified OS is at or above it). **Pin for downloads** (Windows portable/installer path): `qemu-w64-setup-20260811.exe` (QEMU 11.1.0, 206615928 bytes), SHA256 `f98a8aeb5f7faea9765b6dee28316c266cd179d80354a2fed8e50176f9a2e59f` (hashed by us from https://qemu.weilnetz.de/w64/2026/qemu-w64-setup-20260811.exe; winget also verified its manifest hash). Raise the floor only after re-running the per-OS spikes.

## 1. Summary and recommendation

Goal: `:dos` opens a pane running FreeDOS (a "retro PC in a pane"), using an emulator that renders VGA text mode into the pane's PTY so the existing VT parser, themes, recording and RPC all work unchanged.

Headlines:
1. **Licensing verdict: do not bundle by default; download on demand.** FreeDOS 1.4 is a *mix* of licenses, mostly GPL but including freeware-only packages (VERIFIED, section 2.1). Redistribution of the whole ISO inside bilterm releases would make bilterm responsible for per-package notices and source offers. Downloading at runtime from the official host and launching a separate process creates no combination with bilterm's Apache-2.0 code.
2. **Recommended emulator: QEMU (`qemu-system-i386`)**, with `-display curses` where compiled in and a VGA text-buffer reader (QMP `pmemsave` of 0xB8000, VERIFIED 2026-11-18) where it is not. VERIFIED on this machine: FreeDOS 1.4 boots, text appears in a PTY, and keystrokes injected through the PTY reach DOS (section 4.1). Packaged by apt/brew (VERIFIED), FreeBSD ports and OpenBSD ports (primary-source pages read, 4.2), MSYS2/Stefan Weil installers on Windows.
3. **Fallback: Bochs `display_library: term`** (LGPL-2.1, text-only ncurses front end, documented) for a lighter, easier-to-license option; **dosemu2 `-t`** on Linux only; **DOSBox-X / Staging** for games, as a separate window (no text-mode pane bridge known).
4. **Biggest risk:** the bridge is only text mode in v1. Any graphics-mode program (most games, Windows 3.1) shows nothing useful in a curses pane. Second risk: Windows (QEMU's build system can link pdcurses on Windows, but whether the shipped builds include it is UNVERIFIED), mitigated by the text-buffer renderer that needs no curses at all.
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
| **QEMU** `-display curses` | GPL-2.0 | **VERIFIED**: VGA text rendered as curses output on the PTY; boxes and block chars arrive as UTF-8 | **VERIFIED**: bytes written to the PTY reached DOS (echoed `N` at the installer prompt) | Linux apt (10.2.1 installed here), brew 11.1.3; curses display on macOS VERIFIED (4.2); Windows builds UNVERIFIED | very active (pushed today) |
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

### 4.2 Cross-platform findings and support matrix

Evidence (fetched/run 2026-11-18):

| Fact | Source |
|---|---|
| macOS arm64 (Homebrew qemu 11.1.3): `qemu-system-i386 -display help` lists `none, curses, cocoa, dbus`. **VERIFIED** by running over ssh on bigmac.local | first-hand |
| Homebrew qemu formula has runtime dependency `ncurses` | https://formulae.brew.sh/api/formula/qemu.json (2026-11-18) |
| Linux (Ubuntu qemu 10.2.1): curses display works (4.1) | first-hand |
| FreeBSD port `emulators/qemu` 11.1.3_1: ncurses is a mandatory choice of three options (NCURSES_BASE / NCURSES_DEFAULT / NCURSES_PORT), so curses is compiled in | https://www.freshports.org/emulators/qemu/ (2026-11-18). Package build not run: UNVERIFIED |
| OpenBSD port `emulators/qemu` 11.1.3p0; ncurses not listed as a dependency (base curses); readme examples use `qemu-system-i386`; arch restriction list reads aarch64, amd64, powerpc64, riscv64, sparc64 (amd64 OK). Whether `-display curses` is compiled in is UNVERIFIED | https://openports.pl/path/emulators/qemu (2026-11-18) |
| **Windows x64 (win-devvm, Windows 11 10.0.26300): `winget install -e --id SoftwareFreedomConservancy.QEMU` installed QEMU 11.1.0 (hash verified); `qemu-system-i386 -display help` lists `gtk, sdl, egl-headless, curses, spice-app, dbus`, so curses IS compiled into the Weil build. VERIFIED** | first-hand 2026-11-18 |
| **Windows x64: text-buffer path VERIFIED.** `-display none -qmp tcp:127.0.0.1:4555`, QMP `pmemsave` of 0xB8000 (4000 bytes) from PowerShell, every second byte decoded: SeaBIOS/iPXE banner text. `-display curses` under a real ConPTY not yet run: UNVERIFIED | first-hand 2026-11-18 |
| **macOS arm64 (bigmac): QMP `pmemsave` over loopback TCP VERIFIED (SeaBIOS banner decoded); `-display curses` under a Python pty (100x30) printed the SeaBIOS banner. VERIFIED.** Homebrew has qemu 10.2.0 and 11.1.3 | first-hand 2026-11-18 |
| Windows: QEMU's own site says Stefan Weil provides 32/64-bit Windows binaries and installers (https://qemu.weilnetz.de); MSYS2 packages `mingw-w64-x86_64-qemu` / `mingw-w64-ucrt-x86_64-qemu`; 32-bit Windows not supported | https://www.qemu.org/download/ (2026-11-18) |
| QEMU `meson.build` can build curses on Windows via `ncurses`/`ncursesw` pkg-config or library `pdcurses`; and the shipped Weil Windows build does list curses (row above) | https://gitlab.com/qemu-project/qemu/-/raw/master/meson.build (2026-11-18) |
| winget package id `SoftwareFreedomConservancy.QEMU` (v11.1.0, downloads qemu.weilnetz.de) exists and installs. **VERIFIED** | first-hand |
| Bochs configure has `--with-term` (text-only curses GUI; "almost certainly won't work right with the debugger or the textconfig interface"); default GUI on Win32/MinGW is `--with-win32`; the docs do not say term works on Windows or which distro builds enable it | https://bochs.sourceforge.io/doc/docbook/user/compiling.html (2026-11-18) |
| QMP has `screendump` (Remote desktop section) and `pmemsave`/`memsave` (Machines section); detailed arg semantics not read | https://www.qemu.org/docs/master/interop/qemu-qmp-ref.html (2026-11-18, truncated); `pmemsave` itself **VERIFIED** (4.4) |

**Support matrix (v1):**

| OS | Level | Backend order | Status / verification |
|---|---|---|---|
| Linux (x86_64, aarch64) | **Supported** | qemu-curses, qemu-textbuf, bochs-term | curses VERIFIED (QEMU 10.2.1); aarch64: UNVERIFIED, no host here (qemu TCG, same path) |
| macOS arm64 | **Supported** | qemu-curses, qemu-textbuf | curses + pmemsave VERIFIED on bigmac (QEMU 11.1.3); pure-Rust image builder boots to FreeCom VERIFIED on bigmac (D0c, 5.1a) |
| macOS Intel x86_64 | **Supported** | qemu-curses, qemu-textbuf | bigmac builds/tests the `x86_64-apple-darwin` bilterm binary under Rosetta; native Intel QEMU not available on bigmac (no `/usr/local` brew): UNVERIFIED, task D7b |
| Windows 10/11 x86_64 | **Supported** | qemu-textbuf, qemu-curses | QEMU install via winget, curses listed, TCP QMP pmemsave VERIFIED on win-devvm; pure-Rust image build VERIFIED on win-devvm (D0c, 5.1a; image booted on Linux); ConPTY run, QEMU boot on Windows, bilterm `dos run` UNVERIFIED (D6) |
| Windows 11 ARM64 | **Supported** | qemu-textbuf, qemu-curses | win-devvm on bigmac Parallels; x64 QEMU runs under Windows' x64 emulation: UNVERIFIED, task D6b |
| FreeBSD / OpenBSD | **Best-effort** (no guest) | FreeBSD: curses, textbuf, bochs-term; OpenBSD: textbuf first | no BSD VM exists; task D2d proposes adding a FreeBSD and OpenBSD guest to win-devvm/libvirt; until then ports analysis only, not run |
| other (NetBSD, illumos, ...) | **Unsupported** | `dos doctor` explains and exits non-zero | no promises |

Alternatives considered, not selected for v1: serial console (FreeDOS has no serial console by default; COM port only), DOSBox-X/Staging (SDL window, no tty front end known: UNVERIFIED), dosemu2 `-t` (Linux/BSD only, not run), v86 (JS), 86Box (GUI, ROMs). They remain possible third-party backends behind the trait.

### 4.3 Backend contract (`designing-plugin-contracts`)

Hosts depend only on a trait; backends are interchangeable and selected by detection (`--backend` overrides).

```rust
pub trait DosBackend: Send {
    fn id(&self) -> &'static str;                       // "qemu-curses" | "qemu-textbuf" | "bochs-term" | "mock"
    fn probe(&self, env: &dyn HostEnv) -> Probe;         // found? version? capabilities; no side effects
    fn caps(&self) -> BackendCaps;                       // text_pane_native, textbuf, graphics_dump, snapshots, host_folder
    fn plan(&self, spec: &SessionSpec) -> Result<LaunchPlan>; // argv vector, env, control endpoints; no process spawned
    // textbuf backends additionally implement TextSource: fn read_text(&mut self) -> Result<TextScreen /* 80x25 cells+attrs, cursor */>
    //                                                     fn send_key(&mut self, KeyEvent) -> Result<()>
}
```

- `SessionSpec { c_image, extra_drives, mem_mb, size_cols, size_rows, readonly_media }` is backend-neutral. `LaunchPlan` is either `Pty{argv}` (curses-style: the pane is an ordinary PTY pane) or `Bridge{argv, control}` (textbuf-style: bilterm owns a `TextScreen` -> pane-grid renderer and injects keys via QMP `send-key`/`sendkey`).
- Conformance suite: one table-driven test module runs every backend against the **mock backend** (offline; prints a canned 80x25 screen, echoes keys, supports snapshot fakes), then the same assertions run against real backends in `#[ignore]` smoke tests. A new backend = one file + one row in the matrix; the host (`launch.rs`, pane code, Actions) does not change.
- Selection: `detect()` walks the per-OS order from 4.2 using only `probe()` results.

### 4.4 Text-buffer (screendump) renderer -- the no-curses path

Spike result, VERIFIED 2026-11-18: with `-display none` and `-qmp unix:...`, QMP `pmemsave val=0xB8000 size=4000` writes the VGA text buffer; taking every second byte and decoding as CP437 yielded the exact FreeDOS installer screen (box drawing intact), polled via a Python client while keys went in through the monitor command `sendkey`. So a pure-Rust Windows/BSD path needs no curses: poll `pmemsave` (or `pmemread`-style via a temp file; on Windows a named temp file under the private data dir) at ~10-20 Hz when dirty, convert 80x25 (char, attribute) cells to the bilterm grid with the 16-color CGA palette, map cursor from the BIOS data area (0x40:0x50, UNVERIFIED). Cost: polling, no true cursor-shape/blink fidelity. Limits: text modes only (0x3/0x7); in graphics modes the buffer at 0xB8000 is not text, so detect mode (BDA 0x449) and show a "graphics mode" notice. Later (post-v1): QMP `screendump` (PPM/PNG) -> existing Kitty/Sixel path (`src/vt/graphics.rs`) to render graphics modes; caps (64 placements, 512 px) UNVERIFIED for 640x480.

## 4.5 Cargo features

- `dos` (opt-in, off by default; not in `default`, not in release-default builds): compiles `src/dos/` and the Action/RPC/MCP/colon surface. Without it, `bilterm dos ...` and `:dos` print "built without the `dos` feature" and exit non-zero.
- Sub-features (all imply `dos`): `dos-qemu` (default inside `dos`), `dos-bochs`, `dos-mock` (mock backend; always compiled under `cfg(test)`). Backend sub-features only gate code, never bundle binaries. Download/extract crates (zip, sha2) are pulled in only by `dos`, keeping the default dependency tree unchanged.
- `scripts/verify.sh` stays on default features; add one extra `cargo check --features dos` stage in D1.

## 5. Installer / config plumbing

Commands (all behind `src/dos/`, each lowering to an `Action`, section 6):

| Command | Behavior |
|---|---|
| `bilterm dos setup [--backend qemu-curses\|qemu-textbuf\|bochs-term] [--yes]` | detect emulator (`doctor`), print licence notice and ask consent, download manifest entries, verify, unpack, create the data dir |
| `bilterm dos run [--drive-c DIR] [--image FILE] [--name N]` | spawn a DOS pane (or print the argv with `--print-argv`) |
| `bilterm dos doctor` | on unsupported OSes (or a build without the `dos` feature) prints the matrix row, why, and exits non-zero without touching the network; otherwise: emulator found and version, `-display curses` supported (`qemu-system-i386 -display help`), image hashes, free disk, pane size, which backends |
| `bilterm dos update` | re-read the pinned manifest shipped with this bilterm, fetch only changed entries |
| `bilterm dos remove [--all]` | delete downloaded images and per-session disks (confirm) |

- **Data dir** (all platforms; Windows under `%LOCALAPPDATA%`): `<data>/bilterm/dos/` created with `secure_fs::ensure_private_dir` (0700); files via `write_private_file_atomic` (0600); names through `validate_safe_name`. Layout: `images/FD14LIVE.iso`, `images/FD14BOOT.img`, `downloads/*.part`, `c/<name>.img`, `snapshots/<name>/<tag>.img`, `sessions/<name>/` (scratch), `manifest.lock.toml`.
- **Manifest** `src/dos/manifest.toml`, embedded with `include_str!`: per entry `id, version, url, sha256, bytes, license_note, source_url`. Pinned to FreeDOS 1.4 (sha256 above); also pins `FD14BOOT.img` member hash, and per-OS emulator archive entries where we offer a portable download (Windows). Never "latest". An entry is the zip; we verify the zip, then extract only `FD14LIVE.iso` (or chosen member).
- **Download**: HTTPS only (rustls), host allowlist (`download.freedos.org`), size cap = manifest bytes + 1%, resume via HTTP `Range` into `.part` then rename, SHA256 over the final file before it is visible. No network after setup: `run` fails clearly if images are missing rather than fetching.
- **Archive safety**: extract by explicit member name (not whole-tree unzip); reject absolute paths, `..`, symlinks; cap uncompressed size.
- **Testable seams** (`designing-testable-seams`): `trait Downloader { fn fetch(&self, url, range) -> Result<Stream> }`, `trait EmulatorProbe` (find binary, run `-version`/`-display help`), `trait Spawner` (build `CommandSpec`, no process in tests). Mock mode `BILTERM_DOS_OFFLINE_FIXTURE=<dir>` makes setup read files from a directory and `run` spawn `cat` or a script that prints a fake 80x25 screen, so headless WM tests (`WindowManager::new_headless`) can assert pane content without QEMU.
- **Resources**: reuse the P4 limits plan (`docs/plans/session-host-p4-limits.md`): emulator memory `-m 32` default, cap in config, `RLIMIT_AS`/cgroup where P4 provides it, CPU niceness; one emulator per pane, max N panes (default 4).
- **Safety defaults**: never run as root (refuse if euid 0); `-nic none` (no network), no USB, no host audio, no `-snapshot` write-through to the downloaded ISO (images opened read-only; guest disk is a per-session copy-on-write overlay or `-snapshot`).

### 5.1 Persistent C: image

Decision: `bilterm dos setup` produces a persistent per-user `c.img`; Live Environment stays an optional `--live` mode only.

**Recipe PROVEN by spike D0b (2026-11-18, Linux, no sudo, QEMU 10.2.1, files under /datanapps/dos-spike/d0b)**, no FreeDOS installer UI needed:
1. `qemu-img create -f raw c.img 128M`
2. `sfdisk c.img` with `label: dos` / `start=2048, type=6, bootable` (one FAT16 partition at 1 MiB)
3. `mformat -i c.img@@1048576 -F -t 250 -h 16 -s 63 ::` (mtools formats the partition in place; offset syntax `@@`). NOTE (D0c correction, 5.1a): `-F` forces **FAT32**, not FAT16, despite the MBR type 0x06; it booted anyway, but drop `-F` (or use the Rust builder) for a true FAT16 volume
4. `mcopy -i c.img@@1048576 KERNEL.SYS COMMAND.COM ::/` (both taken from the pinned `FD14LIVE.iso`/BASE content)
5. `dd if=mbr.bin of=c.img bs=440 count=1 conv=notrunc` (MBR boot code is required: sfdisk writes the table only and SeaBIOS hangs at "Booting from Hard Disk..." without it; the syslinux `mbr.bin` worked; for a no-syslinux path, embed a ~440-byte public-domain MBR or use FreeDOS `FDISK /MBR`)
6. Boot once from the pinned `FD14BOOT.img` floppy with `c.img` attached (`format=raw` explicitly), wait for the FreeCom prompt, inject `sys c:` + Enter via QMP `sendkey` (screen read via `pmemsave`), see "System transferred."
7. Boot from `c.img` alone: SeaBIOS -> FreeDOS kernel 2043 banner -> `C: HD1, Pri[1] ... size=127 MB` -> FreeCom 0.86 prompt. **VERIFIED.**

So the only scripted-UI step is the single `sys c:` command; no installer dialog is driven. Packages are installed by `unzip` of the pinned `.zip` packages from the Live ISO (`packages/<set>/*.zip`, members are `BIN/ DOC/ APPINFO/ SOURCE/`) into a staging dir, then one `mcopy -s -Q stage/FREEDOS ::/` (about 1 s for 4260 files), plus generated `FDCONFIG.SYS` (HIMEMX, `SHELL=\COMMAND.COM \ /E:1024 /P=\AUTOEXEC.BAT`) and `AUTOEXEC.BAT` (DOSDIR/PATH/TEMP/NLSPATH). **D0b results (2026-11-18, `scripts/dos-spike-install.sh`, Linux, QEMU 10.2.1)**:
- FD14-LiveCD.zip downloaded into a fresh dir, SHA256 checked against `download.freedos.org/1.4/verify.txt` (`2020ff6b...6339`): **VERIFIED** match. Note `verify.txt` lists md5 then sha256; parse only the `sha256sum:` section.
- All 57 BASE packages + AUTOEXEC/FDCONFIG installed into the FAT16 image, `sys c:` via QMP `send-key` + `pmemsave` screen reads, boots to FreeCom with autoexec banner: **VERIFIED**.
- **Reboot persistence VERIFIED**: boot 1 types `echo persist-ok-<pid> > c:\persist.txt` (QMP `send-key`), `quit`; boot 2 is a fresh QEMU process, `type c:\persist.txt` prints the text, and `mtype` on the host reads the same bytes. Whole script runs in about 37 s per image. Tested at 128 MiB (base), 512 MiB (full, FAT16 type 6 -> `0e` above 500 MiB), 1024 MiB (all).
- Keystroke injection: QMP `send-key` qcodes work (shift as a two-key combo for `:` `>` `_`); no HMP needed. QMP over TCP loopback used (also works for the Windows path).
- **Package sizes** (zip -> uncompressed -> on-disk FAT16 used incl. cluster slack, SOURCE/ trees dropped):

| Set | Zips | Zip | Uncompressed (with SOURCE) | Files on disk | On-disk used |
|---|---|---|---|---|---|
| BASE (57) | 12.6 MB | 12.3 MiB | 18.9 MB | 1002 | ~13.4 MiB (image 13756 KiB) |
| FULL = base+apps+archiver+drivers+net+tools+unix (152) | 117 MB | 112 MiB | ~164 MB | 4260 | ~97 MiB (99200 KiB) |
| ALL = FULL+games+sound (191) | 266 MB | 254 MiB | ~395 MB | 5667 | ~259 MiB (265064 KiB) |

  Live ISO is 316 MB (games 114 MB and sound 31 MB zipped dominate; net 67 MB).
- **Decision for `setup --full`**: image **256 MiB** (FAT16, ~2.6x headroom over ~97 MiB used; 512 MiB measured working too), package set `base apps archiver drivers net tools unix` (no games/sound; those are opt-in `--games`). BASE-only default stays 128 MiB (13 MiB used). Max stays 2 GiB (FAT16 limit, UNVERIFIED above 1 GiB).
- Pitfalls: `MKDIR failed` if AUTOEXEC mkdirs a dir that the packages already staged (stage `FREEDOS/TEMP`); kernel prints harmless `suspect partition` CHS warnings from the mformat geometry (boots fine).
- **macOS arm64 (bigmac, QEMU 11.1.3)**: `brew install mtools` OK (4.0.50); `bsdtar`, `unzip`, `python3` present; **`sfdisk` absent, script stops with exit 3 as designed** (UNVERIFIED beyond that; D0c pure-Rust builder covers partition table + MBR + FAT, and must also embed an MBR since syslinux `mbr.bin` is not on macOS either). Windows out of scope here. Other options kept as fallbacks: FreeDOS installer answer file (UNVERIFIED that one exists), or full keystroke-driven installer via the same QMP driver (more fragile).

Not verified: persistence across a guest write+reboot (expected), FAT32/larger sizes, qcow2 (use qcow2 only if snapshots via `qemu-img snapshot` are wanted; raw + file copy is simpler), macOS/Windows runs of this mtools recipe (mtools/sfdisk are Linux tools: the pure-Rust `fatfs` + hand-built MBR replacement of steps 2-5 is built and VERIFIED in 5.1a on Linux, macOS arm64 and Windows x64; dependency under `dos` accepted, section 0.1 item 4).

- **Location and size**: `<data>/bilterm/dos/c/<name>.img` via `secure_fs::ensure_private_dir` (0700) and atomic 0600 writes (`write_private_file_atomic`: build as `.part`, fsync, rename); `validate_safe_name` on `<name>`. Default 128 MiB raw (sparse), configurable up to a cap (default 2 GiB); sparse-file support checked by `doctor`.
- **Lifecycle**: `bilterm dos reset [--name N]` (delete and rebuild from the pinned base, confirm), `snapshot [--name N] [--tag T]` (reflink/plain copy to `snapshots/<name>/<tag>.img`, guest must be stopped or use QMP `stop` + copy + `cont`), `restore --tag T`, `snapshot list|rm`. Per-session overlay mode: `-drive ...,snapshot=on` for throwaway runs.
- **Corruption handling**: single writer (lock file in the dir, `controlling-processes-safely` rules: pid+start-time check); a `.dirty` marker set at run and cleared on clean exit; on a dirty start run a read-only FAT check (`fsck.fat -n` where present, or built-in boot-sector/FAT sanity) and offer restore from the latest snapshot; keep one automatic `pre-run` snapshot rotation (N=2, cap by size).

### 5.1a Spike D0c: pure-Rust image builder (2026-11-18)

Source: `spikes/dos-d0c/` (standalone crate, own empty `[workspace]`, NOT in the bilterm build, bilterm's `Cargo.toml` untouched). Repro: `WORK=<fresh dir on /datanapps> spikes/dos-d0c/scripts/repro.sh` (downloads `FD14-FloppyEdition.zip`, SHA256 `45b1fa7c...fc5a` checked against `verify.txt`, builds, boots; printed `D0C REPRO PASS` on Linux).

**Crates evaluated** (crates.io API, 2026-11-18):

| Crate | Version / last release | Licence | Verdict |
|---|---|---|---|
| `fatfs` | 0.3.6, 2023-01-17 (~1.9M downloads) | MIT | **Chosen.** Formats FAT12/16/32 (`format_volume`, `FatType::Fat16`), creates files, `std` Read+Write+Seek; worked unchanged on Linux, macOS arm64, Windows x64. Caveats: last release is almost 2 years old (maintenance risk; code is small and pinnable/vendorable), no hook to write boot code (we patch sector 0 ourselves), timestamps via `chrono` (not byte-reproducible across runs) |
| `fscommon` | 0.1.1, 2020-07-14 | MIT | **Chosen** for `StreamSlice` (partition view inside the raw file) and `BufStream`; tiny, stable, unmaintained but trivial to inline (~100 lines) |
| `embedded-fatfs` | 0.0.0, 2023-07 | MIT | Rejected: placeholder release, async/embedded API |
| `fat32`, `exfat` | 0.2.0 (2020), 0.1.0 (2023) | MIT | Rejected: wrong scope (FAT32 only / exFAT) |
| `mbrman` | 0.7.0, 2026-11-09 | MIT OR Apache-2.0 | Not needed: an MBR with one entry is 16 bytes written by hand; reconsider only if multiple partitions are wanted |

MIT is compatible with bilterm's Apache-2.0 (needs the MIT notice in the third-party notices). Dependency stays under the `dos` feature (0.1 item 4).

**What the spike does** (`spikes/dos-d0c/src/main.rs`, ~200 lines): `set_len` a sparse raw image (128 MiB default); write an MBR (original 105-byte boot code hand-assembled for this spike: relocate to 0x600, find the active entry, INT 13h/42h LBA read of its first sector, check 0xAA55, jump; so **no syslinux `mbr.bin` and no third-party licence**) with one active type-0x06 partition at LBA 2048; `format_volume` FAT16 into the partition; patch the BPB (63 sectors/track, 16 heads, hidden sectors 2048, drive 0x80); copy `KERNEL.SYS` first, then `COMMAND.COM`, generated `FDCONFIG.SYS` and `AUTOEXEC.BAT`; build as `.part` then rename.

**Evidence** (QEMU `-display none` + QMP `pmemsave` 0xB8000, driver `spikes/dos-d0c/scripts/qemu_drive.py`; and `-display curses` in a PTY):

| Check | Result |
|---|---|
| Linux (QEMU 10.2.1): pure-Rust image boots to `FreeCom version 0.86`, `AUTOEXEC RAN`, `C:\>` | VERIFIED (128, 64 and 512 MiB; same boot sector blob each time) |
| Linux `-display curses` in a PTY: same output | VERIFIED (128 MiB) |
| Image is sparse on Linux | VERIFIED (412 KiB allocated of 128 MiB) |
| macOS arm64 on bigmac (rustc/cargo 1.99, QEMU 11.1.3): build, boot to prompt | VERIFIED; sparse on APFS (2.8 MiB allocated). Image also boots on Linux |
| Windows 11 x64 on win-devvm (MSVC rustc, `vm.sh ssh`): `cargo build --release` + image build | VERIFIED; image pulled back to Linux boots to `C:\>`. QEMU itself not run for this image on Windows (UNVERIFIED; D6) |
| Windows sparse files | **Finding:** plain `set_len` on NTFS allocates all 128 MiB. Marking the file sparse first (`fsutil sparse setflag`, equivalent to `FSCTL_SET_SPARSE`) gave 512 KiB allocated, flag survives the rename: VERIFIED. Real code should call `DeviceIoControl(FSCTL_SET_SPARSE)` |
| Windows ARM64, Bochs `term`, QMP textbuf poll against the new image | UNVERIFIED (not run) |
| `fsck.fat`/Windows `chkdsk` on the image | UNVERIFIED (`mdir`, `sfdisk -l` read it back correctly on Linux) |

**Boot sector decision.** The FreeDOS FAT16 boot sector (`sys c:` writes it) is the one piece pure Rust cannot invent. Findings: (a) the sector written by `fatfs` has no boot code; (b) once the BPB values above are set, a sector captured from the guest's own `sys c:` is **byte-identical** to the one the Rust builder produces with that blob, and the same 450 bytes of code booted FAT16 volumes of 64, 128 and 512 MiB (code is geometry-independent, it reads the BPB at run time); (c) the FAT12 floppy boot sector cannot be reused (236 of 448 code bytes differ); (d) `SYS.COM` in 1.4 is UPX-packed (`UPX!` marker), so the embedded sectors cannot be read out without a UPX unpacker (UNVERIFIED that one is practical in Rust). (e) The FreeDOS kernel boot code is GPL-2.0-or-later: embedding the 450-byte blob in an Apache-2.0 repo is a licensing call for the owner, not made here. Options ranked: 1) **Capture once at `dos setup`**: build the image in Rust, boot the pinned `FD14BOOT.img` once, QMP-type `sys c:`, read sector 0 back (this is exactly step 6 of 5.1, already PROVEN, ~40 s), cache the blob in the data dir; no GPL code in the repo, one scripted command. 2) UPX-unpack `SYS.COM` from the pinned zip in Rust and slice out the FAT16 sector (no guest boot; UNVERIFIED). 3) Commit the blob with a GPL notice (simplest, fastest; needs the licence decision). The spike's `--boot-sector FILE` interface supports all three.

**Doc corrections to 5.1** (found while diffing): the D0b recipe's `mformat -F` forces **FAT32**, not FAT16 (the boot sector of that `c.img` starts `EB 58 90` with `FAT32   ` at 0x52) although the MBR type is 0x06; it booted anyway under FreeDOS, but the Rust builder produces a true FAT16 volume and boots too.

**OWNER DECISION (2026-11-18), closes the boot-sector licensing question:** the FreeDOS FAT16 boot-sector code blob is GPL-2.0-or-later and is **NOT committed to the repo** (options 2 and 3 above rejected). It is captured once at `dos setup` via option 1 (the proven guest `sys c:` step: build the image in Rust, boot the pinned `FD14BOOT.img`, QMP-type `sys c:`, read sector 0 back) and cached in the per-user data dir (`secure_fs`: 0700 dir, 0600 atomic file). The Rust builder takes the cached blob through its `--boot-sector FILE` interface.

**Decision: pure-Rust `fatfs` 0.3.6 + `fscommon` 0.1.1 under the `dos` feature on all OSes, no mtools shell-out.** Revisit only if `fatfs` fails on a real image (fallback: shell out to mtools on Unix). Status: image builder VERIFIED on Linux, macOS arm64, Windows x64 (build); boot-sector source = captured at setup, not committed (owner decision above, closed).

### 5.2 Windows (in v1 scope)

Test machines: win-devvm x64 (this Linux box) and win-devvm ARM64 (bigmac Parallels). Spike results so far are in 4.2 (install, curses listed, pmemsave over TCP: VERIFIED on x64). Remaining requirements, each UNVERIFIED until the D6/D6b task runs:
- **Pane bridge**: bilterm already uses `portable-pty` (ConPTY on Windows); a `Pty{argv}` plan with curses output is possible only if the QEMU build has curses. Default Windows path is `Bridge` with `qemu-textbuf` (4.4): QMP over a TCP loopback socket (`-qmp tcp:127.0.0.1:<port>,server=on,wait=off`, bind loopback only, random port) since Unix sockets are awkward from std on older Windows; token-less so restrict by loopback + per-session random port + process ownership.
- **Emulator install**: detect `qemu-system-i386.exe` on PATH and in `%ProgramFiles%\qemu`; guidance in `dos doctor`: `winget install -e --id SoftwareFreedomConservancy.QEMU` (VERIFIED 2026-11-18), `choco install qemu` (UNVERIFIED), the Stefan Weil installer, or MSYS2 pacman. `setup` never installs system software silently; with consent it can download the pinned installer `qemu-w64-setup-20260811.exe` (SHA256 in 0.1 item 5) and run only extraction into the private data dir.
- **Downloads without bash**: all fetch/verify/extract is Rust (rustls HTTPS, sha2, zip crate) inside `src/dos/`; no `scripts/*.sh` involvement on Windows. Keyboard: Windows Terminal passes VT input; map to QMP key names.
- **Paths**: `%LOCALAPPDATA%\bilterm\dos\` via the existing data-dir helper; `secure_fs` 0700/0600 semantics map to NTFS ACL inheritance (check what `secure_fs.rs` already does on Windows before D6; UNVERIFIED).
- **Image build** without mtools/sfdisk: the pure-Rust `fatfs` builder from 5.1a (VERIFIED building on win-devvm x64; mark the file sparse with `FSCTL_SET_SPARSE` first, NTFS otherwise allocates the full size).
- Windows ARM64: SUPPORTED, verified on win-devvm ARM64 (D6b); QEMU is the x64 build under Windows x64 emulation unless a native ARM64 build is found. 32-bit unsupported (QEMU states it).
- Validation plan (local, CI-free): x64: `cd /home/bill/src/infra/win-devvm && ./vm.sh start && ./vm.sh wait`, then `WIN_DEVVM_DIR=... scripts/build-windows-vm.sh <tag> --arch x64 --test` plus a `dos` smoke added to `scripts/windows-smoke-test.ps1` (qemu present, `-display help`, QMP pmemsave decode, image boot to FreeCom). ARM64: same with `--arch arm64` over `ssh bill@bigmac.local` (PATH `/usr/local/bin:/opt/homebrew/bin`). Remaining spike to run: `-display curses` under a real ConPTY (portable-pty) on both guests.

## 6. Uniform surface (every capability, every surface)

Per `CLAUDE.md`, every user-triggerable command lowers to `crate::action::Action` and runs through `WindowManager::apply` (`src/colon_command/execute_wm.rs`).

- New actions: `Action::DosRun{name, drive_c, image, backend}`, `DosSetup{..}`, `DosDoctor`, `DosRemove{..}`, `DosStatus`, `DosReset`, `DosSnapshot`, `DosRestore`. `DosSetup` is long-running, so it is an async job whose progress is a status line plus RPC events, not a blocking `apply`.
- Parity matrix (all UNVERIFIED until D3 builds them):

| Capability | Colon | CLI | RPC `ctl` | MCP | Key |
|---|---|---|---|---|---|
| open DOS pane | `:dos run [--drive-c DIR]` | `bilterm dos run` | `dos.run` | `dos_run` | palette entry |
| setup/update | `:dos setup` | `bilterm dos setup` | `dos.setup` (+ progress notifications) | `dos_setup` | n/a |
| doctor | `:dos doctor` | `bilterm dos doctor` | `dos.doctor` | `dos_doctor` | n/a |
| remove | `:dos remove` | `bilterm dos remove` | `dos.remove` | `dos_remove` | n/a |
| status | `:dos status` | `bilterm dos status` | `dos.status` | `dos_status` | status bar |
| reset/snapshot/restore | `:dos reset\|snapshot\|restore` | `bilterm dos reset\|snapshot\|restore` | `dos.reset`, `dos.snapshot`, `dos.restore` | `dos_reset`, `dos_snapshot`, `dos_restore` | n/a |
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
- Persistence: images are untrusted input on reload (a shared/imported or corrupted `c.img` is guest-controlled data): never mount it on the host except via our bounded FAT reader; validate size against the cap and MBR signature; snapshots/restore use `validate_safe_name` and stay inside the private dir (no symlink following).
- QMP/control endpoints: Unix socket under the 0700 dir (0600), or on Windows a loopback-only random port; closed when the session ends; no monitor on stdio; never exposed over bilterm RPC beyond the fixed key/text/snapshot verbs.
- Windows: no shell invocation, argv vectors only; installer launches (winget/choco/msi) only on explicit per-run consent and never elevated by bilterm.
- Backend probing runs `<emulator> -version`/`-display help` only; output is parsed with length caps.
- Untrusted text from the guest passes through the normal `VtParser` (OSC 52 and graphics caps already bounded).

## 9. Testing plan

- **Unit** (no network, no emulator): manifest parse and schema; hash verification (good, bad, truncated); resume logic with a mock `Downloader`; zip member extraction with traversal and symlink fixtures; argv builder snapshot tests (`insta`) for each backend; `doctor` against a fake probe; refusal as root; path validation.
- **Headless WM**: `WindowManager::new_headless` + offline-fixture spawner printing a canned 80x25 screen; assert grid text, `pane.read`, `send_keys` echo, Action/colon/RPC parity (a table-driven test that every row of section 6 exists).
- **Ignored real smoke** (`#[ignore]`, `BILTERM_DOS_SMOKE=1`): requires installed QEMU and cached floppy edition; boots, waits for "FreeCom" in the grid, sends a command, asserts echo. Mirrors the D0 spike. Never in `scripts/verify.sh` by default.
- **Backend conformance**: one suite runs against the `mock` backend always, and each real backend under `#[ignore]` + `BILTERM_DOS_SMOKE=1`; includes the text-buffer decoder (golden 4000-byte VGA fixtures from the D0b spike, CP437 and attribute mapping, graphics-mode detection) and QMP framing fuzz/limits.
- **Persistent image**: build recipe test with the pure-Rust writer (FAT/MBR structure assertions, no emulator), plus ignored smoke that boots the built `c.img` to the FreeCom prompt; snapshot/restore/reset round-trips; dirty-marker and corruption fixtures; lock contention.
- **Per-OS (all real machines, local)**: Linux dev box; Windows x64 `win-devvm` (`./vm.sh`); Windows ARM64 `win-devvm` and macOS arm64 + Intel on `ssh bill@bigmac.local` (`x86_64-apple-darwin` via `cargo test --target x86_64-apple-darwin` under Rosetta); BSD only if a guest is added (D2d). Each runs the ignored smoke plus image-build test. Also: matrix table test for `detect()` ordering using fake `HostEnv` per OS; `doctor` output snapshots for unsupported OS; Windows path/ACL tests run on the Windows host manually (D6) and through `cfg(windows)` unit tests.
- **Feature flag**: `cargo check` with and without `--features dos`; default build must not reference `src/dos`.
- Local CI only (owner preference); no hosted-CI downloads of 24-300 MB images.

## 10. PR breakdown

| PR | Scope | Exit criteria |
|---|---|---|
| D0 | Spike script (not wired in): QEMU curses boot and key injection. DONE for Linux (4.1) and macOS `-display help` (4.2) | results in this doc |
| D0b | Persistent-install spike: bounded to ONE script (`scripts/dos-spike-install.sh`, not in verify) that builds `c.img` per 5.1. Linux recipe PROVEN 2026-11-18; packages + AUTOEXEC + reboot persistence now VERIFIED on Linux (2026-11-18); macOS stops at missing sfdisk (exit 3). macOS run: `ssh bill@bigmac.local 'export PATH=/usr/local/bin:/opt/homebrew/bin:$PATH; brew install mtools && cd ~/worktrees/<wt> && scripts/dos-spike-install.sh'` (sfdisk absent on macOS: use the D0c Rust builder or `fdisk -e`) | script reproduces a bootable `c.img` on Linux and macOS; time box 1 day |
| D0c | Pure-Rust image builder spike (`fatfs` + hand-written MBR). **DONE 2026-11-18**, see 5.1a: builds a bootable sparse FAT16 `c.img` with no mtools/sfdisk/dd; boots to FreeCom + AUTOEXEC on Linux, macOS arm64, and (image built on) Windows x64; decision: `fatfs` 0.3.6 + `fscommon`. Boot-sector source decided: captured at `dos setup`, not committed (5.1a). Remaining: Bochs `term` and textbuf-poll runs UNVERIFIED (not run) | image boots VERIFIED; Windows-viable VERIFIED (x64 build+image); crate decision recorded |
| D1 | `dos` feature flag, `src/dos/{manifest,download,verify,paths}.rs`, `manifest.toml`, mock downloader, `bilterm dos setup/remove/doctor` (CLI) | unit tests; `cargo check --features dos`; real setup on Linux |
| D1b | `DosBackend` trait, `HostEnv`, per-OS `detect()`, mock backend, conformance suite | suite green on mock |
| D1c | Persistent image: builder, boot-sector capture at `dos setup` (guest `sys c:` via QMP, cached in the per-user data dir with `secure_fs` 0700/0600; blob NOT committed, GPL-2.0+), `reset/snapshot/restore`, lock, dirty marker, corruption path | image tests green; ignored smoke boots |
| D2a | `qemu-curses` backend + `dos run` + pane spawn via `WindowManager::apply`, min size, `-nic none` | headless fixture + ignored smoke (Linux, macOS) |
| D2b | `qemu-textbuf` backend: QMP client, text decoder, `Bridge` pane source, graphics-mode notice | golden-fixture tests; smoke on Linux |
| D2c | `bochs-term` backend (Linux/BSD; optional) | smoke or documented skip |
| D2d | BSD best-effort: FreeBSD/OpenBSD detection rows, doctor messages; first add a FreeBSD and an OpenBSD guest to win-devvm/libvirt (small infra task, separate repo) and run `dos doctor` + smoke there | rows verified, else stay best-effort |
| D3 | `Action::Dos*`, `:dos ...`, RPC `dos.*`, MCP tools, status line, parity table test | parity test green, `ctl` e2e |
| D6 | **Windows x64**: ConPTY/Bridge wiring, loopback QMP (spike VERIFIED), winget guidance and consented pinned installer download, path/ACL handling, pure-Rust builder, boot-sector capture via guest `sys c:` at setup (QMP over loopback; blob cached, not committed), no-bash setup | `build-windows-vm.sh --arch x64 --test` + dos smoke green on win-devvm x64; `-display curses` under ConPTY result recorded |
| D6b | **Windows ARM64**: same suite via bigmac Parallels win-devvm (x64 QEMU under emulation; speed measured) | `--arch arm64 --test` + dos smoke green |
| D7a | **macOS arm64**: `dos run`/image build/smoke on bigmac (throwaway worktree) | ignored smoke green |
| D7b | **macOS Intel**: `x86_64-apple-darwin` build + tests under Rosetta on bigmac; native Intel QEMU unavailable, so record that gap explicitly or add an Intel Homebrew under `/usr/local` (Rosetta) | tests green; gap documented or closed |
| D4 | Fun features (7.1 to 7.8); graphics via `screendump` -> image protocols is a post-v1 item here | per-feature tests |
| D5 | docs: `docs/DEVELOPER_AND_AGENT_GUIDE.md`, `AGENTS.md` pointer, third-party notices, `--help` text, matrix | `keeping-docs-in-sync` check |

Order: D0b, D0c -> D1 -> D1b -> D1c, D2a, D2b (parallel) -> D3 -> D6, D6b, D7a, D7b (parallel, per-OS verification) -> D2c/D2d, D4, D5.

## 11. Open questions (remaining)

Former questions 1-5 are answered (section 0.1). Genuinely open:

1. BSD: add FreeBSD/OpenBSD guests to win-devvm/libvirt (D2d), or keep BSD documented best-effort indefinitely?
2. `setup --full` default image size (512 MiB proposed) and the exact package list: decided by D0b: 256 MiB, base+apps+archiver+drivers+net+tools+unix (~97 MiB used; see 5.1).

## Sources (fetched 2026-11-17 and 2026-11-18)

- https://www.freedos.org/download/ , https://www.freedos.org/about/ , https://freedos.org/download/announce.html
- https://download.freedos.org/1.4/verify.txt , https://download.freedos.org/1.4/readme.txt (inside zips)
- https://bochs.sourceforge.io/doc/docbook/user/bochsrc.html , https://bochs.sourceforge.io/doc/docbook/user/compiling.html , https://raw.githubusercontent.com/bochs-emu/Bochs/master/bochs/LICENSE
- https://raw.githubusercontent.com/dosbox-staging/dosbox-staging/main/LICENSE
- https://www.qemu.org/download/ , https://gitlab.com/qemu-project/qemu/-/raw/master/meson.build , https://www.qemu.org/docs/master/interop/qemu-qmp-ref.html , https://formulae.brew.sh/api/formula/qemu.json
- https://www.freshports.org/emulators/qemu/ , https://openports.pl/path/emulators/qemu
- GitHub API repo metadata for dosbox-x, dosbox-staging, dosemu2, 86Box, v86, qemu, Bochs; `brew info` for bochs, qemu, dosbox-x, dosbox-staging
- https://sunsite2.icm.edu.pl/packages/freedos/distributions/1.1/errata.txt , SeaBIOS licensing threads at mail.coreboot.org (search results only)
- First-hand: D0 and D0b spikes (Linux QEMU 10.2.1), macOS `qemu-system-i386 -display help` on bigmac.local (QEMU 11.1.3); 2026-11-18 (2): winget QEMU install + `-display help` + TCP QMP pmemsave on win-devvm x64 (QEMU 11.1.0), pmemsave and curses-in-pty on bigmac macOS arm64
