#!/bin/sh
set -eu

case "${1:-}" in
    --help|-h)
        printf '%s\n' 'Usage: bash shelllf-cli-*.run' \
            'Installs the headless CLI and Codex skill for the current user.' \
            'Run as the user who runs Codex; sudo is not needed.'
        exit 0
        ;;
    '') ;;
    *) printf '%s\n' "Unknown argument: $1" >&2; exit 1 ;;
esac

test "$(uname -s)" = Linux || { echo 'This package requires Linux.' >&2; exit 1; }
test -n "${HOME:-}" && test -d "$HOME" || { echo 'HOME must be an existing directory.' >&2; exit 1; }
for tool in awk tail tar sha256sum install mktemp grep; do
    command -v "$tool" >/dev/null || { echo "Missing command: $tool" >&2; exit 1; }
done

work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
trap 'exit 1' HUP INT TERM
payload=$(awk '/^__SHELLLF_PAYLOAD__$/ { print NR + 1; exit }' "$0")
test -n "$payload" || { echo 'Installer payload is missing.' >&2; exit 1; }
tail -n +"$payload" "$0" | tar -xz -C "$work"
cd "$work"
sha256sum --check --status SHA256SUMS || { echo 'Package checksum failed.' >&2; exit 1; }
test "$(uname -m)" = "$(cat ARCH)" || { echo "This package is for $(cat ARCH); this computer is $(uname -m)." >&2; exit 1; }
chmod +x bin/shelllf-cli
bin/shelllf-cli lan debug --help >/dev/null || { echo 'The CLI cannot run on this system. No files were installed.' >&2; exit 1; }

bin_dir="$HOME/.local/bin"
skill_dir="${CODEX_HOME:-$HOME/.codex}/skills/shelllf-cli"
doc_dir="$HOME/.local/share/shelllf-cli"
mkdir -p "$bin_dir" "$skill_dir" "$doc_dir"
install -m 755 bin/shelllf-cli "$bin_dir/.shelllf-cli-new-$$"
mv -f "$bin_dir/.shelllf-cli-new-$$" "$bin_dir/shelllf-cli"
install -m 644 skills/shelllf-cli/SKILL.md "$skill_dir/SKILL.md"
install -m 644 README.zh.md LICENSE BUILD-INFO "$doc_dir/"

path_line='export PATH="$HOME/.local/bin:$PATH"'
for profile in "$HOME/.profile" "$HOME/.bashrc"; do
    if ! grep -Fqx "$path_line" "$profile" 2>/dev/null; then
        printf '\n%s\n' "$path_line" >> "$profile"
    fi
done
printf '\n%s\n' 'Installed ShellLF CLI and Codex skill.' \
    "CLI:   $bin_dir/shelllf-cli" "Skill: $skill_dir/SKILL.md" \
    'Restart Codex to load the skill. In the current shell run:' \
    '  export PATH="$HOME/.local/bin:$PATH"' \
    'First connection (replace the address with your ShellLF computer):' \
    "  $bin_dir/shelllf-cli lan debug inspect 192.168.1.20"
exit 0
__SHELLLF_PAYLOAD__
