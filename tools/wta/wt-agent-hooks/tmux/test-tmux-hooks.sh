#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

set -euo pipefail
export LC_ALL=C
umask 077

if [[ ${1:-} == --pane-worker ]]; then
    work=$2
    exec >"$work/pane.stdout" 2>"$work/pane.stderr"
    export TMPDIR="$work/scratch"
    unset WTA_TMUX_HOOKS_DISABLED OPENCODE_CLIENT WT_SESSION WT_COM_CLSID
    unset IT_SSH_HOOK_ROUTE IT_SSH_HOOK_SOCKET IT_SSH_HOOK_SESSION
    run_sender() {
        local job=$1 mode=$2 source=$3 event=$4
        local -a environment=()
        case $mode in
            normal) ;;
            disabled) environment+=(WTA_TMUX_HOOKS_DISABLED=1) ;;
            acp) environment+=(OPENCODE_CLIENT=acp) ;;
            outside) environment+=(TMUX=) ;;
            no-pane) environment+=(TMUX_PANE=) ;;
            missing-server) environment+=(TMUX="$work/missing,123,0") ;;
            no-tmux) environment+=(PATH="$work/no-tmux") ;;
            old-tmux) environment+=(PATH="$work/old-tmux") ;;
            no-control) environment+=(PATH="$work/no-control") ;;
            missing-utility) environment+=(PATH="$work/missing-utility") ;;
            scratch-timeout) environment+=(PATH="$work/scratch-timeout") ;;
            tmux-timeout) environment+=(PATH="$work/tmux-timeout") ;;
            total-timeout) environment+=(PATH="$work/total-timeout") ;;
            bad-route) environment+=(TMUX_PANE='%1;bad') ;;
            *) exit 1 ;;
        esac
        local start end code
        start=$(date +%s%N)
        env "${environment[@]}" /bin/sh "$work/it-agent-hook.sh" \
            --cli-source "$source" --event "$event" \
            <"$work/input-$job" >"$work/output-$job" 2>"$work/error-$job"
        code=$?
        end=$(date +%s%N)
        printf '%s %s\n' "$code" "$(( (end - start) / 1000000 ))" >"$work/status-$job"
    }
    while read -r job mode source event; do
        if [[ $job == concurrent ]]; then
            run_sender concurrent-a normal copilot agent.stop &
            first=$!
            run_sender concurrent-b normal copilot agent.stop &
            second=$!
            wait "$first"
            wait "$second"
        else
            run_sender "$job" "$mode" "$source" "$event"
        fi
        tmux -N -S "$work/s,1" wait-for -S "done-$job"
    done <"$work/jobs"
    exit 0
fi

work=${1:?private test directory required}
[[ $work == /tmp/it-tmux-tests.* && -d $work ]] || exit 1
for utility in tmux timeout base64 head wc mktemp rm rmdir awk cmp dd tr grep mkfifo script date ln mkdir chmod find diff cp; do
    command -v "$utility" >/dev/null || { printf 'Missing test utility: %s\n' "$utility" >&2; exit 1; }
done
sh -n "$work/it-agent-hook.sh"
bash -n "$work/test-tmux-hooks.sh"
printf 'Runtime: %s; %s\n' "$(tmux -V)" "$BASH_VERSION"

socket="$work/s,1"
declare -a client_pids=() inputs=() outputs=()
ordinary_pid=
cleanup() {
    trap - EXIT HUP INT TERM
    tmux -N -S "$socket" kill-server >/dev/null 2>&1 || [[ ! -S $socket ]] ||
        printf 'Owned tmux server cleanup failed\n' >&2
    for pid in "${client_pids[@]}" ${ordinary_pid:+"$ordinary_pid"}; do
        if kill -0 "$pid" 2>/dev/null; then
            kill -TERM "$pid" 2>/dev/null || true
        fi
        wait "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf 'PASS: %s\n' "$*"; }
tm() { timeout --kill-after=0.1 5 tmux -N -S "$socket" "$@"; }

mkdir "$work/scratch" "$work/no-tmux" "$work/missing-utility" "$work/tmux-timeout" \
    "$work/total-timeout" "$work/old-tmux" "$work/no-control" "$work/scratch-timeout"
mkfifo "$work/jobs" "$work/block" "$work/input-stalled"
exec {jobs}<>"$work/jobs"
exec {stalled}<>"$work/input-stalled"
for mode in missing-utility tmux-timeout total-timeout old-tmux no-control scratch-timeout; do
    for utility in sh timeout mktemp rm rmdir head wc base64 tmux; do
        [[ $mode != missing-utility || $utility != base64 ]] || continue
        [[ $mode != tmux-timeout || $utility != tmux ]] || continue
        [[ $mode != old-tmux || $utility != tmux ]] || continue
        [[ $mode != no-control || $utility != tmux ]] || continue
        [[ $mode != total-timeout || $utility != base64 ]] || continue
        [[ $mode != scratch-timeout || $utility != mktemp ]] || continue
        ln -s "$(command -v "$utility")" "$work/$mode/$utility"
    done
    printf '#!/bin/sh\nif [ "$4" = -V ]; then printf "tmux 3.3a\\n"; exit 0; fi\nexec /usr/bin/tmux "$@"\n' >"$work/old-tmux/tmux"
    printf '#!/bin/sh\nif [ "$4" = list-clients ]; then exit 0; fi\nexec /usr/bin/tmux "$@"\n' >"$work/no-control/tmux"
    chmod 700 "$work/old-tmux/tmux" "$work/no-control/tmux"
done
for entry in tmux-timeout/tmux total-timeout/base64 scratch-timeout/mktemp; do
    printf '#!/bin/sh\nexec /usr/bin/head -c 1 "%s/block"\n' "$work" >"$work/$entry"
    chmod 700 "$work/$entry"
done

printf -v worker_command 'exec bash %q --pane-worker %q' "$work/test-tmux-hooks.sh" "$work"
read -r origin pane < <(tmux -S "$socket" -f /dev/null new-session -d -s origin \
    -P -F '#{session_id} #{pane_id}' "$worker_command")
other=$(tm new-session -d -s other -P -F '#{session_id}' cat)
for index in 0 1 2; do
    mkfifo "$work/client-$index.in" "$work/client-$index.out"
    exec {input}<>"$work/client-$index.in"
    session=$origin
    [[ $index != 2 ]] || session=$other
    tmux -N -S "$socket" -C attach-session -t "$session" \
        <"$work/client-$index.in" >"$work/client-$index.out" 2>"$work/client-$index.err" &
    client_pids+=("$!")
    exec {output}<"$work/client-$index.out"
    inputs+=("$input")
    outputs+=("$output")
    : >"$work/client-$index.log"
done

barrier() {
    local index=$1 marker="barrier-$RANDOM-$RANDOM" line found=
    printf 'display-message -p -l %s\n' "$marker" >&"${inputs[index]}"
    while IFS= read -r -t 5 line <&"${outputs[index]}"; do
        printf '%s\n' "$line" >>"$work/client-$index.log"
        [[ $line != "$marker" ]] || found=1
        if [[ -n $found && $line == '%end '* ]]; then return; fi
        [[ $line != '%error '* ]] || fail 'control command failed'
    done
    fail 'control client barrier timed out'
}
drain() { for index in 0 1 2; do barrier "$index"; done; }
clear_logs() { drain; for index in 0 1 2; do : >"$work/client-$index.log"; done; }
drain

mkfifo "$work/ordinary.in"
exec {ordinary}<>"$work/ordinary.in"
tm set-hook -g client-attached 'wait-for -S ordinary-ready'
script -qefc "tmux -N -S '$socket' attach-session -t '$origin'" /dev/null \
    <"$work/ordinary.in" >"$work/ordinary.out" 2>"$work/ordinary.err" &
ordinary_pid=$!
tm wait-for ordinary-ready
tm set-hook -gu client-attached
[[ $(tm list-clients -t "$origin" -F '#{client_control_mode}' | grep -c '^0$') == 1 ]] ||
    fail 'ordinary client was not attached'
tm link-window -s "$pane" -t "$other:"

check_status() {
    local job=$1 expected=${2:-} code elapsed
    read -r code elapsed <"$work/status-$job"
    [[ $code == 0 ]] || fail "$job returned nonzero"
    [[ ! -s $work/output-$job ]] || fail "$job wrote stdout"
    [[ $elapsed -lt 4000 ]] || fail "$job exceeded hook deadline (${elapsed}ms)"
    if [[ -n $expected ]]; then
        [[ $(<"$work/error-$job") == "it-agent-hook: $expected" ]] || fail "$job diagnostic mismatch"
    else
        [[ ! -s $work/error-$job ]] || fail "$job unexpectedly wrote stderr: $(<"$work/error-$job")"
    fi
    [[ -z $(find "$work/scratch" -mindepth 1 -print -quit) ]] || fail "$job left private scratch files"
}
emit_job() {
    local job=$1 mode=${2:-normal} source=${3:-copilot} event=${4:-agent.stop}
    printf '%s %s %s %s\n' "$job" "$mode" "$source" "$event" >&"$jobs"
    tm wait-for "done-$job"
    drain
}
no_messages() {
    for index in 0 1 2; do
        if grep -q '^%message ' "$work/client-$index.log"; then fail 'unexpected notification'; fi
    done
}
verify() {
    local expected_count=$1 expected_source=${2:-copilot} expected_event=${3:-agent.stop}
    local wire_version=${4:-2} expected_route=${5:-} second_route=${6:-}
    local index directory file
    if grep -q '^%message ' "$work/client-2.log"; then fail 'other session received a notification'; fi
    for index in 0 1; do
        directory="$work/check-$index"
        mkdir "$directory"
        awk -v directory="$directory" -v session="$origin" -v pane="$pane" \
            -v source="$expected_source" -v event="$expected_event" -v expected="$expected_count" \
            -v version="$wire_version" -v route="$expected_route" -v second_route="$second_route" '
            /^%message / {
                line = substr($0, 10)
                split(line, part, " ")
                if(version==2) {
                    if(part[1]!="IT_AGENT_HOOK/2" || part[2]!=session || part[3]!=pane ||
                       part[4]!=source || part[5]!=event) exit 1
                    identity=part[2] " " part[3] " " part[4] " " part[5]
                    token=part[6]; part_index=part[7]; count=part[8]; data=part[9]
                } else {
                    if(part[1]!="IT_AGENT_HOOK/3" || (part[2]!=route && part[2]!=second_route) ||
                       part[3]!=source || part[4]!=event) exit 1
                    identity=part[2] " " part[3] " " part[4]
                    token=part[5]; part_index=part[6]; count=part[7]; data=part[8]
                }
                if (length(line) > 8192 || token !~ /^[A-Za-z0-9._-]+$/ || length(token) > 64 ||
                    part_index !~ /^(0|[1-9][0-9]*)$/ || count !~ /^[1-9][0-9]*$/ ||
                    count > 234 || part_index >= count || length(data) > 6000 || length(data) % 4 ||
                    data !~ /^[A-Za-z0-9+\/]*={0,2}$/) exit 1
                prefix = sprintf("IT_AGENT_HOOK/%s %s %s %s %s ", version, identity, token, part_index, count)
                if (line != prefix data) exit 2
                if (part_index + 1 < count && (length(data) != 6000 || data ~ /=/)) exit 3
                if (length(data) == 0 && !(count == 1 && part_index == 0)) exit 4
                if (!(token in seen)) { seen[token] = count; identities[token]=identity; transfers++ }
                if (seen[token] != count || identities[token]!=identity || next_index[token] != part_index) exit 5
                next_index[token]++
                printf "%s", data > (directory "/" token ".b64")
            }
            END {
                if (transfers != expected) exit 6
                for (token in seen) if (next_index[token] != seen[token]) exit 7
            }
        ' "$work/client-$index.log" || fail "invalid/missing v$wire_version frame for client $index (validator=$?, messages=$(grep -c '^%message ' "$work/client-$index.log" || :))"
        for file in "$directory"/*.b64; do
            base64 -d "$file" >"${file%.b64}.raw" || fail 'invalid base64'
        done
    done
    diff -r "$work/check-0" "$work/check-1" || fail 'origin clients did not receive identical transfers'
}
finish_check() {
    local file
    for file in "$work/check-0"/* "$work/check-1"/*; do rm -- "$file"; done
    rmdir "$work/check-0" "$work/check-1"
}
one_body() {
    local job=$1 source=${2:-copilot} event=${3:-agent.stop} file
    verify 1 "$source" "$event"
    for file in "$work/check-0"/*.raw; do cmp "$work/input-$job" "$file" || fail "$job raw bytes changed"; done
    finish_check
}

printf '{"session_id":"sidekick-raw","sessionId":"conflict","prompt":"private","tool_output":"raw"}\r\ninvalid JSON "\377\000\344\275\240\\n%% #{session_name}\n' >"$work/input-raw"
clear_logs
emit_job raw
check_status raw
one_body raw
pass 'raw invalid JSON, conflicting IDs, sidekick IDs, Unicode and binary bytes are unchanged'

: >"$work/input-empty"
clear_logs
emit_job empty
check_status empty
one_body empty
pass 'empty stdin retains the final framing space'

for source in claude copilot codex gemini opencode; do
    printf '{"source":"%s"}' "$source" >"$work/input-source"
    clear_logs
    emit_job source normal "$source" agent.notification
    check_status source
    one_body source "$source" agent.notification
done
pass 'all five canonical sources use the existing event vocabulary'
for event in agent.session.start agent.session.end agent.prompt.submit agent.notification \
    agent.tool.starting agent.stop agent.error agent.subagent.stop; do
    clear_logs
    emit_job source normal copilot "$event"
    check_status source
    one_body source copilot "$event"
done
for failure in bad-source bad-event; do
    clear_logs
    if [[ $failure == bad-source ]]; then
        emit_job source normal unknown agent.stop
        check_status source 'invalid source'
    else
        emit_job source normal copilot agent.tool.finished
        check_status source 'invalid event'
    fi
    no_messages
done
pass 'canonical events are accepted; unknown sources/events are rejected privately'

printf '%s' '{"session_id":"native-fixture-session","cwd":"/repo","message":"literal \\n; #{session_name}; %Y","prompt":"TEST_PROMPT_REDACT_ME","tool_output":"' >"$work/input-capture"
printf '\344\275\240\345\245\275' >>"$work/input-capture"
head -c 5000 /dev/zero | tr '\000' x >>"$work/input-capture"
printf '"}\r\n' >>"$work/input-capture"
clear_logs
emit_job capture
check_status capture
one_body capture
grep '^%message ' "$work/client-0.log" >"$work/real-shell-v2.messages"
cp "$work/input-capture" "$work/real-shell-v2.payload"
[[ $(wc -l <"$work/real-shell-v2.messages") == 2 ]] || fail 'capture must contain two real chunks'
pass 'real two-chunk capture and exact raw body are available for native tests'

printf '{"tool_output":"' >"$work/input-large"
for ((i=0; i<10000; i++)); do printf '\344\275\240\345\245\275\\n' >>"$work/input-large"; done
printf '"}\r\n' >>"$work/input-large"
head -c 1048576 /dev/zero >"$work/input-limit"
head -c 1048577 /dev/zero >"$work/input-over"
for job in large limit; do
    clear_logs
    emit_job "$job"
    check_status "$job"
    if [[ $job == limit ]]; then
        [[ $(grep -c '^%message ' "$work/client-0.log") == 234 ]] || fail '1MiB did not produce 234 parts'
    fi
    one_body "$job"
done
read -r code elapsed <"$work/status-limit"
pass "large Unicode/tool output and exactly 1MiB reassemble byte-for-byte (${elapsed}ms for 1MiB)"
clear_logs
emit_job over
check_status over 'stdin size limit exceeded'
no_messages
pass '1MiB+1 is rejected without partial delivery'

for job in concurrent-a concurrent-b; do
    cp "$work/input-large" "$work/input-$job"
    printf '%s raw body\n' "$job" >>"$work/input-$job"
done
clear_logs
emit_job concurrent
check_status concurrent-a
check_status concurrent-b
verify 2
for job in concurrent-a concurrent-b; do
    matched=
    for file in "$work/check-0"/*.raw; do
        if cmp -s "$work/input-$job" "$file"; then matched=1; fi
    done
    [[ -n $matched ]] || fail 'concurrent body missing'
done
finish_check
pass 'concurrent hooks have distinct transfer IDs and independently reassemble'

for mode in disabled outside no-pane no-tmux old-tmux no-control missing-server acp; do
    : >"$work/input-noop"
    clear_logs
    emit_job noop "$mode" opencode
    check_status noop
    no_messages
done
[[ ! -e $work/missing ]] || fail 'sender created a missing server socket'
pass 'missing tmux/environment and explicit opt-outs are silent no-ops'
for mode in missing-utility scratch-timeout tmux-timeout total-timeout bad-route; do
    : >"$work/input-failure"
    clear_logs
    emit_job failure "$mode"
    case $mode in
        missing-utility) diagnostic='required utility unavailable' ;;
        scratch-timeout) diagnostic='private scratch directory unavailable' ;;
        tmux-timeout) diagnostic='tmux version lookup failed' ;;
        total-timeout) diagnostic='hook timed out' ;;
        bad-route) diagnostic='invalid tmux environment' ;;
    esac
    check_status failure "$diagnostic"
    no_messages
done
clear_logs
printf '{"partial":"private"' >&"$stalled"
emit_job stalled
check_status stalled 'stdin read failed or timed out'
no_messages
pass 'missing utility, command/overall/stdin timeouts stay private, bounded and clean'

tm unlink-window -t "$other:1"
tm new-window -d -t "$origin" cat
tm move-window -s "$pane" -t "$other:"
clear_logs
emit_job raw
check_status raw
no_messages
[[ -z $(tm capture-pane -p -t "$pane" | tr -d '\n') ]] || fail 'sender wrote terminal output'
if grep -q 'IT_AGENT_HOOK/' "$work/ordinary.out"; then fail 'ordinary client received hook output'; fi
pass 'moved pane never reroutes, linked sessions stay isolated, no terminal/status injection'

tm new-session -d -s it-hooks cat
tm set-option -g @it-ssh-hooks-protocol 3
for index in 0 1; do
    printf 'switch-client -t =it-hooks\n' >&"${inputs[index]}"
done
drain
[[ $(tm list-clients -t '=it-hooks' -F '#{client_control_mode}' | grep -c '^1$') == 2 ]] ||
    fail 'v3 control clients did not switch to the dedicated session'
[[ $(tm show-options -gqv @it-ssh-hooks-protocol) == 3 ]] || fail 'v3 protocol marker missing'
route_a=11111111-2222-4333-8444-555555555555
route_b=aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee
emit_v3() {
    local job=$1 route=$2 mode=${3:-normal} start end code
    local -a environment=(-u TMUX -u TMUX_PANE -u WTA_TMUX_HOOKS_DISABLED -u OPENCODE_CLIENT)
    environment+=("IT_SSH_HOOK_ROUTE=$route" "IT_SSH_HOOK_SOCKET=$socket" IT_SSH_HOOK_SESSION=it-hooks "TMPDIR=$work/scratch")
    case $mode in
        normal) ;;
        no-channel)
            environment+=("IT_SSH_HOOK_SOCKET=$work/absent" "TMUX=$socket,123,${other#\$}" "TMUX_PANE=$pane") ;;
        bad-session) environment+=(IT_SSH_HOOK_SESSION=other) ;;
        disabled) environment+=(WTA_TMUX_HOOKS_DISABLED=1) ;;
        acp) environment+=(OPENCODE_CLIENT=acp) ;;
    esac
    start=$(date +%s%N)
    env "${environment[@]}" /bin/sh "$work/it-agent-hook.sh" --cli-source "${4:-copilot}" --event agent.stop \
        <"$work/input-$job" >"$work/output-$job" 2>"$work/error-$job"
    code=$?
    end=$(date +%s%N)
    printf '%s %s\n' "$code" "$(( (end - start) / 1000000 ))" >"$work/status-$job"
}
for job in raw empty large limit; do
    clear_logs
    emit_v3 "$job" "$route_a"
    check_status "$job"
    drain
    verify 1 copilot agent.stop 3 "$route_a"
    for file in "$work/check-0"/*.raw; do cmp "$work/input-$job" "$file" || fail 'v3 changed raw bytes'; done
    finish_check
done
clear_logs
emit_v3 over "$route_a"
check_status over 'stdin size limit exceeded'
drain
no_messages
pass 'v3 ordinary shells preserve raw/Unicode/empty/1MiB bodies and reject overflow'

clear_logs
emit_v3 concurrent-a "$route_a" &
first=$!
emit_v3 concurrent-b "$route_b" &
second=$!
wait "$first"
wait "$second"
check_status concurrent-a
check_status concurrent-b
drain
verify 2 copilot agent.stop 3 "$route_a" "$route_b"
for job in concurrent-a concurrent-b; do
    matched=
    for file in "$work/check-0"/*.raw; do
        if cmp -s "$work/input-$job" "$file"; then matched=1; fi
    done
    [[ -n $matched ]] || fail 'v3 route body missing'
done
finish_check
for invalid in '' not-a-uuid "$route_a-" 11111111-2222-4333-8444-55555555555z; do
    clear_logs
    emit_v3 raw "$invalid"
    check_status raw 'invalid managed SSH route'
    drain
    no_messages
done
for mode in no-channel disabled acp bad-session; do
    clear_logs
    emit_v3 raw "$route_a" "$mode" opencode
    if [[ $mode == bad-session ]]; then check_status raw 'invalid managed SSH session'
    else check_status raw; fi
    drain
    no_messages
done
tm set-option -gu @it-ssh-hooks-protocol
clear_logs
emit_v3 raw "$route_a"
check_status raw
drain
no_messages
pass 'v3 isolates concurrent routes, rejects malformed routes, and never falls back to v2 or unmarked servers'
printf 'All shell sender transport tests passed.\n'
