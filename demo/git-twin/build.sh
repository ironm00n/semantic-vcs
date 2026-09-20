#!/usr/bin/env bash
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
work="$here/work"

verify() {
  local first_raw shadow_raw logged_raw
  test "$(git -C "$work" rev-parse --show-toplevel)" = "$work" || return 1
  test "$(git -C "$work" rev-list --parents -n 1 HEAD | wc -w)" -eq 3 || return 1
  git -C "$work" diff --quiet HEAD -- || return 1
  first_raw=$(grep -n '^[[:space:]]*let raw = read(' "$work/src/main.rs" | cut -d: -f1) || return 1
  shadow_raw=$(grep -n '^[[:space:]]*let raw = normalize(' "$work/src/main.rs" | cut -d: -f1) || return 1
  logged_raw=$(grep -n '^[[:space:]]*log(&raw);' "$work/src/main.rs" | cut -d: -f1) || return 1
  test "$first_raw" -lt "$shadow_raw" && test "$shadow_raw" -lt "$logged_raw" || return 1
  cargo check -q --manifest-path "$work/Cargo.toml" || return 1
  echo "git merged cleanly; log(&raw) now resolves to the normalized shadow at line $shadow_raw"
}

if [[ -e "$work" ]]; then
  if verify; then
    exit 0
  fi
  echo "refusing to replace unrecognized $work; remove it before rebuilding" >&2
  exit 1
fi

mkdir -p "$work"
cp -R "$here/../config/." "$work/"

export GIT_AUTHOR_NAME="HackMIT Demo" GIT_AUTHOR_EMAIL="demo@localhost"
export GIT_COMMITTER_NAME="$GIT_AUTHOR_NAME" GIT_COMMITTER_EMAIL="$GIT_AUTHOR_EMAIL"
git -C "$work" init -q -b main
git -C "$work" add .
git -C "$work" commit -qm "base"

git -C "$work" switch -qc normalize
git -C "$work" apply "$here/normalize.patch"
git -C "$work" commit -qam "normalize before parsing"

git -C "$work" switch -qc logging main
git -C "$work" apply "$here/logging.patch"
git -C "$work" commit -qam "log the original input"

git -C "$work" merge --no-edit normalize
verify
