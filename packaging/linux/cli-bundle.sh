#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
binary=$(realpath "${1:?Usage: cli-bundle.sh <Linux binary> <output directory>}")
output=${2:?Usage: cli-bundle.sh <Linux binary> <output directory>}
mkdir -p "$output"
output=$(realpath "$output")
test "$(uname -s)" = Linux || { echo 'Build this package on Linux.' >&2; exit 1; }
machine=$(LC_ALL=C readelf -h "$binary" | awk -F: '/Machine:/ {gsub(/^ +/, "", $2); print $2}')
case "$machine" in
    *X86-64*) arch=x86_64 ;;
    AArch64) arch=aarch64 ;;
    *) echo "Unsupported ELF architecture: $machine" >&2; exit 1 ;;
esac
version=$("$binary" --version | awk '{print $2}')
case "$version" in ''|*[!0-9A-Za-z.+-]*) echo 'Invalid version.' >&2; exit 1 ;; esac
name="shelllf-cli-$version-linux-$arch"
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
trap 'exit 1' HUP INT TERM
mkdir -p "$work/bin" "$work/skills/shelllf-cli"
install -m 755 "$binary" "$work/bin/shelllf-cli"
strip "$work/bin/shelllf-cli"
install -m 644 "$root/crates/shellrs-cli/SKILL.md" "$work/skills/shelllf-cli/SKILL.md"
install -m 644 "$root/crates/shellrs-cli/README.zh.md" "$work/README.zh.md"
install -m 644 "$root/LICENSE" "$work/LICENSE"
printf '%s\n' "$arch" > "$work/ARCH"
printf 'version=%s\narchitecture=%s\nsource_commit=%s\nsource_state=%s\n' \
    "$version" "$arch" "$(git -C "$root" rev-parse HEAD)" \
    "$(if test -z "$(git -C "$root" status --porcelain)"; then echo clean; else echo modified; fi)" > "$work/BUILD-INFO"
(cd "$work" && sha256sum bin/shelllf-cli skills/shelllf-cli/SKILL.md README.zh.md LICENSE ARCH BUILD-INFO > SHA256SUMS)
sed 's/\r$//' "$root/packaging/linux/cli-installer.sh" > "$output/$name.run"
tar -czf "$work/payload.tar.gz" -C "$work" bin skills README.zh.md LICENSE ARCH BUILD-INFO SHA256SUMS
cat "$work/payload.tar.gz" >> "$output/$name.run"
chmod 755 "$output/$name.run"
(cd "$output" && sha256sum "$name.run" > "$name.run.sha256")
printf '%s\n' "$output/$name.run"
