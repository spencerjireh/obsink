#!/usr/bin/env bash
#
# Commit-message policy, shared by the lefthook commit-msg hook and the `commits` CI job.
#
#   scripts/check-commit-msg.sh --file <path>           # one message (hook: {1} = .git/COMMIT_EDITMSG)
#   scripts/check-commit-msg.sh --range <base>..<head>  # every commit in a range (CI: the PR range)
#
# Format:  <type>(<scope>)?: <subject> (OBS-<n>)
#   type       feat | fix | refactor | test | perf | build | chore | docs | ci
#   scope      optional, lowercase [a-z0-9-]; several joined with "," or "/"  e.g. (cli,desktop)
#   (OBS-<n>)  required for feat/fix/refactor/test/perf/build, optional for chore/docs/ci;
#              several as "(OBS-12, OBS-13)"
# Only the subject line is checked. In --file mode, "fixup!"/"squash!" subjects pass so that
# `git commit --fixup` works; they must be autosquashed before the PR (CI rejects them).
# Merge commits fail: rebase branches onto main instead of merging main into them.

set -euo pipefail

TYPES_WITH_REF='feat|fix|refactor|test|perf|build'
TYPES_REF_OPTIONAL='chore|docs|ci'
SCOPE='(\([a-z0-9-]+([,/][a-z0-9-]+)*\))?'
REF='\(OBS-[0-9]+(, OBS-[0-9]+)*\)'
RE_WITH_REF="^(${TYPES_WITH_REF})${SCOPE}: .+ ${REF}$"
RE_REF_OPTIONAL="^(${TYPES_REF_OPTIONAL})${SCOPE}: .+$"

usage() {
    echo "usage: $0 --file <path> | --range <base>..<head>" >&2
    exit 2
}

explain() {
    cat >&2 <<'HELP'
expected:  <type>(<scope>)?: <subject> (OBS-<n>)
  type       feat|fix|refactor|test|perf|build|chore|docs|ci
  scope      optional, lowercase, e.g. (core) or (cli,desktop)
  (OBS-<n>)  required for feat|fix|refactor|test|perf|build; optional for chore|docs|ci
example:   fix(server): reject batch uploads over 50 MB (OBS-91)
HELP
}

check() {
    local subject="$1"
    [[ "$subject" =~ $RE_WITH_REF ]] || [[ "$subject" =~ $RE_REF_OPTIONAL ]]
}

[ $# -eq 2 ] || usage
mode="$1"
arg="$2"

case "$mode" in
    --file)
        [ -f "$arg" ] || { echo "no such file: $arg" >&2; exit 2; }
        # git passes the message with comment lines still present
        subject="$(sed -e '/^#/d' -e '/^[[:space:]]*$/d' "$arg" | head -n 1 || true)"
        case "$subject" in
            fixup!\ *|squash!\ *) exit 0 ;;
        esac
        if check "$subject"; then
            exit 0
        fi
        echo "commit message rejected: '$subject'" >&2
        explain
        exit 1
        ;;
    --range)
        status=0
        while IFS= read -r subject; do
            [ -n "$subject" ] || continue
            if check "$subject"; then
                printf 'ok    %s\n' "$subject"
            else
                printf 'FAIL  %s\n' "$subject" >&2
                status=1
            fi
        done < <(git log --format=%s "$arg")
        [ "$status" -eq 0 ] || explain
        exit "$status"
        ;;
    *)
        usage
        ;;
esac
