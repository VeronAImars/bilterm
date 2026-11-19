#!/usr/bin/env python3
"""Real-PTY smoke test for `bilterm attach` (P2 T-A8). Unix only, not part of verify.sh.

Starts `bilterm --headless --host-ptys --attach`, runs `bilterm attach <name>` inside a real PTY,
types a command, expects its output on the client screen, detaches with Ctrl-] d (exit 0), then
checks `--ro` input is not applied, and that killing the host gives a non-zero client exit.
Usage: scripts/attach_smoke.py [path/to/bilterm]   (default: $CARGO_TARGET_DIR or ./target debug)
"""
import os, pty, select, shutil, signal, subprocess, sys, tempfile, time, fcntl, termios, struct

def find_bin():
    if len(sys.argv) > 1:
        return sys.argv[1]
    tgt = os.environ.get("CARGO_TARGET_DIR", "target")
    return os.path.join(tgt, "debug", "bilterm")

BIN = os.path.realpath(find_bin())
NAME = "smoke%d" % os.getpid()
fails = []

def check(ok, what):
    print(("PASS " if ok else "FAIL ") + what)
    if not ok:
        fails.append(what)

def spawn_client(env, *extra):
    pid, fd = pty.fork()
    if pid == 0:
        os.execve(BIN, [BIN, "attach", NAME, *extra], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    return pid, fd

def read_until(fd, needle, timeout=15):
    buf, end = b"", time.time() + timeout
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.2)
        if r:
            try:
                d = os.read(fd, 65536)
            except OSError:
                break
            if not d:
                break
            buf += d
            if needle in buf:
                return True, buf
    return needle in buf, buf

def wait_exit(pid, timeout=10):
    end = time.time() + timeout
    while time.time() < end:
        p, st = os.waitpid(pid, os.WNOHANG)
        if p:
            return os.waitstatus_to_exitcode(st)
        time.sleep(0.1)
    os.kill(pid, signal.SIGKILL); os.waitpid(pid, 0)
    return None

rt = tempfile.mkdtemp(prefix="bt-smoke-", dir=os.environ.get("SMOKE_TMPDIR", "/datanapps" if os.path.isdir("/datanapps") else None))
os.chmod(rt, 0o700)
env = dict(os.environ, BILTERM_RUNTIME_DIR=rt, SHELL="/bin/sh", TERM="xterm-256color")
host = subprocess.Popen([BIN, "--headless", "--host-ptys", "--attach", "--name", NAME], env=env,
                        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    sock = os.path.join(rt, "host-%s.attach.sock" % NAME)
    end = time.time() + 30
    while not os.path.exists(sock) and time.time() < end:
        time.sleep(0.1)
    check(os.path.exists(sock), "host created attach socket")
    check(os.path.exists(sock) and (os.stat(sock).st_mode & 0o077) == 0, "socket mode is 0600-class (no group/other)")

    pid, fd = spawn_client(env)
    time.sleep(1.0)
    os.write(fd, b"echo SMOKE$((40+2))\n")
    ok, _ = read_until(fd, b"SMOKE42")
    check(ok, "rw client sees command output rendered from host pane")
    os.write(fd, b"\x1dd")
    check(wait_exit(pid) == 0, "Ctrl-] d detaches with exit 0")

    pid, fd = spawn_client(env, "--ro")
    time.sleep(1.0)
    os.write(fd, b"echo ROTEST$((1+1))\n")
    ok, _ = read_until(fd, b"ROTEST2", timeout=3)
    check(not ok, "--ro client input is not applied")
    os.write(fd, b"\x1dd")
    check(wait_exit(pid) == 0, "ro client detaches with exit 0")

    pid, fd = spawn_client(env)
    time.sleep(1.0)
    host.kill(); host.wait()
    code = wait_exit(pid, 30)
    check(code not in (None, 0), "client exits non-zero when host dies (got %r)" % (code,))
finally:
    if host.poll() is None:
        host.kill(); host.wait()
    shutil.rmtree(rt, ignore_errors=True)

print("attach smoke: %s" % ("FAIL (%d)" % len(fails) if fails else "PASS"))
sys.exit(1 if fails else 0)
