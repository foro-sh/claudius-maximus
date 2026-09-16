#!/usr/bin/env bash
# Checks add-instance.sh's config validation — the half that runs before it
# needs root. A valid config gets as far as the root check; a bad one must not.
set -uo pipefail
cd "$(dirname "$0")"

fake_binary=$(mktemp) && chmod +x "$fake_binary"
trap 'rm -f "$fake_binary"' EXIT

# stdout+stderr of one run, with a valid baseline env the case can override.
run() {
    env CLAUDIUS_BINARY="${CLAUDIUS_BINARY-$fake_binary}" \
        REPOS="${REPOS-foro-sh/platform=/home/bot3/repos/platform}" \
        GIT_AUTHOR_NAME="${GIT_AUTHOR_NAME-Someone}" \
        GIT_AUTHOR_EMAIL="${GIT_AUTHOR_EMAIL-someone@example.com}" \
        ./add-instance.sh "$@" 2>&1
}

fail=0
expect() {
    local want=$1 got=$2
    if [[ $got == *"$want"* ]]; then return; fi
    echo "FAIL: expected ${want@Q} in ${got@Q}"
    fail=1
}

# A config with nothing wrong with it reaches the root check and stops there,
# which is as far as this test can go unprivileged.
expect "must run as root" "$(run bot3 claudius-tertius 'Claudius Tertius')"

expect "usage:" "$(run bot3 claudius-tertius)"
expect "set REPOS" "$(REPOS= run bot3 l 'I')"
expect "set GIT_AUTHOR_EMAIL" "$(GIT_AUTHOR_EMAIL= run bot3 l 'I')"
expect "clone path must be absolute" "$(REPOS=foro-sh/platform=repos/platform run bot3 l 'I')"
expect "want owner/name=" "$(REPOS=platform=/home/bot3/repos/platform run bot3 l 'I')"
# The one that matters: instance 3 pointed at instance 2's tree.
expect "instances must never share a working tree" \
    "$(REPOS=foro-sh/platform=/home/bot2/repos/platform run bot3 l 'I')"
# A per-repo author allowlist is a third field, not a second clone path.
expect "must run as root" \
    "$(REPOS='foro-sh/platform=/home/bot3/repos/platform=alice|bob' run bot3 l 'I')"
expect "no binary at" "$(CLAUDIUS_BINARY=/nonexistent run bot3 l 'I')"

[[ $fail -eq 0 ]] && echo "all add-instance.sh config checks passed"
exit $fail
