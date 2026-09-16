#!/usr/bin/env bash
# Provision one Claudius instance: a unix user, its clones, its env file and
# its systemd unit. Run as root, from a checkout of this repo, once per
# subscription you want draining a queue.
#
#   REPOS=foro-sh/platform=/home/claudebot3/repos/platform \
#   GITHUB_CLIENT_ID=Iv1.xxxxxxxxxxxx \
#   GIT_AUTHOR_NAME="Someone" GIT_AUTHOR_EMAIL=someone@example.com \
#   ./add-instance.sh claudebot3 claudius-tertius "Claudius Tertius"
#
# The two steps that need a human — `claude login` for that person's
# subscription, and the worker's own GitHub device flow — are printed at the
# end rather than automated: both are interactive, and both must run as the
# person who actually holds the subscription.
#
# Safe to re-run: an existing user, clone or env file is left alone.
set -euo pipefail

die() { echo "add-instance: $*" >&2; exit 1; }

[[ $# -eq 3 ]] || die "usage: REPOS=... GITHUB_CLIENT_ID=... GIT_AUTHOR_NAME=... GIT_AUTHOR_EMAIL=... $0 <unix-user> <label> <display name>"
user=$1
label=$2
instance=$3

[[ -n ${REPOS:-} ]] || die "set REPOS=owner/name=/abs/path/to/clone[,...] — see README"
# One OAuth App is shared by every instance — the device flow is what makes
# each one a different GitHub account, not a different app.
[[ -n ${GITHUB_CLIENT_ID:-} ]] || die "set GITHUB_CLIENT_ID — the GitHub OAuth App the device flow authorizes against"
[[ -n ${GIT_AUTHOR_NAME:-} ]] || die "set GIT_AUTHOR_NAME — commits are attributed to it"
[[ -n ${GIT_AUTHOR_EMAIL:-} ]] || die "set GIT_AUTHOR_EMAIL — must be verified on this instance's GitHub account"

binary=${CLAUDIUS_BINARY:-target/release/claudius-maximus}
[[ -x $binary ]] || die "no binary at $binary — 'cargo build --release' first, or set CLAUDIUS_BINARY"
[[ -f claudius@.service ]] || die "run me from the repo checkout (claudius@.service not found here)"

home=/home/$user

# Parsed up front so a typo in REPOS fails before anything is created — and
# before the script needs root at all. Two instances sharing a working tree
# corrupt each other (both check out branches and hard-reset onto origin's
# default), so a clone outside this instance's own home is refused, not warned
# about.
declare -A clones=()
IFS=',' read -ra entries <<< "$REPOS"
for entry in "${entries[@]}"; do
    [[ -n $entry ]] || continue
    repo=${entry%%=*}
    rest=${entry#*=}
    path=${rest%%=*}
    [[ $repo == */* ]] || die "invalid REPOS entry '$entry' (want owner/name=/abs/path/to/clone)"
    [[ $path == /* ]] || die "invalid REPOS entry '$entry' (clone path must be absolute)"
    [[ $path == "$home"/* ]] || die "clone path $path is outside $home — instances must never share a working tree"
    clones[$path]=$repo
done
[[ ${#clones[@]} -gt 0 ]] || die "REPOS is empty"

[[ $EUID -eq 0 ]] || die "must run as root (it creates a unix user and writes /etc)"

# A label two live instances share means two workers planning one issue and
# opening competing PRs. The worker catches that at startup with its own lock,
# but catching it here means never starting the second unit at all.
for env_file in /etc/claudius-*.env; do
    [[ -e $env_file ]] || continue
    [[ $env_file == "/etc/claudius-$user.env" ]] && continue
    if grep -qxF "LABEL=$label" "$env_file"; then
        die "label '$label' is already owned by $env_file — every instance needs its own queue"
    fi
done

if id -u "$user" >/dev/null 2>&1; then
    echo "user $user already exists, leaving it alone"
else
    useradd -m -s /usr/sbin/nologin "$user"
    echo "created user $user"
fi

for entry in "${!clones[@]}"; do
    if [[ -d $entry ]]; then
        echo "clone $entry already exists, leaving it alone"
    else
        sudo -u "$user" -H git clone "https://github.com/${clones[$entry]}.git" "$entry"
    fi
done

install -d -o "$user" -g "$user" -m 755 "$home/claudius-maximus"
install -o "$user" -g "$user" -m 755 "$binary" "$home/claudius-maximus/claudius-maximus"

env_file=/etc/claudius-$user.env
if [[ -e $env_file ]]; then
    echo "$env_file already exists, leaving it alone"
else
    # No GitHub token here on purpose: the device flow puts it in
    # $HOME/.claudius-maximus/github-token-<instance>, mode 0600.
    umask 077
    cat > "$env_file" <<EOF
REPOS=$REPOS
GITHUB_CLIENT_ID=$GITHUB_CLIENT_ID
LABEL=$label
INSTANCE=$instance
PLAN_MODEL=${PLAN_MODEL:-claude-opus-5}
PLAN_EFFORT=${PLAN_EFFORT:-high}
IMPLEMENT_MODEL=${IMPLEMENT_MODEL:-claude-sonnet-5}
IMPLEMENT_EFFORT=${IMPLEMENT_EFFORT:-high}
POLL_INTERVAL=${POLL_INTERVAL:-60}
${CLAUDIUS_MAXIMUS_MATTERMOST_WEBHOOK_URL:+CLAUDIUS_MAXIMUS_MATTERMOST_WEBHOOK_URL=$CLAUDIUS_MAXIMUS_MATTERMOST_WEBHOOK_URL}
GIT_AUTHOR_NAME="$GIT_AUTHOR_NAME"
GIT_AUTHOR_EMAIL=$GIT_AUTHOR_EMAIL
GIT_COMMITTER_NAME="${GIT_COMMITTER_NAME:-$GIT_AUTHOR_NAME}"
GIT_COMMITTER_EMAIL=${GIT_COMMITTER_EMAIL:-$GIT_AUTHOR_EMAIL}
EOF
    chown "root:$user" "$env_file"
    chmod 640 "$env_file"
    echo "wrote $env_file"
fi

unit=/etc/systemd/system/claudius@.service
if [[ -e $unit ]] && ! cmp -s claudius@.service "$unit"; then
    die "$unit differs from this checkout's — reconcile them, then re-run (the template is shared by every instance)"
fi
install -m 644 claudius@.service "$unit"
systemctl daemon-reload

cat <<EOF

Provisioned $instance ($user, label '$label').

Three things left, none of them automatable:

  1. The labels '$label', '$label:planned' and '$label:done' must exist in
     every repo this instance serves. The worker moves them; it never
     creates them.

  2. This instance's own subscription login, as the person who holds it:
       sudo -u $user -H bash -l
         npm install -g @anthropic-ai/claude-code    # or a per-user install
         claude login && claude doctor
         unset ANTHROPIC_API_KEY

  3. Its GitHub device-flow login — run the worker once in the foreground as
     that same user, open the code + URL it prints as this instance's GitHub
     account, then Ctrl-C:
       sudo -u $user -H bash -lc 'set -a; . $env_file; set +a; $home/claudius-maximus/claudius-maximus'

Then:
    systemctl enable --now claudius@$user
    journalctl -u claudius@$user -f
EOF
