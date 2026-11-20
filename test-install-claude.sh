#!/usr/bin/env bash
# Checks install-claude.sh: the validation that runs before it needs root, and
# the settings.json merge that keeps the CLI from updating itself.
set -uo pipefail
cd "$(dirname "$0")"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fail=0
expect() {
    local want=$1 got=$2
    if [[ $got == *"$want"* ]]; then return; fi
    echo "FAIL: expected ${want@Q} in ${got@Q}"
    fail=1
}

# The script finds its pin file next to itself, so a copy without one stands in
# for a checkout that lost it, and the checkout stays read-only.
cp install-claude.sh "$work/"
expect "usage:" "$("$work/install-claude.sh" 2>&1)"
expect "usage:" "$(./install-claude.sh a b 2>&1)"
expect "no pinned version" "$("$work/install-claude.sh" claudius-tertius 2>&1)"
: > "$work/claude-code.version"
expect "no pinned version" "$("$work/install-claude.sh" claudius-tertius 2>&1)"
echo 'latest' > "$work/claude-code.version"
expect "want a bare version" "$("$work/install-claude.sh" claudius-tertius 2>&1)"
# A valid pin gets as far as the root check (or the tools it needs first).
got=$(./install-claude.sh claudius-tertius 2>&1)
[[ $got == *"must run as root"* || $got == *"is not installed"* ]] || {
    echo "FAIL: a valid pin did not reach the root check: ${got@Q}"
    fail=1
}
[[ $(tr -d '[:space:]' < claude-code.version) =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
    echo "FAIL: claude-code.version is not a bare version"
    fail=1
}

# The merge, pulled out of the script itself rather than copied.
merge() {
    {
        sed -n '/^die()/p' install-claude.sh
        sed -n '/^merge_settings()/,/^}/p' install-claude.sh
        echo "merge_settings $1"
    } | bash 2>&1
}

# Missing file: created, with the keys.
merge "$work/new.json" >/dev/null
[[ $(jq -c . "$work/new.json") == '{"env":{"DISABLE_AUTOUPDATER":"1"},"autoUpdates":false}' ]] || {
    echo "FAIL: created settings got $(cat "$work/new.json")"
    fail=1
}

# Existing keys, including other env entries, survive.
echo '{"theme":"dark","env":{"FOO":"bar"},"permissions":{"allow":["Bash(ls)"]}}' > "$work/old.json"
merge "$work/old.json" >/dev/null
[[ $(jq -c . "$work/old.json") == '{"theme":"dark","env":{"FOO":"bar","DISABLE_AUTOUPDATER":"1"},"permissions":{"allow":["Bash(ls)"]},"autoUpdates":false}' ]] || {
    echo "FAIL: merged settings got $(cat "$work/old.json")"
    fail=1
}

# Idempotent, and it overrides a setting that turns updates back on.
echo '{"autoUpdates":true,"env":{"DISABLE_AUTOUPDATER":"0"}}' > "$work/on.json"
merge "$work/on.json" >/dev/null
first=$(cat "$work/on.json")
merge "$work/on.json" >/dev/null
[[ $(jq -c . "$work/on.json") == '{"autoUpdates":false,"env":{"DISABLE_AUTOUPDATER":"1"}}' && $first == "$(cat "$work/on.json")" ]] || {
    echo "FAIL: second merge changed things: $(cat "$work/on.json")"
    fail=1
}

# Garbage is an error and is left as it was; no temp file is left behind.
echo 'not json' > "$work/bad.json"
expect "not valid JSON" "$(merge "$work/bad.json")"
[[ $(cat "$work/bad.json") == "not json" && $(ls "$work"/bad.json* | wc -l) -eq 1 ]] || {
    echo "FAIL: an invalid settings file was touched or left a temp file"
    fail=1
}

[[ $fail -eq 0 ]] && echo "all install-claude.sh checks passed"
exit $fail
