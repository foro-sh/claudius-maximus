#!/usr/bin/env bash
# Checks add-instance.sh's config validation, the half that runs before it
# needs root. A valid config gets as far as the root check; a bad one must not.
set -uo pipefail
cd "$(dirname "$0")"

fake_binary=$(mktemp) && chmod +x "$fake_binary"
trap 'rm -f "$fake_binary"' EXIT

# stdout+stderr of one run, with a valid baseline env the case can override.
# Default REPOS omits the clone path so the script expands it under the
# auto-picked (or explicit) instance home.
run() {
    env CLAUDIUS_BINARY="${CLAUDIUS_BINARY-$fake_binary}" \
        REPOS="${REPOS-foro-sh/claudius-maximus}" \
        GITHUB_CLIENT_ID="${GITHUB_CLIENT_ID-Iv1.testclientid}" \
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

# Auto-pick (no args) and an explicit known name both reach the root check.
expect "must run as root" "$(run)"
expect "must run as root" "$(run claudius-tertius)"

expect "usage:" "$(run claudius-tertius extra)"
expect "not a known instance name" "$(run claudius-primus)"
expect "set REPOS" "$(REPOS= run claudius-tertius)"
expect "set GIT_AUTHOR_EMAIL" "$(GIT_AUTHOR_EMAIL= run claudius-tertius)"
expect "set GITHUB_CLIENT_ID" "$(GITHUB_CLIENT_ID= run claudius-tertius)"
expect "clone path must be absolute" \
    "$(REPOS=foro-sh/claudius-maximus=repos/claudius-maximus run claudius-tertius)"
expect "want owner/name" "$(REPOS=platform run claudius-tertius)"
# The one that matters: instance 3 pointed at instance 2's tree.
expect "instances must never share a working tree" \
    "$(REPOS=foro-sh/claudius-maximus=/home/claudius-secundus/repos/claudius-maximus run claudius-tertius)"
# Authors-only form still defaults the clone under this instance's home.
expect "must run as root" \
    "$(REPOS='foro-sh/claudius-maximus=alice|bob' run claudius-tertius)"
# Two repos with the same basename must not share one default tree.
expect "claimed by both" \
    "$(REPOS='foro-sh/foro=/home/claudius-tertius/repos/x,acme/foro=/home/claudius-tertius/repos/x' run claudius-tertius)"
# Absolute path + authors still fine.
expect "must run as root" \
    "$(REPOS='foro-sh/claudius-maximus=/home/claudius-tertius/repos/claudius-maximus=alice|bob' run claudius-tertius)"
expect "no binary at" "$(CLAUDIUS_BINARY=/nonexistent run claudius-tertius)"

# 100 Latin ordinals in the table (tokens inside the INSTANCE_ORDINALS array).
ordinals_count=$(sed -n '/^INSTANCE_ORDINALS=(/,/^)/p' add-instance.sh \
    | tr -s '[:space:]' '\n' \
    | grep -cE '^[a-z]+(-[a-z]+)*$')
[[ $ordinals_count -eq 100 ]] || {
    echo "FAIL: expected 100 ordinals, got $ordinals_count"
    fail=1
}

# Status ports are derived from the ordinal, so the same slot always gets the
# same port and two instances can never be handed one. Pulled out of the script
# itself rather than copied, so a change to the table is caught here.
ports=$(
    {
        sed -n '/^INSTANCE_ORDINALS=(/,/^)/p' add-instance.sh
        sed -n '/^STATUS_PORT_BASE=/p' add-instance.sh
        sed -n '/^instance_name_from_ordinal()/,/^}/p' add-instance.sh
        sed -n '/^status_port_of()/,/^}/p' add-instance.sh
        sed -n '/^status_port_for_name()/,/^}/p' add-instance.sh
        echo 'status_port_for_name claudius-maximus'
        echo 'status_port_for_name claudius-tertius'
        echo 'status_port_for_name claudius-centesimus'
        echo 'status_port_for_name claudius-nobody || echo unknown'
    } | bash
)
[[ $ports == $'9781\n9783\n9880\nunknown' ]] || {
    echo "FAIL: status ports got ${ports@Q}"
    fail=1
}

# Two instances collide on the port, however each of them spells its address,
# and two that serve no page at all never collide.
claims=$(
    {
        sed -n '/^status_port_of()/,/^}/p' add-instance.sh
        echo 'echo "bare=$(status_port_of 9781)"'
        echo 'echo "full=$(status_port_of 127.0.0.1:9781)"'
        echo 'echo "v6=$(status_port_of [::1]:9781)"'
        echo 'echo "off=$(status_port_of off)/$(status_port_of OFF)/$(status_port_of "")"'
    } | bash
)
[[ $claims == $'bare=9781\nfull=9781\nv6=9781\noff=//' ]] || {
    echo "FAIL: status address normalisation got ${claims@Q}"
    fail=1
}

# The closing instructions must not offer a page that is turned off: with
# STATUS_ADDR=off the address is the word 'off', and `curl http://off/` sends
# an operator somewhere that does not exist.
offered() {
    {
        echo "user=claudius-maximus"
        echo "status_addr=$1"
        sed -n '/^status_port_of()/,/^}/p' add-instance.sh
        sed -n '/^# The closing instructions offer the page only/,/^fi$/p' add-instance.sh
        echo 'printf "%s\n" "$page_lines"'
    } | bash
}
[[ $(offered 127.0.0.1:9781) == *"curl http://127.0.0.1:9781/healthz"* ]] || {
    echo "FAIL: a served instance is not told where its page is"
    fail=1
}
[[ $(offered off) != *curl* ]] || {
    echo "FAIL: an instance with STATUS_ADDR=off is offered a page it does not serve"
    fail=1
}

display=$(bash -c '
display_name_from_instance() {
    local out= part
    local IFS=-
    for part in $1; do
        out+="${out:+ }${part^}"
    done
    echo "$out"
}
display_name_from_instance claudius-vicesimus-primus
')
[[ $display == "Claudius Vicesimus Primus" ]] || {
    echo "FAIL: display name got ${display@Q}"
    fail=1
}

[[ $fail -eq 0 ]] && echo "all add-instance.sh config checks passed"
exit $fail
