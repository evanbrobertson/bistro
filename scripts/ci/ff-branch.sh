#!/usr/bin/env bash
# Fast-forward a branch to a commit-ish. Refuses anything that is not a
# fast-forward, so a channel can never carry code that was not on main.
# Creates the branch if it does not exist.
#
# usage: ff-branch.sh <commit-ish> <branch>
set -euo pipefail

target="$1"
branch="$2"
sha="$(git rev-parse --verify "${target}^{commit}")"

git fetch --quiet origin "+refs/heads/${branch}:refs/remotes/origin/${branch}" 2>/dev/null || true

if git rev-parse --verify --quiet "refs/remotes/origin/${branch}" >/dev/null; then
  current="$(git rev-parse "refs/remotes/origin/${branch}")"
  if [[ "${current}" == "${sha}" ]]; then
    echo "${branch} is already at ${sha}."
    exit 0
  fi
  if ! git merge-base --is-ancestor "${current}" "${sha}"; then
    echo "Refusing: ${branch} (${current}) is not an ancestor of ${sha}." >&2
    exit 1
  fi
fi

git push origin "${sha}:refs/heads/${branch}"
