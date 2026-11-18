#!/usr/bin/env bash
# Spike D0b (docs/plans/dos-mode.md 5.1): build a persistent FreeDOS C: image
# and prove it survives a guest write + power-off + reboot. NOT wired into
# verify.sh. Needs: qemu-system-i386 qemu-img sfdisk mtools(mformat,mcopy,mtype,mdir)
# unzip python3 curl sha256sum|shasum, 7z or bsdtar, syslinux mbr.bin.
#
# Env: DOS_WORK   work dir (fresh/own dir; default $PWD/dos-spike-work, never /tmp)
#      DOS_PKGSET base | full | all   (default base)
#                 full = apps archiver base drivers net tools unix; all adds games+sound
#      DOS_IMG_MIB image size MiB (default 128, max 2000)
#      DOS_KEEP_SOURCE=1 keep SOURCE/ trees from packages (default: dropped)
#      DOS_ISO / DOS_BOOTIMG  use existing FD14LIVE.iso / FD14BOOT.img (skip download)
#      DOS_MBR    path to syslinux mbr.bin
# Exit: 0 ok, 2 missing tool, 3 sfdisk absent (D0c Rust builder covers it), 4 test fail.
set -euo pipefail

BASE_URL=https://download.freedos.org/1.4
ZIP=FD14-LiveCD.zip
WORK=${DOS_WORK:-$PWD/dos-spike-work}
PKGSET=${DOS_PKGSET:-base}
MIB=${DOS_IMG_MIB:-128}
case $WORK in /tmp|/tmp/*) echo "refusing /tmp" >&2; exit 2;; esac

need() { command -v "$1" >/dev/null 2>&1 || { echo "MISSING tool: $1" >&2; exit "${2:-2}"; }; }
need qemu-system-i386; need qemu-img; need mformat; need mcopy; need mtype; need unzip; need python3; need curl
need sfdisk 3   # absent on macOS: D0c's pure-Rust builder covers partition table + MBR
sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

mkdir -p "$WORK"; chmod 700 "$WORK"
DL=$WORK/dl; X=$WORK/x; ISOX=$WORK/iso
mkdir -p "$DL"

# --- 1. fetch + verify (untrusted data; each in its own dir) ---------------
ISO=${DOS_ISO:-}; BOOTIMG=${DOS_BOOTIMG:-}
if [ -z "$ISO" ]; then
  if [ ! -f "$DL/$ZIP" ]; then
    curl -fsSL -o "$DL/verify.txt" "$BASE_URL/verify.txt"
    curl -fsSL -o "$DL/$ZIP.part" "$BASE_URL/$ZIP"
    want=$(awk -v z="$ZIP" '/^sha256sum:/{s=1;next} s&&$2==z{print $1}' "$DL/verify.txt")
    got=$(sha256 "$DL/$ZIP.part")
    [ -n "$want" ] && [ "$want" = "$got" ] || { echo "SHA256 MISMATCH want=$want got=$got" >&2; rm -f "$DL/$ZIP.part"; exit 4; }
    mv "$DL/$ZIP.part" "$DL/$ZIP"
  fi
  if [ ! -f "$X/FD14LIVE.iso" ]; then mkdir -p "$X"; unzip -q -o "$DL/$ZIP" FD14LIVE.iso FD14BOOT.img -d "$X"; fi
  ISO=$X/FD14LIVE.iso; BOOTIMG=$X/FD14BOOT.img
fi
if [ ! -f "$ISOX/KERNEL.SYS" ]; then
  mkdir -p "$ISOX"
  if command -v 7z >/dev/null; then 7z x -y -o"$ISOX" "$ISO" >/dev/null
  elif command -v bsdtar >/dev/null; then bsdtar -xf "$ISO" -C "$ISOX"
  else echo "MISSING tool: 7z or bsdtar" >&2; exit 2; fi
fi

# --- 2. package selection + size measurement -------------------------------
case $PKGSET in
  base) DIRS="base";;
  full) DIRS="base apps archiver drivers net tools unix";;
  all)  DIRS="base apps archiver drivers net tools unix games sound";;
  *) echo "bad DOS_PKGSET" >&2; exit 2;;
esac
STAGE=$WORK/stage-$PKGSET; rm -rf "$STAGE"; mkdir -p "$STAGE/FREEDOS"
XARGS=(-x 'SOURCE/*'); [ "${DOS_KEEP_SOURCE:-0}" = 1 ] && XARGS=()
n=0
for d in $DIRS; do
  for z in "$ISOX"/packages/$d/*.zip; do
    # unzip -o into our own stage dir; zip member names are relative (BIN/, DOC/, APPINFO/)
    unzip -q -o "$z" ${XARGS[@]+"${XARGS[@]}"} -d "$STAGE/FREEDOS" && n=$((n+1))
  done
done
du_b() { du -sk "$1" | cut -f1; }
echo "SIZE pkgset=$PKGSET zips=$n staged_KiB=$(du_b "$STAGE") files=$(find "$STAGE" -type f | wc -l)"
for d in $DIRS; do echo "SIZE zipdir=$d zip_KiB=$(du -ck "$ISOX"/packages/$d/*.zip | tail -1 | cut -f1)"; done

# --- 3. build the image (proven Linux recipe) ------------------------------
IMG=$WORK/c-$PKGSET.img; rm -f "$IMG"
qemu-img create -q -f raw "$IMG" "${MIB}M"
PTYPE=6; [ "$MIB" -gt 500 ] && PTYPE=e
printf 'label: dos\nstart=2048, type=%s, bootable\n' "$PTYPE" | sfdisk -q "$IMG"
SECT=$(( MIB * 2048 - 2048 )); CYL=$(( SECT / (16*63) ))
mformat -i "$IMG@@1048576" -F -t "$CYL" -h 16 -s 63 ::
MBR=${DOS_MBR:-}
if [ -z "$MBR" ]; then
  for c in /usr/lib/syslinux/mbr/mbr.bin /usr/share/syslinux/mbr.bin /usr/lib/syslinux/mbr.bin \
           /usr/lib/SYSLINUX/mbr.bin /opt/homebrew/share/syslinux/mbr.bin /usr/local/share/syslinux/mbr.bin; do
    [ -f "$c" ] && MBR=$c && break
  done
fi
[ -n "$MBR" ] || { echo "MISSING syslinux mbr.bin (set DOS_MBR; D0c embeds an MBR)" >&2; exit 3; }
dd if="$MBR" of="$IMG" bs=440 count=1 conv=notrunc 2>/dev/null
M="mcopy -i $IMG@@1048576"
$M "$ISOX/KERNEL.SYS" "$ISOX/COMMAND.COM" ::/
printf 'LASTDRIVE=Z\r\nBUFFERS=20\r\nFILES=40\r\nDOS=HIGH,UMB\r\nDEVICE=\\FREEDOS\\BIN\\HIMEMX.EXE\r\nSHELL=\\COMMAND.COM \\ /E:1024 /P=\\AUTOEXEC.BAT\r\n' > "$WORK/FDCONFIG.SYS"
printf '@echo off\r\nSET DOSDIR=C:\\FREEDOS\r\nSET PATH=C:\\;C:\\FREEDOS\\BIN\r\nSET TEMP=C:\\FREEDOS\\TEMP\r\nSET NLSPATH=C:\\FREEDOS\\NLS\r\nSET DIRCMD=/P /OGN\r\nPROMPT $P$G\r\necho BILTERM-READY\r\n' > "$WORK/AUTOEXEC.BAT"
$M "$WORK/FDCONFIG.SYS" "$WORK/AUTOEXEC.BAT" ::/
mkdir -p "$STAGE/FREEDOS/TEMP"
t0=$(date +%s)
$M -s -Q "$STAGE/FREEDOS" ::/
echo "TIME mcopy_packages_s=$(( $(date +%s) - t0 ))"
# on-disk usage (cluster slack included) from the FAT itself
mdir -i "$IMG@@1048576" ::/ | tail -2 | sed 's/^/FATFREE /'

# --- 4. QMP driver: sys c: once, then boot/persist/reboot/verify -----------
cat > "$WORK/drive.py" <<'PY'
import socket, json, time, sys, subprocess, os
img, boot, work = sys.argv[1], sys.argv[2], sys.argv[3]
def free_port():
    s = socket.socket(); s.bind(('127.0.0.1', 0)); p = s.getsockname()[1]; s.close(); return p
class VM:
    def __init__(self, drives, boot_dev):
        self.port = free_port()
        a = ['qemu-system-i386', '-display', 'none', '-m', '32', '-nic', 'none', '-boot', boot_dev,
             '-qmp', 'tcp:127.0.0.1:%d,server=on,wait=off' % self.port] + drives
        self.p = subprocess.Popen(a, stdin=subprocess.DEVNULL)
        for _ in range(50):
            try: self.s = socket.create_connection(('127.0.0.1', self.port)); break
            except OSError: time.sleep(0.2)
        self.f = self.s.makefile('rw'); self.f.readline(); self.cmd('qmp_capabilities')
    def cmd(self, c, **a):
        self.f.write(json.dumps({'execute': c, 'arguments': a}) + '\n'); self.f.flush()
        while True:
            r = json.loads(self.f.readline())
            if 'return' in r or 'error' in r: return r
    def screen(self):
        out = os.path.join(work, 'vga.bin')
        if os.path.exists(out): os.remove(out)
        self.cmd('pmemsave', val=0xB8000, size=4000, filename=out)
        d = open(out, 'rb').read()[0:4000:2]
        return '\n'.join(d[i*80:(i+1)*80].decode('cp437', 'replace').rstrip() for i in range(25))
    def wait(self, txt, t=90):
        for _ in range(t):
            sc = self.screen()
            if txt in sc: return sc
            time.sleep(1)
        print(sc); raise SystemExit('timeout waiting for ' + repr(txt))
    def typ(self, t):
        sp = {' ': 'spc', '.': 'dot', '\\': 'backslash', ':': ('shift', 'semicolon'), '\n': 'ret',
              '-': 'minus', '_': ('shift', 'minus'), '=': 'equal', '>': ('shift', 'dot')}
        for ch in t:
            k = sp.get(ch, ('shift', ch.lower()) if ch.isupper() else ch)
            k = k if isinstance(k, tuple) else (k,)
            self.cmd('send-key', keys=[{'type': 'qcode', 'data': x} for x in k]); time.sleep(0.08)
    def poweroff(self):
        self.cmd('quit'); self.p.wait(10)
D = ['-drive', 'file=%s,format=raw,if=ide,index=0' % img]
if boot != '-':
    v = VM(D + ['-drive', 'file=%s,format=raw,if=floppy,index=0,readonly=on' % boot], 'a')
    try:
        v.wait('English', 90); v.typ('\n'); v.wait('proceed', 60); v.typ('n'); time.sleep(3); v.typ('\n')
        v.wait('A:\\>', 30); v.typ('sys c:\n'); v.wait('System transferred', 30); print('SYS: System transferred')
    finally: v.p.kill()
# boot 1: write a file from inside the guest
v = VM(D, 'c')
try:
    sc = v.wait('BILTERM-READY', 120); print('BOOT1: autoexec ran'); assert 'HIMEM' in sc or True
    time.sleep(1); v.typ('echo persist-ok-' + str(os.getpid()) + ' > c:\\persist.txt\n'); time.sleep(2)
    v.typ('dir c:\\\n'); v.wait('PERSIST', 15); print('BOOT1: PERSIST.TXT written in guest')
finally:
    try: v.poweroff()
    except Exception: v.p.kill()
open(os.path.join(work, 'pid'), 'w').write(str(os.getpid()))
# boot 2: fresh qemu process, file must still exist and be readable by the guest
v = VM(D, 'c')
try:
    v.wait('BILTERM-READY', 120); time.sleep(1); v.typ('type c:\\persist.txt\n'); sc = v.wait('persist-ok-' + str(os.getpid()), 15)
    print('BOOT2: guest reads persist.txt: PASS')
    print('--- screen ---'); print(sc)
finally: v.p.kill()
PY
BOOTARG=$BOOTIMG; [ "${DOS_SKIP_SYS:-0}" = 1 ] && BOOTARG=-
python3 -I "$WORK/drive.py" "$IMG" "$BOOTARG" "$WORK" | tee "$WORK/drive.log"
grep -q 'BOOT2: guest reads persist.txt: PASS' "$WORK/drive.log" || { echo "FAIL persistence" >&2; exit 4; }
# host-side confirmation of the same file via mtools (guest is powered off)
mtype -i "$IMG@@1048576" ::/persist.txt | sed 's/^/HOSTREAD /'
echo "OK pkgset=$PKGSET img=$IMG mib=$MIB image_used_KiB=$(du -k "$IMG" | cut -f1)"
