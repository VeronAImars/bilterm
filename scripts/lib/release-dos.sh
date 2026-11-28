#!/usr/bin/env bash
# Shared helpers for the release scripts' DOS-mode switch (sourced, never run).
#   release_dos_init  [--dos]   -> sets RELEASE_DOS (0|1) and RELEASE_FEATURES (comma list)
#   release_archive_name TAG TRIPLE   -> bilterm-<TAG>[-dos]-<TRIPLE>
#   release_alias_name TRIPLE         -> bilterm-<TRIPLE>, or empty for DOS builds
#   release_cargo_feature_args        -> prints "--features a,b" (or nothing)
# Features come from $BILTERM_FEATURES (comma/space separated, may already hold
# other features); `--dos` appends `dos`. DOS is on iff `dos` is in the list.

release_dos_init() {
    local want_dos=0 a f
    for a in "$@"; do [[ "$a" == "--dos" ]] && want_dos=1; done
    local list="${BILTERM_FEATURES:-}" out=() seen=" "
    list="${list//,/ }"
    for f in $list; do
        [[ "$seen" == *" $f "* ]] && continue
        seen+="$f "; out+=("$f")
    done
    if [[ $want_dos -eq 1 && "$seen" != *" dos "* ]]; then out+=(dos); seen+="dos "; fi
    RELEASE_DOS=0; [[ "$seen" == *" dos "* ]] && RELEASE_DOS=1
    RELEASE_FEATURES=""
    if [[ ${#out[@]} -gt 0 ]]; then RELEASE_FEATURES="$(IFS=,; echo "${out[*]}")"; fi
}

release_archive_name() {
    if [[ "${RELEASE_DOS:-0}" == 1 ]]; then echo "bilterm-$1-dos-$2"; else echo "bilterm-$1-$2"; fi
}

# Unversioned installer alias ("latest" assets). Empty (skipped) for DOS builds.
release_alias_name() {
    if [[ "${RELEASE_DOS:-0}" == 1 ]]; then return 0; fi
    echo "bilterm-$1"
}

release_cargo_feature_args() {
    if [[ -n "${RELEASE_FEATURES:-}" ]]; then echo "--features $RELEASE_FEATURES"; fi
}
