#!/usr/bin/env bash
# Serial merge queue for main, run from any checkout: for each PR number in
# order, rebase its branch onto origin/main in a temporary worktree (the
# ruleset wants branches up to date), force-push, wait for the checks, and
# rebase-merge (the branch is deleted on merge). Stops at the first rebase
# conflict, failed check, or refused merge, so the remaining PRs stay open.
#
#   scripts/merge-pr.sh 41 42 43
#
# GitHub's own merge queue is not available for repositories owned by a user
# account, which is why this script exists.
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

[ $# -gt 0 ] || { echo "usage: scripts/merge-pr.sh <pr number> [<pr number> ...]" >&2; exit 2; }

for pr in "$@"; do
    branch="$(gh pr view "$pr" --json headRefName,state --jq 'if .state != "OPEN" then error("PR \(.state)") else .headRefName end')"
    echo "=== PR $pr ($branch)"
    git fetch -q origin "main" "$branch"
    if git merge-base --is-ancestor origin/main "origin/$branch"; then
        echo "up to date with main"
    else
        wt="$(mktemp -d)/wt"
        git worktree add -q "$wt" "origin/$branch"
        if ! git -C "$wt" rebase -q origin/main; then
            echo "rebase conflict on PR $pr; resolve it on the branch and rerun" >&2
            git -C "$wt" rebase --abort || true
            git worktree remove --force "$wt"
            exit 1
        fi
        git -C "$wt" push -q --force-with-lease "origin" "HEAD:$branch"
        git worktree remove --force "$wt"
        echo "rebased and pushed"
        # The new head has no check runs for a moment; wait until CI reports.
        for _ in $(seq 1 30); do
            gh pr checks "$pr" --json bucket --jq 'length' 2>/dev/null | grep -qv '^0$' && break
            sleep 5
        done
    fi
    gh pr checks "$pr" --watch --interval 30 >/dev/null 2>&1 || true
    if ! gh pr checks "$pr" --json bucket --jq 'all(.[]; .bucket == "pass" or .bucket == "skipping")' | grep -q true; then
        echo "checks failed on PR $pr:" >&2
        gh pr checks "$pr" --json name,bucket --jq '.[] | select(.bucket != "pass") | "  \(.name): \(.bucket)"' >&2
        exit 1
    fi
    if ! gh pr merge "$pr" --rebase --delete-branch >/dev/null 2>&1; then
        echo "merge refused on PR $pr: $(gh pr view "$pr" --json mergeStateStatus --jq .mergeStateStatus)" >&2
        exit 1
    fi
    echo "merged PR $pr: $(gh pr view "$pr" --json mergeCommit --jq '.mergeCommit.oid[:7]')"
done
echo "all merged"
