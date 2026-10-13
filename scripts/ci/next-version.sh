#!/usr/bin/env bash
# Print the next release version (no leading v) from the Conventional Commits
# since the last tag. Print nothing if there is nothing to release.
#
# Rules (0.x): breaking change or feat -> minor, fix or perf -> patch.
# From 1.0 on: breaking -> major, feat -> minor, fix or perf -> patch.
set -euo pipefail

last="$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || true)"
if [[ -z "${last}" ]]; then
  range="HEAD"
  major=0 minor=0 patch=0
else
  range="${last}..HEAD"
  IFS=. read -r major minor patch <<<"${last#v}"
fi

bump=""
while IFS= read -r -d '' msg; do
  subject="${msg%%$'\n'*}"
  if [[ "${subject}" =~ ^[a-z]+(\([^\)]+\))?!: ]] || grep -q '^BREAKING[ -]CHANGE:' <<<"${msg}"; then
    bump="breaking"
  elif [[ "${subject}" =~ ^feat(\([^\)]+\))?: ]]; then
    [[ "${bump}" == "breaking" ]] || bump="feat"
  elif [[ "${subject}" =~ ^(fix|perf)(\([^\)]+\))?: ]]; then
    [[ -n "${bump}" ]] || bump="fix"
  fi
done < <(git log --format='%B%x00' "${range}")

[[ -n "${bump}" ]] || exit 0

if [[ -z "${last}" ]]; then
  echo "0.1.0"
  exit 0
fi

case "${bump}" in
  breaking) if [[ "${major}" -ge 1 ]]; then major=$((major + 1)) minor=0 patch=0; else minor=$((minor + 1)) patch=0; fi ;;
  feat) minor=$((minor + 1)); patch=0 ;;
  fix) patch=$((patch + 1)) ;;
esac
echo "${major}.${minor}.${patch}"
