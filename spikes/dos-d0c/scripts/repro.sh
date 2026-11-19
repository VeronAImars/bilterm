#!/usr/bin/env bash
# D0c end-to-end repro (Linux). Needs: cargo, qemu-system-i386, python3, unzip, mtools' mcopy ONLY to
# read files out of the downloaded floppy image (not used to build the C: image). Everything under $WORK.
# usage: WORK=/datanapps/dos-spike/d0c-repro scripts/repro.sh
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="${WORK:?set WORK to a fresh dir on roomy storage (not /tmp)}"
[ -z "$(ls -A "$WORK" 2>/dev/null)" ] || { echo "WORK must be empty/new" >&2; exit 1; }
mkdir -p "$WORK/dl" "$WORK/fl" "$WORK/ex" "$WORK/out"
ZIP=FD14-FloppyEdition.zip
SHA=45b1fa7c52dd996c3bfa5e352ffcd410781b952a6ad629f15a4c9ec4bbaefc5a   # = https://download.freedos.org/1.4/verify.txt
curl -sSfL -o "$WORK/dl/$ZIP" "https://download.freedos.org/1.4/$ZIP"
echo "$SHA  $WORK/dl/$ZIP" | sha256sum -c -
unzip -q "$WORK/dl/$ZIP" -d "$WORK/fl"
F="$WORK/fl/144m/x86BOOT.img"
mcopy -i "$F" ::/KERNEL.SYS ::/freedos/bin/COMMAND.COM "$WORK/ex/"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$WORK/target}"
(cd "$HERE" && cargo build --release -q)
B="$CARGO_TARGET_DIR/release/dos-d0c"; export D0C_TMP="$WORK/out"
# 1. build WITHOUT FreeDOS boot code, 2. let the guest's own `sys c:` write it, 3. capture the sector
"$B" build --out "$WORK/out/seed.img" --kernel "$WORK/ex/KERNEL.SYS" --command "$WORK/ex/COMMAND.COM"
python3 "$HERE/scripts/qemu_drive.py" sys "$WORK/out/seed.img" "$F" >/dev/null
"$B" extract-bs --image "$WORK/out/seed.img" --out "$WORK/out/bs-fd16.bin"
# 4. pure-Rust build with the captured sector, then prove it boots
"$B" build --out "$WORK/out/c.img" --kernel "$WORK/ex/KERNEL.SYS" --command "$WORK/ex/COMMAND.COM" --boot-sector "$WORK/out/bs-fd16.bin"
python3 "$HERE/scripts/qemu_drive.py" boot "$WORK/out/c.img"
python3 "$HERE/scripts/qemu_drive.py" curses "$WORK/out/c.img"
echo "D0C REPRO PASS"
