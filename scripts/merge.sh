#!/usr/bin/env bash
# Merges the current, already pushed branch into main: opens a pull request if
# there is none, runs `just check` here on exactly that commit, and
# squash-merges only a green result, so main gets one commit titled
# "<pull request title> (#N)".
#
# The repository has no CI, so this script is the gate, and GitHub enforces
# what the script cannot. The script does not push: pushing stays a separate,
# visible step.
#
# Required ruleset on main (repository settings; the script relies on it):
#   - restrict deletions and block force pushes;
#   - require a pull request before merging, squash as the only merge method;
#   - require the status check `skorupa/check`, with "Require branches to be
#     up to date before merging".
# After a green check the script posts `skorupa/check` on the exact head
# commit. The lock below serialises merges from this clone only; another
# clone or the web page could still move main between the last fetch and the
# merge. With the ruleset GitHub then refuses: the head no longer contains
# main, and a rebased head is a new commit without the status. So a merge
# that goes through squashes a head that contains main, and its tree is the
# tree that was checked. The script verifies that on the merge commit GitHub
# reports. Anyone with write access can post the status: it closes the race,
# it does not stop a collaborator who wants to bypass the gate.
#
# The check runs the definition found on the branch, so a branch that changes
# the gate (scripts, hooks, justfile, flake, toolchain, lint and dependency
# policy, budget, any Cargo.toml) could weaken its own check. Such a branch
# needs explicit consent: MERGE_GATE_CHANGE=1.
set -euo pipefail

fail() {
    echo "merge: $1" >&2
    exit 1
}

# One merge at a time. The lock file is in the shared .git directory, so it
# covers every worktree of this clone, and nobody moves main under a running
# check. The script holds the descriptor and its children inherit it, so the
# lock lasts as long as any of them lives. Network commands have timeouts, so
# a stuck child cannot hold it forever.
git_common=$(git rev-parse --path-format=absolute --git-common-dir) ||
    fail "run this inside the repository."
exec 9>"$git_common/skorupa-merge.lock"
flock --wait 7200 9 || fail "another merge has been running for too long."
# git prints paths relative to the top level.
cd "$(git rev-parse --show-toplevel)"

branch=$(git symbolic-ref --short HEAD) || fail "check out a branch first."
commit=$(git rev-parse HEAD)
[ "$branch" != main ] || fail "switch to a work branch; main is merged into only through a pull request."

clean() {
    local status
    status=$(git status --porcelain --untracked-files=all) || fail "cannot read the state of the work tree."
    [ -z "$status" ]
}
clean || fail "commit or stash your changes first, untracked files included."

fetch() {
    timeout --kill-after=5s 120 git fetch --quiet --no-auto-maintenance origin ||
        fail "cannot fetch from origin."
}
fetch
git merge-base --is-ancestor origin/main HEAD ||
    fail "the branch does not contain the latest main: git rebase origin/main, push, and run this again."
[ "$(git rev-list --count origin/main..HEAD)" -gt 0 ] || fail "nothing to merge."
[ "$(git rev-parse --verify --quiet "origin/$branch")" = "$commit" ] ||
    fail "push $branch to origin first."

# Cargo.toml files carry the lint tables and profiles of every workspace.
gate=$(git diff --name-only origin/main "$commit" -- scripts .githooks justfile \
    flake.nix flake.lock rust-toolchain.toml clippy.toml deny.toml budget.toml \
    ':(glob)**/Cargo.toml')
if [ -n "$gate" ] && [ "${MERGE_GATE_CHANGE:-}" != 1 ]; then
    fail "the branch changes the merge gate (${gate//$'\n'/ }). Review those changes and run with MERGE_GATE_CHANGE=1."
fi

# The check runs on a tree equal to the commit: the tree is clean before it,
# and afterwards we verify that nothing moved. The check formats and fixes
# nothing.
echo "Running just check for $(git log -1 --format='%h %s' "$commit")."
just check || fail "just check failed, not merging."
[ "$(git rev-parse HEAD)" = "$commit" ] || fail "HEAD moved during the check, run this again."
clean || fail "the check changed files in the work tree, not merging: see git status."

open_pr() {
    timeout --kill-after=5s 30 gh pr list --head "$branch" --base main --state open \
        --json number --jq '.[0].number // empty'
}
number=$(open_pr)
if [ -z "$number" ]; then
    timeout --kill-after=5s 60 gh pr create --base main --head "$branch" --fill
    number=$(open_pr)
fi
[[ "$number" =~ ^[0-9]+$ ]] || fail "found no open pull request for $branch."

# main may have moved and the branch on origin may have changed meanwhile.
fetch
git merge-base --is-ancestor origin/main "$commit" ||
    fail "main changed during the check: git rebase origin/main, push, and run this again."
base=$(git rev-parse origin/main)
pr_now=$(timeout --kill-after=5s 30 gh pr view "$number" --json baseRefName,headRefOid \
    --jq '.baseRefName + " " + .headRefOid') ||
    fail "cannot read pull request #$number, not merging."
[ "$pr_now" = "main $commit" ] ||
    fail "the base or head of pull request #$number is not the checked commit, not merging."
title=$(timeout --kill-after=5s 30 gh pr view "$number" --json title --jq .title) ||
    fail "cannot read the title of pull request #$number, not merging."
[ -n "$title" ] || fail "pull request #$number has no title, not merging."

# The status is what the ruleset on main requires. It names the exact commit,
# so it says nothing about any other head of this pull request.
timeout --kill-after=5s 30 gh api --method POST "repos/{owner}/{repo}/statuses/$commit" \
    -f state=success -f context=skorupa/check \
    -f description="just check passed (fmt, clippy, tests, budget, req-lint, deny) on base ${base:0:12}" \
    >/dev/null ||
    fail "cannot post the skorupa/check status on $commit, not merging."
# The comment is the trace for readers: which commit, on which base.
timeout --kill-after=5s 30 gh pr comment "$number" --body \
    "\`just check\` passed locally for $commit on base $base." >/dev/null ||
    fail "cannot leave the check result on pull request #$number, not merging."
# GitHub refuses the merge if the head is no longer $commit, and, with the
# ruleset, if the head does not contain the current main.
timeout --kill-after=5s 120 gh pr merge "$number" --squash --match-head-commit "$commit" \
    --subject "$title (#$number)" ||
    fail "GitHub did not merge pull request #$number."

# Verify the commit GitHub made for this pull request, not whatever main
# points at by now: it must carry exactly the tree that was checked.
unverified="pull request #$number may be MERGED but the result is NOT VERIFIED"
merged=$(timeout --kill-after=5s 30 gh pr view "$number" --json state,mergeCommit \
    --jq '.state + " " + (.mergeCommit.oid // "")') ||
    fail "$unverified: cannot read its state."
[[ "$merged" =~ ^MERGED\ ([0-9a-f]{40,64})$ ]] ||
    fail "$unverified: GitHub reports '$merged' instead of a merge commit."
merge_commit=${BASH_REMATCH[1]}
fetch
git cat-file -e "$merge_commit^{commit}" 2>/dev/null ||
    fail "$unverified: merge commit $merge_commit is not on origin."
if [ "$(git rev-parse "$merge_commit^{tree}")" != "$(git rev-parse "$commit^{tree}")" ]; then
    fail "pull request #$number is MERGED as $merge_commit with a tree that was NOT checked. Run just check on main now."
fi
echo "Merged: $(git log -1 --format='%h %s' "$merge_commit"), tree as checked."
