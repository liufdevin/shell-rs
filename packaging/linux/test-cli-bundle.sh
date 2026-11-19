#!/bin/sh
set -eu

package=$(realpath "${1:?Usage: test-cli-bundle.sh <installer.run>}")
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
trap 'exit 1' HUP INT TERM
mkdir -p "$work/home" "$work/codex"

run_installer() {
    env -u DISPLAY -u WAYLAND_DISPLAY HOME="$work/home" CODEX_HOME="$work/codex" sh "$package"
}

run_installer
test -x "$work/home/.local/bin/shelllf-cli"
test -f "$work/codex/skills/shelllf-cli/SKILL.md"
grep -q 'lan debug' "$work/codex/skills/shelllf-cli/SKILL.md"
test -f "$work/home/.local/share/shelllf-cli/LICENSE"
test "$(grep -c '^export PATH=' "$work/home/.bashrc")" = 1
env -u DISPLAY -u WAYLAND_DISPLAY SHELLRS_DATA_DIR="$work/data" "$work/home/.local/bin/shelllf-cli" lan debug peers > "$work/peers.json"
test "$(cat "$work/peers.json")" = '[]'
identity_before=$(sha256sum "$work/data/lan-cli/identity")
run_installer
test "$(grep -c '^export PATH=' "$work/home/.bashrc")" = 1
test "$identity_before" = "$(sha256sum "$work/data/lan-cli/identity")"
env SHELLRS_DATA_DIR="$work/data" "$work/home/.local/bin/shelllf-cli" lan debug logs --help >/dev/null
env SHELLRS_DATA_DIR="$work/data" "$work/home/.local/bin/shelllf-cli" lan debug exec --help >/dev/null

size=$(wc -c < "$package")
head -c "$((size - 100))" "$package" > "$work/truncated.run"
mkdir -p "$work/broken-home"
if env HOME="$work/broken-home" CODEX_HOME="$work/broken-codex" sh "$work/truncated.run" > "$work/failure.log" 2>&1; then
    echo 'Truncated installer unexpectedly succeeded.' >&2
    exit 1
fi
test ! -e "$work/broken-home/.local/bin/shelllf-cli"
printf '%s\n' 'Passed: install, headless startup, skill, repeat install, identity preservation, damaged package rejection.'
