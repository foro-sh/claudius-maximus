#!/usr/bin/env bash
# Install the pinned Claude Code CLI for one instance's unix user, and turn
# its auto-updater off. Run as root, once per instance: add-instance.sh calls
# it for a new one, and bumping claude-code.version means running it again for
# every existing one (see "Updating Claude Code" in the README).
#
#   ./install-claude.sh claudius-secundus
#
# The version lives in claude-code.version, next to this script, and nowhere
# else. The CLI never updates itself: an update that fails halfway leaves the
# instance without a `claude` until somebody notices (#25), and it happens at
# a moment nobody picked and differently on each box.
#
# Safe to re-run, including on a box an auto-update already broke: the
# leftovers of a failed update are what it clears first.
set -euo pipefail

die() { echo "install-claude: $*" >&2; exit 1; }

# Merge the keys that switch the auto-updater off into a Claude Code
# settings.json, keeping every other key in it. The file is created when it is
# missing. An existing file that is not valid JSON is an error and is left as
# it was: guessing at someone's settings is worse than stopping.
#
# This lives in settings.json and not only in the unit's Environment= because
# the incident began in an interactive `claude /login` under
# `sudo -u <user> -H bash -l`, which never sees the unit's environment.
merge_settings() {
    local file=$1 tmp
    [[ -s $file ]] || echo '{}' > "$file"
    tmp=$(mktemp "$file.XXXXXX")
    if ! jq '(.env //= {}) | .env.DISABLE_AUTOUPDATER = "1" | .autoUpdates = false' "$file" > "$tmp"; then
        rm -f "$tmp"
        die "$file is not valid JSON, fix or remove it, then re-run"
    fi
    mv "$tmp" "$file"
}

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
pin_file=$here/claude-code.version

[[ $# -eq 1 ]] || die "usage: $0 claudius-<ordinal>"
user=$1

[[ -s $pin_file ]] || die "no pinned version in $pin_file, it holds the one version every instance runs"
version=$(tr -d '[:space:]' < "$pin_file")
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "$pin_file holds '$version', want a bare version like 2.1.329"

command -v npm >/dev/null || die "npm is not installed, the CLI is installed from the npm package"
command -v jq >/dev/null || die "jq is not installed, it edits the user's Claude Code settings"

[[ $EUID -eq 0 ]] || die "must run as root (it installs into another user's home)"
home=$(getent passwd "$user" | cut -d: -f6)
[[ -n $home && -d $home ]] || die "no user '$user' with a home directory, run add-instance.sh first"

# What a failed auto-update leaves behind: npm renames the package aside to a
# .claude-code-<random> directory and the bin symlink to .claude-<random>, and
# the next update (or this install) trips over the ENOTEMPTY. Only that
# pattern, and only under this user's own prefix. Nothing may be mid-run: stop
# the instance first, or restart it right after.
scope=$home/.local/lib/node_modules/@anthropic-ai
for stale in "$scope"/.claude-code-* "$home"/.local/bin/.claude-*; do
    [[ -e $stale || -L $stale ]] || continue
    echo "removing $stale, left behind by a failed update"
    rm -rf -- "$stale"
done

# Never --ignore-scripts: the package fetches its native binary in a
# postinstall step, and without it all that is left is a stub.
(cd "$home" && sudo -u "$user" -H npm install -g --prefix "$home/.local" "@anthropic-ai/claude-code@$version")

# Asked of the installed binary rather than trusted from npm's exit status,
# which is also what catches the stub that only prints "claude native binary
# not installed".
claude=$home/.local/bin/claude
installed=$(cd "$home" && sudo -u "$user" -H "$claude" --version) \
    || die "$claude does not run, the install is broken"
[[ $installed == "$version"* ]] || die "$claude reports '$installed', wanted $version"
echo "$user: claude $version"

install -d -o "$user" -g "$user" -m 700 "$home/.claude"
settings=$home/.claude/settings.json
merge_settings "$settings"
chown "$user:$user" "$settings"
chmod 600 "$settings"
echo "$user: auto-updater off in $settings"
