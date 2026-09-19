#!/usr/bin/env bash
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
work="$here/work"

if [[ -e "$work" ]]; then
  echo "refusing to replace $work; remove it before rebuilding" >&2
  exit 1
fi

mkdir -p "$work"
cp -R "$here/../config/." "$work/"

git -C "$work" init -q -b main
git -C "$work" config user.name "HackMIT Demo"
git -C "$work" config user.email "demo@localhost"
git -C "$work" add .
git -C "$work" commit -qm "base"

git -C "$work" switch -qc normalize
git -C "$work" apply "$here/normalize.patch"
git -C "$work" commit -qam "normalize before parsing"

git -C "$work" switch -qc logging main
git -C "$work" apply "$here/logging.patch"
git -C "$work" commit -qam "log the original input"

git -C "$work" merge --no-edit normalize
cargo check -q --manifest-path "$work/Cargo.toml"

first_raw=$(grep -n 'let raw = read' "$work/src/main.rs" | cut -d: -f1)
shadow_raw=$(grep -n 'let raw = normalize' "$work/src/main.rs" | cut -d: -f1)
logged_raw=$(grep -n 'log(&raw)' "$work/src/main.rs" | cut -d: -f1)
test "$first_raw" -lt "$shadow_raw"
test "$shadow_raw" -lt "$logged_raw"

echo "git merged cleanly; log(&raw) now resolves to the normalized shadow at line $shadow_raw"
