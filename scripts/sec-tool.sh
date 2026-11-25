#!/usr/bin/env bash
# Optional security-tier tools for scripts/verify.sh: semgrep | cargo-audit | cargo-deny.
# A missing tool (or an unreachable advisory DB) prints `SKIPPED <stage>: <reason>` and exits 0;
# verify.sh lists those lines in target/verify/skips.txt. Real findings exit 1.
set -uo pipefail
cd "$(dirname "$0")/.."
stage="${1:-}"
skip() { echo "SKIPPED $stage: $1"; exit 0; }
offline_re='failed to fetch|couldn.t fetch|could not resolve|network|timed out|Connection refused|failed to open advisory|unable to (connect|resolve)'
case "$stage" in
  semgrep)
    command -v semgrep >/dev/null 2>&1 || skip "semgrep not installed (pipx install semgrep)"
    command -v python3 >/dev/null 2>&1 || skip "python3 not installed"
    exec python3 scripts/semgrep-gate.py ;;
  cargo-audit)
    cargo audit --version >/dev/null 2>&1 || skip "cargo-audit not installed (cargo install cargo-audit)"
    out=$(cargo audit --db "${CARGO_HOME:-$HOME/.cargo}/advisory-db" 2>&1); rc=$?
    printf '%s\n' "$out"
    [ $rc -eq 0 ] && exit 0
    printf '%s' "$out" | grep -qiE "$offline_re" && skip "advisory DB unreachable (offline?)"
    exit 1 ;;
  cargo-deny)
    cargo deny --version >/dev/null 2>&1 || skip "cargo-deny not installed (cargo install cargo-deny)"
    out=$(cargo deny check 2>&1); rc=$?
    printf '%s\n' "$out"
    [ $rc -eq 0 ] && exit 0
    printf '%s' "$out" | grep -qiE "$offline_re" && skip "advisory DB unreachable (offline?)"
    exit 1 ;;
  *) echo "usage: sec-tool.sh semgrep|cargo-audit|cargo-deny" >&2; exit 2 ;;
esac
