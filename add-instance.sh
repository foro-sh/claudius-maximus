#!/usr/bin/env bash
# Provision one Claudius instance: a unix user, its clones, its env file and
# its systemd unit. Run as root, from a checkout of this repo, once per
# subscription you want draining a queue.
#
#   REPOS=foro-sh/claudius-maximus \
#   GITHUB_CLIENT_ID=Iv1.xxxxxxxxxxxx \
#   GIT_AUTHOR_NAME="Someone" GIT_AUTHOR_EMAIL=someone@example.com \
#   ./add-instance.sh
#
# The script picks the next free Latin-ordinal name (claudius-maximus,
# claudius-secundus, … claudius-centesimus; cap 100) and uses it as the unix
# user, the LABEL, and — title-cased — the INSTANCE display name. Pass a name
# from that list explicitly to re-run / repair that slot.
#
# Clone paths default to /home/<name>/repos/<owner>/<repo> when REPOS entries
# omit a path. Absolute paths are still accepted and must stay under that home.
#
# The two steps that need a human — `claude login` for that person's
# subscription, and the worker's own GitHub device flow — are printed at the
# end rather than automated: both are interactive, and both must run as the
# person who actually holds the subscription.
#
# Safe to re-run for a given name: an existing user, clone or env file is
# left alone.
set -euo pipefail

die() { echo "add-instance: $*" >&2; exit 1; }

# 1=maximus … 100=centesimus. maximus replaces primus on purpose: that is the
# project's name. Compounds are kebab-case (vicesimus-primus); 18–19 use the
# classical subtraction forms. Every full name fits Linux's 32-char user limit.
INSTANCE_ORDINALS=(
    maximus secundus tertius quartus quintus sextus septimus octavus nonus decimus
    undecimus duodecimus tertius-decimus quartus-decimus quintus-decimus
    sextus-decimus septimus-decimus duodevicesimus undevicesimus vicesimus
    vicesimus-primus vicesimus-secundus vicesimus-tertius vicesimus-quartus
    vicesimus-quintus vicesimus-sextus vicesimus-septimus vicesimus-octavus
    vicesimus-nonus
    tricesimus tricesimus-primus tricesimus-secundus tricesimus-tertius
    tricesimus-quartus tricesimus-quintus tricesimus-sextus tricesimus-septimus
    tricesimus-octavus tricesimus-nonus
    quadragesimus quadragesimus-primus quadragesimus-secundus quadragesimus-tertius
    quadragesimus-quartus quadragesimus-quintus quadragesimus-sextus
    quadragesimus-septimus quadragesimus-octavus quadragesimus-nonus
    quinquagesimus quinquagesimus-primus quinquagesimus-secundus
    quinquagesimus-tertius quinquagesimus-quartus quinquagesimus-quintus
    quinquagesimus-sextus quinquagesimus-septimus quinquagesimus-octavus
    quinquagesimus-nonus
    sexagesimus sexagesimus-primus sexagesimus-secundus sexagesimus-tertius
    sexagesimus-quartus sexagesimus-quintus sexagesimus-sextus
    sexagesimus-septimus sexagesimus-octavus sexagesimus-nonus
    septuagesimus septuagesimus-primus septuagesimus-secundus septuagesimus-tertius
    septuagesimus-quartus septuagesimus-quintus septuagesimus-sextus
    septuagesimus-septimus septuagesimus-octavus septuagesimus-nonus
    octogesimus octogesimus-primus octogesimus-secundus octogesimus-tertius
    octogesimus-quartus octogesimus-quintus octogesimus-sextus
    octogesimus-septimus octogesimus-octavus octogesimus-nonus
    nonagesimus nonagesimus-primus nonagesimus-secundus nonagesimus-tertius
    nonagesimus-quartus nonagesimus-quintus nonagesimus-sextus
    nonagesimus-septimus nonagesimus-octavus nonagesimus-nonus
    centesimus
)

instance_name_from_ordinal() {
    echo "claudius-$1"
}

display_name_from_instance() {
    # claudius-vicesimus-primus → Claudius Vicesimus Primus
    local out= part
    local IFS=-
    # shellcheck disable=SC2086
    for part in $1; do
        out+="${out:+ }${part^}"
    done
    echo "$out"
}

name_is_known() {
    local want=$1 ordinal
    for ordinal in "${INSTANCE_ORDINALS[@]}"; do
        [[ $(instance_name_from_ordinal "$ordinal") == "$want" ]] && return 0
    done
    return 1
}

# Taken if the unix user exists, its env file exists, or any env file already
# claims this LABEL — any one of those means the slot is occupied.
name_is_taken() {
    local name=$1 env_file
    id -u "$name" >/dev/null 2>&1 && return 0
    [[ -e /etc/$name.env ]] && return 0
    for env_file in /etc/claudius-*.env; do
        [[ -e $env_file ]] || continue
        grep -qxF "LABEL=$name" "$env_file" && return 0
    done
    return 1
}

pick_next_name() {
    local ordinal name
    for ordinal in "${INSTANCE_ORDINALS[@]}"; do
        name=$(instance_name_from_ordinal "$ordinal")
        if ! name_is_taken "$name"; then
            echo "$name"
            return 0
        fi
    done
    die "all ${#INSTANCE_ORDINALS[@]} instance names are taken — cap is 100"
}

[[ $# -le 1 ]] || die "usage: REPOS=... GITHUB_CLIENT_ID=... GIT_AUTHOR_NAME=... GIT_AUTHOR_EMAIL=... $0 [claudius-<ordinal>]"

if [[ $# -eq 1 ]]; then
    user=$1
    name_is_known "$user" || die "'$user' is not a known instance name (want claudius-maximus … claudius-centesimus)"
else
    user=$(pick_next_name)
fi
label=$user
instance=$(display_name_from_instance "$user")

[[ -n ${REPOS:-} ]] || die "set REPOS=owner/name[,owner/name2[=author|author]|…] — see README"
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
# about. Path may be omitted: owner/name → /home/<user>/repos/owner/name.
declare -A clones=()
declare -a repos_expanded=()
IFS=',' read -ra entries <<< "$REPOS"
for entry in "${entries[@]}"; do
    [[ -n $entry ]] || continue
    repo=${entry%%=*}
    rest=
    [[ $entry == *=* ]] && rest=${entry#*=}
    [[ $repo == */* ]] || die "invalid REPOS entry '$entry' (want owner/name[=/abs/path][=author|author])"

    path=
    authors=
    if [[ -z $rest ]]; then
        path=$home/repos/$repo
    elif [[ $rest == /* ]]; then
        path=${rest%%=*}
        if [[ $rest == *=* ]]; then
            authors=${rest#*=}
            [[ -n $authors ]] || die "invalid REPOS entry '$entry' (trailing '=' with no author allowlist)"
        fi
    elif [[ $rest == */* ]]; then
        # A relative path (has a slash but no leading one) — refuse rather than
        # treat it as an author allowlist.
        die "invalid REPOS entry '$entry' (clone path must be absolute)"
    else
        # authors only — no path
        path=$home/repos/$repo
        authors=$rest
    fi
    [[ $path == "$home"/* ]] || die "clone path $path is outside $home — instances must never share a working tree"
    if [[ -n ${clones[$path]+x} && ${clones[$path]} != "$repo" ]]; then
        die "clone path $path is claimed by both ${clones[$path]} and $repo"
    fi
    clones[$path]=$repo
    if [[ -n $authors ]]; then
        repos_expanded+=("$repo=$path=$authors")
    else
        repos_expanded+=("$repo=$path")
    fi
done
[[ ${#clones[@]} -gt 0 ]] || die "REPOS is empty"

# Join expanded entries for the env file (worker still wants absolute paths).
IFS=,
repos_for_env="${repos_expanded[*]}"
unset IFS

[[ $EUID -eq 0 ]] || die "must run as root (it creates a unix user and writes /etc)"

# A label two live instances share means two workers planning one issue and
# opening competing PRs. The worker catches that at startup with its own lock,
# but catching it here means never starting the second unit at all. Skip our
# own env file so re-runs stay idempotent.
for env_file in /etc/claudius-*.env; do
    [[ -e $env_file ]] || continue
    [[ $env_file == "/etc/$user.env" ]] && continue
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

env_file=/etc/$user.env
if [[ -e $env_file ]]; then
    echo "$env_file already exists, leaving it alone"
else
    # No GitHub token here on purpose: the device flow puts it in
    # $HOME/.claudius-maximus/github-token, mode 0600.
    umask 077
    cat > "$env_file" <<EOF
REPOS=$repos_for_env
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
