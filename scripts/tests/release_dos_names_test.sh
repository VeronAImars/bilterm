#!/usr/bin/env bash
# Name generation / alias skipping / feature composition for the release scripts' --dos switch.
# No compiling: uses lib/release-dos.sh and the scripts' --dry-run paths.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"; S="$HERE/.."
# shellcheck source=../lib/release-dos.sh
source "$S/lib/release-dos.sh"
fail=0
eq() { if [[ "$2" == "$3" ]]; then echo "ok   $1"; else echo "FAIL $1: got [$2] want [$3]"; fail=1; fi; }

unset BILTERM_FEATURES
release_dos_init
eq "default not dos" "$RELEASE_DOS|$RELEASE_FEATURES" "0|"
eq "normal name" "$(release_archive_name v1.2.3 x86_64-unknown-linux-gnu)" "bilterm-v1.2.3-x86_64-unknown-linux-gnu"
eq "normal alias" "$(release_alias_name x86_64-unknown-linux-gnu)" "bilterm-x86_64-unknown-linux-gnu"
release_dos_init --dos
eq "--dos" "$RELEASE_DOS|$RELEASE_FEATURES" "1|dos"
eq "dos name" "$(release_archive_name v1.2.3 x86_64-pc-windows-msvc)" "bilterm-v1.2.3-dos-x86_64-pc-windows-msvc"
eq "dos alias skipped" "$(release_alias_name x86_64-pc-windows-msvc)" ""
BILTERM_FEATURES=dos release_dos_init
eq "env dos" "$RELEASE_DOS|$RELEASE_FEATURES" "1|dos"
BILTERM_FEATURES="foo,bar" release_dos_init --dos
eq "compose" "$RELEASE_DOS|$RELEASE_FEATURES" "1|foo,bar,dos"
BILTERM_FEATURES="dos foo dos" release_dos_init --dos
eq "dedupe" "$RELEASE_FEATURES" "dos,foo"
BILTERM_FEATURES="foo" release_dos_init
eq "other features only" "$RELEASE_DOS|$RELEASE_FEATURES" "0|foo"

# Dry-run through each script.
out="$("$S/build-release-assets.sh" v9.9.9 --dos --dry-run)"
grep -q 'archive: bilterm-v9.9.9-dos-x86_64-unknown-linux-gnu.tar.gz' <<<"$out" && grep -q -- '--features dos' <<<"$out" \
    && ! grep -qE 'alias:   bilterm-' <<<"$out"; eq "linux dry-run dos" "$?" 0
out="$("$S/build-release-assets.sh" v9.9.9 --dry-run)"
grep -q 'alias:   bilterm-x86_64-unknown-linux-gnu.tar.gz' <<<"$out" && ! grep -q -- '-dos-\|--features' <<<"$out"; eq "linux dry-run normal" "$?" 0
out="$(BILTERM_FEATURES=dos "$S/build-release-mac-win.sh" v9.9.9 --windows --windows-arm64 --dry-run)"
grep -q 'bilterm-v9.9.9-dos-aarch64-pc-windows-msvc.zip' <<<"$out" && ! grep -qE 'alias:   bilterm-' <<<"$out"; eq "win dry-run dos" "$?" 0
out="$("$S/build-release-mac-win.sh" v9.9.9 --dos --dry-run)"
grep -q 'bilterm-v9.9.9-dos-aarch64-apple-darwin.tar.gz' <<<"$out" && ! grep -qE 'alias:   bilterm-' <<<"$out"; eq "mac dry-run dos" "$?" 0
bash -n "$S/build-windows-vm.sh"; eq "vm script syntax" "$?" 0
grep -q -- '--dos' "$S/build-windows-vm.sh"; eq "vm script has --dos" "$?" 0
exit $fail
