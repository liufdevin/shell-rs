#!/bin/sh
# What `cargo run` and `cargo test` run programs through on macOS (see
# .cargo/config.toml).
#
# The keychain knows a program by its signature, and an unsigned build is a
# new program each time it is built: it asks again for every password it
# reads. With SHELLRS_SIGN_IDENTITY set to a code signing identity of this
# computer (`security find-identity -v -p codesigning` lists them, such as
# "Apple Development: …"), ShellRS is signed with it before it runs, so
# "Always Allow" holds from one build to the next. Without it, programs only
# run.
set -e
program="$1"
if [ -n "$SHELLRS_SIGN_IDENTITY" ] && [ "$(basename "$program")" = ShellLF ]; then
    # Signed already, unless the build since replaced it.
    if ! codesign -dv "$program" 2>&1 | grep -qxF "Authority=$SHELLRS_SIGN_IDENTITY" \
        || ! codesign --verify "$program" 2>/dev/null; then
        # The linker names it after a hash that changes with each build,
        # and the keychain knows a program by its name too.
        if ! output=$(codesign --force --sign "$SHELLRS_SIGN_IDENTITY" \
            --identifier com.shellrs.ShellRS.dev "$program" 2>&1); then
            printf '%s\n' "$output" >&2
            exit 1
        fi
    fi
fi
exec "$@"
