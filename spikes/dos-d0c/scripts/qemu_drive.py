#!/usr/bin/env python3
"""Spike D0c QEMU driver (QMP over a unix socket, VGA text read via pmemsave 0xB8000).

  qemu_drive.py sys   IMG FLOPPY   boot FLOPPY with IMG attached, run `sys c:` (captures the FreeDOS boot sector)
  qemu_drive.py boot  IMG          boot IMG alone, print the 80x25 screen once FreeCom prompt appears
  qemu_drive.py curses IMG         boot IMG alone under -display curses in a PTY, print final screen bytes
Exit 0 only if the expected text was seen.
"""
import json, os, pty, select, socket, struct, subprocess, sys, tempfile, time, fcntl, termios

QEMU = os.environ.get("QEMU", "qemu-system-i386")
work = tempfile.mkdtemp(prefix="d0c-", dir=os.environ.get("D0C_TMP", "."))
sock_path = os.path.join(work, "qmp.sock")
vga = os.path.join(work, "vga.bin")


def start(args):
    p = subprocess.Popen([QEMU, "-display", "none", "-m", "32", "-nic", "none", "-no-reboot",
                          "-qmp", f"unix:{sock_path},server=on,wait=off"] + args, stdin=subprocess.DEVNULL)
    for _ in range(50):
        if os.path.exists(sock_path):
            break
        time.sleep(0.1)
    s = socket.socket(socket.AF_UNIX); s.connect(sock_path); f = s.makefile("rw"); f.readline(); cmd(f, "qmp_capabilities")
    return p, f


def cmd(f, c, **a):
    f.write(json.dumps({"execute": c, "arguments": a}) + "\n"); f.flush()
    while True:
        r = json.loads(f.readline())
        if "return" in r or "error" in r:
            return r


def screen(f):
    if os.path.exists(vga):
        os.remove(vga)
    r = cmd(f, "pmemsave", val=0xB8000, size=4000, filename=vga)
    if "error" in r or not os.path.exists(vga):
        return "" if not os.environ.get("D0C_DEBUG") else f"ERR {r}"
    d = open(vga, "rb").read()[0:4000:2]
    return "\n".join(d[i * 80:(i + 1) * 80].decode("cp437", "replace").rstrip() for i in range(25))


KM = {" ": "spc", ".": "dot", "\\": "backslash", "/": "slash", ":": "shift-semicolon", "\n": "ret", "-": "minus"}


def typ(f, t):
    for ch in t:
        k = KM.get(ch, ch.lower() if not ch.isupper() else "shift-" + ch.lower())
        cmd(f, "human-monitor-command", **{"command-line": "sendkey " + k}); time.sleep(0.08)


def wait(f, txt, t=90):
    sc = ""
    for _ in range(t):
        sc = screen(f)
        if txt in sc:
            return sc
        time.sleep(1)
    print(sc); sys.exit(f"timeout waiting for {txt!r}")


def drive(img, extra=()):
    return ["-drive", f"file={img},format=raw,if=ide", *extra]


mode = sys.argv[1]
if mode == "sys":
    img, floppy = sys.argv[2], sys.argv[3]
    p, f = start(drive(img, ["-drive", f"file={floppy},format=raw,if=floppy"]) + ["-boot", "a"])
    try:
        wait(f, "English", 90); typ(f, "\n")
        wait(f, "proceed", 60); typ(f, "n"); time.sleep(3); typ(f, "\n"); time.sleep(5)
        typ(f, "sys c:\n")
        sc = wait(f, "System transferred", 30); print(sc)
    finally:
        p.kill()
elif mode == "boot":
    img = sys.argv[2]
    p, f = start(drive(img) + ["-boot", "c"])
    try:
        sc = wait(f, "AUTOEXEC RAN", 60); time.sleep(3); sc = screen(f); print(sc)
        ok = "C:\\>" in sc
        sys.exit(0 if ok else "no FreeCom prompt")
    finally:
        p.kill()
elif mode == "curses":
    img = sys.argv[2]
    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm"
        os.execvp(QEMU, [QEMU, "-display", "curses", "-m", "32", "-nic", "none", "-no-reboot",
                         "-drive", f"file={img},format=raw,if=ide", "-boot", "c"])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    buf = b""; t0 = time.time()
    while time.time() - t0 < 25:
        r, _, _ = select.select([fd], [], [], 0.5)
        if r:
            try: buf += os.read(fd, 65536)
            except OSError: break
    os.kill(pid, 9)
    txt = buf.decode("cp437", "replace")
    print(len(buf), "bytes; AUTOEXEC RAN:", "AUTOEXEC RAN" in txt, "; prompt C:\\>:", "C:\\>" in txt)
    sys.exit(0 if "AUTOEXEC RAN" in txt else 1)
