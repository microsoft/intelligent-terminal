#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

exec 1>/dev/null
LC_ALL=C
export LC_ALL
umask 077

note() { printf '%s\n' "it-agent-hook: $1" >&2; }
if [ "${IT_SSH_HOOK_ROUTE+x}" != x ]; then
    [ -n "${TMUX:-}" ] && [ -n "${TMUX_PANE:-}" ] || exit 0
fi
[ -z "${WTA_TMUX_HOOKS_DISABLED:-}" ] || exit 0
command -v tmux >/dev/null 2>&1 || exit 0
for utility in sh timeout mktemp rm rmdir head wc base64; do
    command -v "$utility" >/dev/null 2>&1 || {
        note 'required utility unavailable'
        exit 0
    }
done
if [ "${IT_SSH_HOOK_ROUTE+x}" = x ]; then
    for utility in stat id; do
        command -v "$utility" >/dev/null 2>&1 || {
            note 'required utility unavailable'
            exit 0
        }
    done
fi

scratch=$(timeout --kill-after=0.05 0.1 mktemp -d "${TMPDIR:-/tmp}/it-agent-hook.XXXXXXXXXXXX" 2>/dev/null) || {
    note 'private scratch directory unavailable'
    exit 0
}
cleanup() {
    timeout --kill-after=0.05 0.1 rm -f -- "$scratch/raw" "$scratch/encoded" "$scratch/version" \
        "$scratch/panes" "$scratch/clients" "$scratch/targets" "$scratch/error" "$scratch/commands" 2>/dev/null ||
        note 'scratch cleanup failed'
    timeout --kill-after=0.05 0.1 rmdir -- "$scratch" 2>/dev/null || note 'scratch cleanup failed'
}
trap cleanup 0
trap 'exit 0' HUP INT TERM

# fd 3 carries untouched hook stdin; stdin supplies only this fixed script.
# timeout owns the whole worker process group, including blocked utilities.
timeout --kill-after=0.1 3.2 sh -s -- "$scratch" "$@" 3<&0 <<'WORKER'
dir=$1
shift
fail() { printf '%s\n' "it-agent-hook: $1" >&2; exit 0; }
source=
event=
while [ "$#" -gt 0 ]; do
    [ "$#" -ge 2 ] || fail 'invalid arguments'
    case $1 in
        --cli-source) [ -z "$source" ] || fail 'invalid arguments'; source=$2 ;;
        --event) [ -z "$event" ] || fail 'invalid arguments'; event=$2 ;;
        *) fail 'invalid arguments' ;;
    esac
    shift 2
done
case $source in claude|copilot|codex|gemini|opencode) ;; *) fail 'invalid source' ;; esac
case $event in
    agent.session.start|agent.session.end|agent.prompt.submit|agent.notification|\
    agent.tool.starting|agent.stop|agent.error|agent.subagent.stop) ;;
    *) fail 'invalid event' ;;
esac
[ "$source" != opencode ] || [ "${OPENCODE_CLIENT:-}" != acp ] || exit 0

managed=
if [ "${IT_SSH_HOOK_ROUTE+x}" = x ]; then
    managed=1
    route=$IT_SSH_HOOK_ROUTE
    [ "${#route}" -eq 36 ] || fail 'invalid managed SSH route'
    case $route in ''|*[!0-9A-Fa-f-]*) fail 'invalid managed SSH route' ;; esac
    previous_ifs=$IFS
    IFS=-
    set -- $route
    IFS=$previous_ifs
    [ "$#" -eq 5 ] && [ "${#1}" -eq 8 ] && [ "${#2}" -eq 4 ] &&
        [ "${#3}" -eq 4 ] && [ "${#4}" -eq 4 ] && [ "${#5}" -eq 12 ] ||
        fail 'invalid managed SSH route'
    socket=${IT_SSH_HOOK_SOCKET:-}
    [ -n "$socket" ] && [ -n "${IT_SSH_HOOK_SESSION:-}" ] || exit 0
    [ "$IT_SSH_HOOK_SESSION" = it-hooks ] || fail 'invalid managed SSH session'
    case $socket in /*) ;; *) fail 'invalid managed SSH socket' ;; esac
    [ "${#socket}" -le 107 ] && [ ! -L "$socket" ] || fail 'invalid managed SSH socket'
    [ -S "$socket" ] || exit 0
    socket_owner=$(stat -c %u -- "$socket" 2>/dev/null) || fail 'managed socket lookup failed'
    current_user=$(id -u 2>/dev/null) || fail 'user lookup failed'
    [ "$socket_owner" = "$current_user" ] || fail 'managed socket ownership mismatch'
else
    number=${TMUX##*,}
    rest=${TMUX%,*}
    [ "$rest" != "$TMUX" ] || fail 'invalid tmux environment'
    pid=${rest##*,}
    socket=${rest%,*}
    [ "$socket" != "$rest" ] || fail 'invalid tmux environment'
    case $socket in /*) ;; *) fail 'invalid tmux environment' ;; esac
    case $number in ''|*[!0-9]*) fail 'invalid tmux environment' ;; esac
    case $pid in ''|*[!0-9]*) fail 'invalid tmux environment' ;; esac
    case $pid in *[1-9]*) ;; *) fail 'invalid tmux environment' ;; esac
    case $TMUX_PANE in %*) pane_number=${TMUX_PANE#%} ;; *) fail 'invalid tmux environment' ;; esac
    case $pane_number in ''|*[!0-9]*) fail 'invalid tmux environment' ;; esac
    [ "${#socket}" -le 1024 ] && [ "${#pid}" -le 1024 ] &&
        [ "${#number}" -lt 1024 ] && [ "${#TMUX_PANE}" -le 1024 ] ||
        fail 'invalid tmux environment'
    [ -S "$socket" ] || exit 0
    session=\$$number
    pane=$TMUX_PANE
fi
transfer=${dir##*/}

tmux_run() {
    timeout --kill-after=0.1 1 tmux -N -S "$socket" "$@" 2>"$dir/error"
}
lookup_failed() {
    error=
    IFS= read -r error <"$dir/error"
    case $error in
        *"can't find session:"*|*"no server running"*|\
        *"error connecting to "*" (No such file or directory)"*|\
        *"error connecting to "*" (Connection refused)"*) exit 0 ;;
    esac
    fail 'tmux lookup failed'
}
tmux_run -V >"$dir/version" || fail 'tmux version lookup failed'
IFS=' ' read -r product version ignored <"$dir/version"
major=${version%%.*}
minor=${version#*.}
minor=${minor%%[!0-9]*}
[ "$product" = tmux ] && [ "$minor" != "$version" ] || exit 0
case $major:$minor in *[!0-9:]*|:*|*:) exit 0 ;; esac
[ "${#major}" -le 6 ] && [ "${#minor}" -le 6 ] || exit 0
[ "$major" -gt 3 ] || { [ "$major" -eq 3 ] && [ "$minor" -ge 4 ]; } || exit 0
if [ -n "$managed" ]; then
    marker=$(tmux_run show-options -gqv @it-ssh-hooks-protocol) || lookup_failed
    [ "$marker" = 3 ] || exit 0
    tmux_run list-sessions -F '#{session_id} #{==:#{session_name},it-hooks}' >"$dir/panes" || lookup_failed
    session=
    while IFS=' ' read -r candidate matching; do
        [ "$matching" != 1 ] || session=$candidate
    done <"$dir/panes"
    [ -n "$session" ] || exit 0
    frame="IT_AGENT_HOOK/3 $route $source $event $transfer"
else
    tmux_run list-panes -s -t "$session" -F '#{pane_id}' >"$dir/panes" || lookup_failed
    found=
    while IFS= read -r candidate; do
        [ "$candidate" != "$pane" ] || found=1
    done <"$dir/panes"
    [ -n "$found" ] || exit 0
    frame="IT_AGENT_HOOK/2 $session $pane $source $event $transfer"
fi
# Printable separators also work for non-UTF-8 tmux clients outside a pane.
tmux_run list-clients -t "$session" \
    -F '#{client_control_mode} #{client_name} #{session_id}' >"$dir/clients" || lookup_failed
: >"$dir/targets"
while IFS=' ' read -r control client client_session extra; do
    [ "$control" = 1 ] && [ "$client_session" = "$session" ] || continue
    [ -z "$extra" ] && [ "${#client}" -le 1024 ] || fail 'invalid tmux client'
    case $client in ''|*[!A-Za-z0-9_./:-]*) fail 'invalid tmux client' ;; esac
    printf '%s\n' "$client" >>"$dir/targets" || fail 'client lookup write failed'
done <"$dir/clients"
[ -s "$dir/targets" ] || exit 0

if [ -t 3 ]; then
    : >"$dir/raw"
else
    timeout --kill-after=0.1 1 head -c 1048577 <&3 >"$dir/raw" 2>/dev/null ||
        fail 'stdin read failed or timed out'
fi
size=$(wc -c <"$dir/raw" 2>/dev/null) || fail 'stdin size lookup failed'
case $size in ''|*[!0-9]*) fail 'stdin size lookup failed' ;; esac
[ "$size" -le 1048576 ] || fail 'stdin size limit exceeded'
base64 --wrap=6000 "$dir/raw" >"$dir/encoded" 2>/dev/null || fail 'base64 encoding failed'
count=$(( (size + 4499) / 4500 ))
if [ "$count" -eq 0 ]; then
    count=1
    printf '\n' >"$dir/encoded"
fi

: >"$dir/commands"
index=0
while IFS= read -r data; do
    case $data in *[!A-Za-z0-9+/=]*) fail 'invalid base64 output' ;; esac
    while IFS= read -r client; do
        # All fields are restricted tokens or Base64, so single-quoted tmux
        # literals cannot contain quoting, expansion or command separators.
        printf "display-message -l -c '%s' '%s'\n" "$client" \
            "$frame $index $count $data" \
            >>"$dir/commands" || fail 'command framing failed'
    done <"$dir/targets"
    index=$((index + 1))
done <"$dir/encoded"
[ "$index" -eq "$count" ] || fail 'encoded input read failed'
[ ! -s "$dir/commands" ] || tmux_run source-file - <"$dir/commands" ||
    fail 'tmux notification failed'
exit 0
WORKER
status=$?
case $status in
    0) ;;
    124|137) note 'hook timed out' ;;
    *) note 'hook worker failed' ;;
esac
exit 0
