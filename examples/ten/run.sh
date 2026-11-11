#!/bin/sh
# Runs the ten demo cards in order on a fresh copy of start/.
# Usage: examples/ten/run.sh <path-to-sayraap-binary> <empty-work-folder> [extra sayraap options]
# Example extra options: --check "cargo check --quiet"
# Every card must end in exactly one new commit or in a clean rollback.
set -u
here=$(cd "$(dirname "$0")" && pwd)
bin=$1
work=$2
shift 2
mkdir -p "$work"
cp -R "$here/start/." "$work/"
git -C "$work" init -q -b main
git -C "$work" add -A
git -C "$work" -c user.name=demo -c user.email=demo@localhost commit -q -m start
for card in "$here"/cards/*.toml; do
  before=$(git -C "$work" rev-list --count HEAD)
  start=$(date +%s)
  echo "=== $(basename "$card" .toml)"
  "$bin" --lang en --timeout 900 "$@" --dir "$work" run "$card"
  code=$?
  after=$(git -C "$work" rev-list --count HEAD)
  dirty=$(git -C "$work" status --short | wc -l | tr -d ' ')
  echo "--- exit=$code new_commits=$((after - before)) dirty_files=$dirty seconds=$(( $(date +%s) - start ))"
done
echo "=== history"
git -C "$work" log --oneline
