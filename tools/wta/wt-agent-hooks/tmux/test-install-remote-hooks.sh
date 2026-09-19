#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

set -euo pipefail
export LC_ALL=C
umask 077
work=${1:?private test directory required}
[[ $work == /tmp/it-tmux-tests.* && -d $work ]] || exit 1
bin="$work/install-bin"
mkdir "$bin"
declare -a sockets=() clients=()
cleanup() {
    trap - EXIT HUP INT TERM
    for socket in "${sockets[@]}"; do
        [[ -S $socket && ! -L $socket ]] || continue
        error=$(tmux -N -S "$socket" kill-server 2>&1) || case $error in
            *'no server running'*|*'No such file or directory'*) ;;
            *) printf 'Owned installer test socket cleanup failed\n' >&2 ;;
        esac
    done
    for pid in "${clients[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then kill -TERM "$pid" 2>/dev/null || true; fi
        wait "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
fail() { printf 'FAIL installer: %s\n' "$*" >&2; exit 1; }
pass() { printf 'PASS installer: %s\n' "$*"; }
for utility in sh tmux timeout mktemp rm rmdir head wc base64 stat id flock sha256sum \
    awk sed readlink mkdir mv ln cp find sort cat chmod dirname; do
    ln -s "$(command -v "$utility")" "$bin/$utility"
done
cat >"$bin/getent" <<'SH'
#!/bin/sh
printf 'test-user:x:%s:%s:Test:%s:/bin/sh\n' "$(id -u)" "$(id -g)" "$TEST_LOGIN_HOME"
SH
chmod 700 "$bin/getent"
cat >"$work/fake-provider" <<'SH'
#!/bin/bash
set -euo pipefail
name=${0##*/}
if [[ $name == snap && ${1:-} == run ]]; then
    printf '%s\n' probed >>"$TEST_STATE/snap-probe"
    shift 3
    case ${TEST_SNAP_MODE:-unavailable} in
        unavailable) export PATH="$TEST_SNAP_TOOLS" ;;
        available) ;;
        *) exit 2 ;;
    esac
    exec /bin/sh "$@"
fi
[[ -z ${IT_SSH_HOOK_ROUTE+x} ]] || exit 9
if IFS= read -r unexpected; then exit 10; fi
export HOME="$TEST_RUNTIME_HOME"
state="$TEST_STATE/$name"
mkdir -p "$state"
quote() {
    local value=$1
    value=${value//\\/\\\\}
    value=${value//\"/\\\"}
    printf '"%s"' "$value"
}
printf '%s %s\n' "$name" "$*" >>"$TEST_STATE/calls"
if [[ ${1:-} == plugin && ${2:-} == marketplace ]]; then
    case ${3:-} in
        list)
            if [[ -f $state/market ]]; then
                printf '[{"name":"it-ssh-local","source":'; quote "$(<"$state/market")"; printf '}]\n'
            else printf '[]\n'; fi ;;
        add) printf '%s' "$4" >"$state/market" ;;
        update) [[ -f $state/market ]] ;;
        *) exit 2 ;;
    esac
    exit 0
fi
if [[ ${2:-} == list ]]; then
    if [[ $name == copilot ]]; then
        if [[ ${TEST_COMPACT_SOURCE:-} == 1 ]]; then
            mkdir -p "$HOME/.copilot"
            {
                printf '// CLI-owned registration metadata\n{"installedPlugins":[\n'
                for entry in legacy-installed installed; do
                    [[ -f $state/$entry ]] || continue
                    identity=it-ssh-hooks
                    [[ $entry != legacy-installed ]] || identity=it-tmux-hooks
                    printf '{"name":'; quote "$identity"
                    printf ',"source":{"source":"local","path":'; quote "$(<"$state/$entry")"
                    printf ',},},\n'
                done
                printf '],/* comments and trailing commas are legal here */}\n'
            } >"$HOME/.copilot/config.json"
        fi
        printf '['
        separator=
        if [[ -f $state/legacy-installed ]]; then
            enabled=true; [[ ! -f $state/legacy-disabled ]] || enabled=false
            printf '{"name":"it-tmux-hooks","version":"2.0.0","enabled":%s' "$enabled"
            if [[ ${TEST_COMPACT_SOURCE:-} == 1 ]]; then printf ',"source":"installed"}'
            else printf ',"source":"local","installedFrom":'; quote "$(<"$state/legacy-installed")"; printf '}'; fi
            separator=,
        fi
        if [[ -f $state/installed ]]; then
            enabled=true; [[ ! -f $state/disabled ]] || enabled=false
            printf '%s{"name":"it-ssh-hooks","version":"3.0.0","description":"\\u4f60\\u597d \\ud83d\\ude00","enabled":%s' "$separator" "$enabled"
            if [[ ${TEST_COMPACT_SOURCE:-} == 1 ]]; then printf ',"source":"installed"}'
            else printf ',"source":"local","installedFrom":'; quote "$(<"$state/installed")"; printf '}'; fi
        fi
        printf ']\n'
    elif [[ ! -f $state/installed ]]; then
        printf '[]\n'
    elif [[ $name == gemini ]]; then
        [[ ${3:-} == --output-format && ${4:-} == json ]] || exit 2
        enabled=true; [[ ! -f $state/disabled ]] || enabled=false
        printf '[{"name":"it-ssh-hooks","version":"3.0.0","isActive":%s,"installMetadata":{"source":' "$enabled"
        quote "$(<"$state/installed")"; printf '}}]\n'
    else
        enabled=true; [[ ! -f $state/disabled ]] || enabled=false
        identity=it-ssh-hooks@it-ssh-local
        [[ ! -f $state/identity ]] || identity=$(<"$state/identity")
        printf '[{"id":'; quote "$identity"
        printf ',"version":"3.0.0","enabled":%s,"installPath":"/private-cli-cache"}]\n' "$enabled"
    fi
    exit 0
fi
case ${2:-} in
    install)
        if [[ $name == copilot && -f $state/legacy-installed ]]; then
            : >"$state/duplicate-at-install"
        fi
        if [[ $name == copilot || $name == gemini ]]; then
            printf '%s' "$3" >"$state/installed"
        else
            printf '%s/it-ssh-hooks' "$(<"$state/market")" >"$state/installed"
        fi
        if [[ ${TEST_FAIL_ONCE:-} == "$name" && ! -f $state/failed-once ]]; then
            : >"$state/failed-once"
            exit 1
        fi ;;
    update)
        if [[ ${TEST_FAIL_UPDATE_ONCE:-} == "$name" && ! -f $state/update-failed-once ]]; then
            rm "$state/installed"
            : >"$state/update-failed-once"
            exit 1
        fi
        [[ -f $state/installed && ! -f $state/disabled ]] ;;
    uninstall)
        [[ $name == copilot && $3 == it-tmux-hooks && -f $state/legacy-installed && ! -f $state/legacy-disabled ]] || exit 2
        if [[ ${TEST_KEEP_LEGACY:-} == 1 ]]; then exit 0; fi
        rm "$state/legacy-installed"
        if [[ ${TEST_FAIL_LEGACY_UNINSTALL_ONCE:-} == 1 && ! -f $state/legacy-uninstall-failed ]]; then
            : >"$state/legacy-uninstall-failed"
            exit 1
        fi ;;
    *) exit 2 ;;
esac
SH
chmod 700 "$work/fake-provider"
for provider in claude copilot codex gemini opencode; do
    ln -s "$work/fake-provider" "$bin/$provider"
done
mkdir "$work/snap-tools"
ln -s /bin/sh "$work/snap-tools/sh"
export TEST_SNAP_TOOLS="$work/snap-tools"
export TEST_RUNTIME_HOME="$work/cli-runtime-home"
mkdir "$TEST_RUNTIME_HOME"
cp "$work/it-agent-hook.sh" "$work/upload-hook.sh"
new_case() {
    TEST_LOGIN_HOME="$work/$1"
    TEST_STATE="$work/state-$2"
    export TEST_LOGIN_HOME TEST_STATE
    mkdir "$TEST_LOGIN_HOME" "$TEST_STATE"
    sockets+=("$TEST_LOGIN_HOME/.intelligent-terminal/run/tmux-hooks.sock")
    : >"$TEST_STATE/calls"
}
run_install() {
    env PATH="$bin" HOME="$TEST_RUNTIME_HOME" IT_SSH_HOOK_ROUTE=11111111-2222-4333-8444-555555555555 \
        /bin/sh "$work/install-remote-hooks.sh" --hook-source "${TEST_UPLOAD_SOURCE:-$work/upload-hook.sh}" \
        --login-home "$TEST_LOGIN_HOME" --allowed-clis claude,copilot,codex,gemini,opencode \
        "$@" >"$work/install.stdout" 2>"$work/install.stderr"
}
expect_success() {
    run_install "$@" || { cat "$work/install.stderr" >&2; fail 'setup failed'; }
    [[ ! -s $work/install.stdout ]] || fail 'setup polluted stdout'
}
create_legacy_bundle() {
    legacy="$TEST_LOGIN_HOME/.intelligent-terminal/plugins/it-tmux-hooks"
    mkdir -p "$legacy/scripts" "$TEST_STATE/copilot"
    printf '%s\n' 'Intelligent Terminal managed remote hook bundle v2' >"$legacy/.it-managed"
    printf '%s\n' '{"name":"it-tmux-hooks","version":"2.0.0"}' >"$legacy/plugin.json"
    printf '#!/bin/sh\n# Historical fixture: IT_AGENT_HOOK/2\nexit 0\n' >"$legacy/scripts/it-agent-hook.sh"
    chmod 700 "$legacy/scripts/it-agent-hook.sh"
    printf '%s' "$legacy" >"$TEST_STATE/copilot/legacy-installed"
}

new_case 'h '"'"'"\$' fresh
home_fresh=$TEST_LOGIN_HOME
state_fresh=$TEST_STATE
expect_success
hooks="$TEST_LOGIN_HOME/.intelligent-terminal/ssh-hooks"
[[ -f $hooks/manifest && ! -e $hooks/pending ]] || fail 'manifest was not committed last'
[[ ! -e $TEST_RUNTIME_HOME/.intelligent-terminal ]] || fail 'installer used CLI runtime HOME'
for provider in claude copilot codex gemini; do
    grep -q "provider=$provider result=installed" "$work/install.stderr" || fail "$provider not installed"
    [[ -f $hooks/receipts/$provider ]] || fail "$provider receipt missing"
done
grep -q 'provider=opencode result=unsupported-registration-api' "$work/install.stderr" ||
    fail 'OpenCode unsupported API was not reported'
[[ $(tmux -N -S "${sockets[0]}" show-options -gqv @it-ssh-hooks-protocol) == 3 ]] ||
    fail 'dedicated server protocol marker missing'
generation=$(readlink "$hooks/current")
calls=$(grep -c ' install ' "$TEST_STATE/calls")
expect_success
[[ $(readlink "$hooks/current") == "$generation" ]] || fail 'same assets changed generation'
[[ $(grep -c ' install ' "$TEST_STATE/calls") == "$calls" ]] || fail 'idempotent setup reinstalled plugins'
cp "$work/upload-hook.sh" "$TEST_LOGIN_HOME/alternate 'upload.sh"
TEST_UPLOAD_SOURCE="$TEST_LOGIN_HOME/alternate 'upload.sh" expect_success
[[ $(readlink "$hooks/current") == "$generation" ]] || fail 'upload path affected asset hash'
printf '\n# upgrade fixture\n' >>"$work/upload-hook.sh"
expect_success
[[ $(readlink "$hooks/current") != "$generation" ]] || fail 'changed assets did not upgrade'
grep -q 'copilot plugin update it-ssh-hooks' "$TEST_STATE/calls" || fail 'upgrade did not use CLI API'
pass 'fresh/idempotent/upgrade installs use original login HOME and managed CLI APIs'

mkfifo "$work/installer-control.in" "$work/installer-control.out"
exec {control_input}<>"$work/installer-control.in"
env PATH="$bin" HOME="$TEST_RUNTIME_HOME" /bin/sh "$work/install-remote-hooks.sh" \
    --hook-source "$work/upload-hook.sh" --login-home "$TEST_LOGIN_HOME" --attach-control \
    --allowed-clis claude,copilot,codex,gemini,opencode \
    <"$work/installer-control.in" >"$work/installer-control.out" 2>"$work/attach.stderr" &
controller=$!
clients+=("$controller")
exec {control_output}<"$work/installer-control.out"
printf "display-message -p 'IT_HOOK_READY/3 installer-stdin-preserved #{@it-ssh-hooks-protocol} #{@it-ssh-hooks-installed-clis} #{@it-ssh-hooks-unavailable-clis}'\n" >&"$control_input"
first_line=1
ready=
while IFS= read -r -t 10 line <&"$control_output"; do
    if [[ -n $first_line ]]; then
        [[ $line == '%begin '* ]] || fail 'installer polluted control stdout'
        first_line=
    fi
    if [[ $line == 'IT_HOOK_READY/3 installer-stdin-preserved 3 claude,copilot,codex,gemini opencode' ]]; then ready=1; break; fi
done
[[ -n $ready ]] || fail 'installer consumed or closed control stdin'
expect_success
printf 'detach-client\n' >&"$control_input"
wait "$controller" || fail 'owned control client failed to detach'
clients=()
exec {control_input}>&-
exec {control_output}<&-
tmux -N -S "${sockets[0]}" has-session -t '=it-hooks' || fail 'controller disconnect killed shared server'
pass 'setup preserves queued control stdin, releases its lock, and keeps the shared server alive'

: >"$TEST_STATE/copilot/disabled"
updates=$(grep -c 'copilot plugin update' "$TEST_STATE/calls")
printf '\n# disabled fixture\n' >>"$work/upload-hook.sh"
expect_success
grep -q 'provider=copilot result=user-disabled' "$work/install.stderr" || fail 'disabled plugin ignored'
[[ $(tmux -N -S "${sockets[0]}" show-options -gqv @it-ssh-hooks-unavailable-clis) == copilot,opencode ]] ||
    fail 'disabled detected provider unavailable status missing'
[[ $(grep -c 'copilot plugin update' "$TEST_STATE/calls") == "$updates" ]] || fail 'disabled plugin was updated'
[[ -f $TEST_STATE/copilot/disabled ]] || fail 'disabled plugin was re-enabled'
cp "$hooks/current/it-agent-hook.sh" "$work/saved-managed-hook"
printf '\n# operator edit\n' >>"$hooks/current/it-agent-hook.sh"
if run_install; then fail 'modified managed file was overwritten'; fi
grep -q 'modified-managed-files' "$work/install.stderr" || fail 'modified managed file failure missing'
cp "$work/saved-managed-hook" "$hooks/current/it-agent-hook.sh"
pass 'user-disabled plugins and operator-modified managed files are preserved'

: >"$work/installer-evidence"
for provider in claude copilot codex gemini; do
    directory="$hooks/current/$provider/it-ssh-hooks"
    case $provider in
        claude) manifest=.claude-plugin/plugin.json ;;
        copilot) manifest=plugin.json ;;
        codex) manifest=.codex-plugin/plugin.json ;;
        gemini) manifest=gemini-extension.json ;;
    esac
    printf '%s\t' "$provider" >>"$work/installer-evidence"
    printf '%s' "$hooks/current/it-agent-hook.sh" | base64 --wrap=0 >>"$work/installer-evidence"
    printf '\t' >>"$work/installer-evidence"
    base64 --wrap=0 "$directory/hooks/hooks.json" >>"$work/installer-evidence"
    printf '\t' >>"$work/installer-evidence"
    base64 --wrap=0 "$directory/$manifest" >>"$work/installer-evidence"
    printf '\n' >>"$work/installer-evidence"
done

new_case retry retry
export TEST_FAIL_ONCE=copilot
if run_install; then fail 'partial installation reported success'; fi
hooks="$TEST_LOGIN_HOME/.intelligent-terminal/ssh-hooks"
[[ -f $hooks/pending && -f $hooks/receipts/copilot.pending && ! -e $hooks/manifest ]] ||
    fail 'partial transaction ownership was lost'
unset TEST_FAIL_ONCE
expect_success
[[ -f $hooks/manifest && ! -e $hooks/receipts/copilot.pending ]] || fail 'partial setup was not repaired'
pass 'API failure after a side effect is recoverable with manifest-last commit'
printf '\n# update retry fixture\n' >>"$work/upload-hook.sh"
export TEST_FAIL_UPDATE_ONCE=copilot
if run_install; then fail 'failed update reported success'; fi
[[ -f $hooks/receipts/copilot.pending && ! -f $TEST_STATE/copilot/installed ]] ||
    fail 'failed update transaction was lost'
unset TEST_FAIL_UPDATE_ONCE
expect_success
[[ -f $TEST_STATE/copilot/installed && ! -f $hooks/receipts/copilot.pending ]] ||
    fail 'update failure was mistaken for operator removal'
tmux -N -S "$TEST_LOGIN_HOME/.intelligent-terminal/run/tmux-hooks.sock" set-option -gu @it-ssh-hooks-protocol
expect_success
[[ $(tmux -N -S "$TEST_LOGIN_HOME/.intelligent-terminal/run/tmux-hooks.sock" show-options -gqv @it-ssh-hooks-protocol) == 3 ]] ||
    fail 'owned server reservation was not repaired'
pass 'interrupted update and dedicated-server initialization are repairable'

new_case foreign foreign
mkdir "$TEST_STATE/copilot"
printf '/foreign/user/plugin' >"$TEST_STATE/copilot/installed"
expect_success
grep -q 'provider=copilot result=foreign-plugin' "$work/install.stderr" || fail 'foreign plugin not detected'
[[ $(<"$TEST_STATE/copilot/installed") == /foreign/user/plugin ]] || fail 'foreign plugin changed'
if grep -q 'copilot plugin install\|copilot plugin update' "$TEST_STATE/calls"; then fail 'foreign plugin API mutation'; fi
pass 'foreign same-name CLI plugin is not overwritten'

new_case fq foreign-qualified
mkdir "$TEST_STATE/claude"
printf '/foreign/user/plugin' >"$TEST_STATE/claude/installed"
printf 'it-ssh-hooks@another-market' >"$TEST_STATE/claude/identity"
expect_success
grep -q 'provider=claude result=foreign-plugin' "$work/install.stderr" ||
    fail 'same-name plugin from another marketplace was not detected'
pass 'foreign marketplace-qualified plugin identities are preserved'

new_case fm foreign-market
mkdir "$TEST_STATE/claude"
printf '/foreign/user/marketplace' >"$TEST_STATE/claude/market"
expect_success
grep -q 'provider=claude result=foreign-marketplace' "$work/install.stderr" ||
    fail 'foreign marketplace was not detected'
[[ $(<"$TEST_STATE/claude/market") == /foreign/user/marketplace ]] || fail 'foreign marketplace was changed'

new_case ff foreign-files
mkdir -p "$TEST_LOGIN_HOME/.intelligent-terminal/ssh-hooks"
printf 'user-owned file' >"$TEST_LOGIN_HOME/.intelligent-terminal/ssh-hooks/it-agent-hook.sh"
if run_install; then fail 'foreign managed directory was accepted'; fi
grep -q foreign-managed-directory "$work/install.stderr" || fail 'foreign directory diagnostic missing'
[[ $(<"$TEST_LOGIN_HOME/.intelligent-terminal/ssh-hooks/it-agent-hook.sh") == 'user-owned file' ]] ||
    fail 'foreign file changed'
pass 'foreign same-name marketplace and files are not overwritten'

new_case removed removed
expect_success
installs=$(grep -c '^copilot plugin install ' "$TEST_STATE/calls")
rm "$TEST_STATE/copilot/installed"
expect_success
grep -q 'provider=copilot result=user-removed' "$work/install.stderr" || fail 'user removal was ignored'
[[ $(grep -c '^copilot plugin install ' "$TEST_STATE/calls") == "$installs" ]] || fail 'removed plugin reinstalled'
pass 'operator removal is not mistaken for an incomplete installation'

new_case legacy legacy
create_legacy_bundle
legacy_hash=$(sha256sum <"$legacy/scripts/it-agent-hook.sh")
expect_success --allowed-clis copilot
hooks="$TEST_LOGIN_HOME/.intelligent-terminal/ssh-hooks"
[[ ! -f $TEST_STATE/copilot/legacy-installed && -f $TEST_STATE/copilot/installed ]] ||
    fail 'owned legacy registration was not replaced'
[[ ! -f $TEST_STATE/copilot/duplicate-at-install ]] || fail 'migration installed duplicate hooks'
[[ $(sha256sum <"$legacy/scripts/it-agent-hook.sh") == "$legacy_hash" ]] || fail 'old bundle was rewritten in place'
[[ -f $hooks/receipts/copilot.legacy.migrated && ! -e $hooks/receipts/copilot.legacy.pending ]] ||
    fail 'migration receipt was not committed'
[[ $(tmux -N -S "$TEST_LOGIN_HOME/.intelligent-terminal/run/tmux-hooks.sock" show-options -gqv @it-ssh-hooks-installed-clis) == copilot ]] ||
    fail 'completed migration did not advertise the verified v3 registration'
grep -q 'copilot plugin uninstall it-tmux-hooks' "$TEST_STATE/calls" || fail 'migration bypassed native CLI uninstall'
expect_success --allowed-clis copilot
[[ $(grep -c '^copilot plugin uninstall it-tmux-hooks$' "$TEST_STATE/calls") == 1 ]] ||
    fail 'completed migration ran more than once'
pass 'verified owned v2 Copilot bundle migrates without duplicate registration or in-place source edits'

new_case compact compact-source
create_legacy_bundle
export TEST_COMPACT_SOURCE=1
expect_success --allowed-clis copilot
grep -q 'provider=copilot result=installed' "$work/install.stderr" ||
    fail 'compact CLI output did not use owned JSONC registration metadata'
[[ ! -f $TEST_STATE/copilot/duplicate-at-install ]] || fail 'compact source migration duplicated plugins'
expect_success --allowed-clis copilot
[[ $(grep -c '^copilot plugin install ' "$TEST_STATE/calls") == 1 ]] ||
    fail 'compact source registration was not idempotent'
: >"$TEST_STATE/copilot/disabled"
expect_success --allowed-clis copilot
grep -q 'provider=copilot result=user-disabled' "$work/install.stderr" ||
    fail 'JSONC origin overrode disabled CLI status'
rm "$TEST_STATE/copilot/disabled"
printf '/foreign/user/it-ssh-hooks' >"$TEST_STATE/copilot/installed"
expect_success --allowed-clis copilot
grep -q 'provider=copilot result=foreign-plugin' "$work/install.stderr" ||
    fail 'compact source accepted a foreign registered origin'
[[ $(grep -c '^copilot plugin install ' "$TEST_STATE/calls") == 1 ]] ||
    fail 'compact source conflict rewrote the plugin registration'
unset TEST_COMPACT_SOURCE
rm -f "$TEST_RUNTIME_HOME/.copilot/config.json"
rmdir "$TEST_RUNTIME_HOME/.copilot"
pass 'compact Copilot plugin output resolves ownership through bounded CLI-owned JSONC metadata'

for problem in marker source disabled symlink newer; do
    new_case "legacy-$problem" "legacy-$problem"
    create_legacy_bundle
    case $problem in
        marker) printf 'foreign marker\n' >"$legacy/.it-managed" ;;
        source) printf '/foreign/user/it-tmux-hooks' >"$TEST_STATE/copilot/legacy-installed" ;;
        disabled) : >"$TEST_STATE/copilot/legacy-disabled" ;;
        symlink)
            cp "$legacy/scripts/it-agent-hook.sh" "$work/legacy-external.sh"
            rm "$legacy/scripts/it-agent-hook.sh"
            ln -s "$work/legacy-external.sh" "$legacy/scripts/it-agent-hook.sh" ;;
        newer) printf '# IT_AGENT_HOOK/3\n' >>"$legacy/scripts/it-agent-hook.sh" ;;
    esac
    expect_success --allowed-clis copilot
    [[ -f $TEST_STATE/copilot/legacy-installed && ! -f $TEST_STATE/copilot/installed ]] ||
        fail 'unverified or disabled legacy registration was changed'
    if grep -q '^copilot plugin uninstall\|^copilot plugin install ' "$TEST_STATE/calls"; then
        fail 'legacy conflict caused a registration mutation'
    fi
    expected_result=legacy-conflict
    [[ $problem != disabled ]] || expected_result=user-disabled
    grep -q "provider=copilot result=$expected_result" "$work/install.stderr" || fail 'legacy conflict was not reported'
    [[ $(tmux -N -S "$TEST_LOGIN_HOME/.intelligent-terminal/run/tmux-hooks.sock" show-options -gqv @it-ssh-hooks-installed-clis) == - ]] ||
        fail 'v2-only or conflicting registration claimed v3 installed'
    [[ $(tmux -N -S "$TEST_LOGIN_HOME/.intelligent-terminal/run/tmux-hooks.sock" show-options -gqv @it-ssh-hooks-unavailable-clis) == copilot ]] ||
        fail 'legacy conflict was not visible in finite readiness metadata'
done
pass 'legacy name alone never grants ownership; conflicts and user disablement block duplication'

for interruption in uninstall install retained; do
    new_case "lm-$interruption" "lm-$interruption"
    create_legacy_bundle
    case $interruption in
        uninstall) export TEST_FAIL_LEGACY_UNINSTALL_ONCE=1 ;;
        install) export TEST_FAIL_ONCE=copilot ;;
        retained) export TEST_KEEP_LEGACY=1 ;;
    esac
    if run_install --allowed-clis copilot; then fail 'incomplete legacy migration reported success'; fi
    hooks="$TEST_LOGIN_HOME/.intelligent-terminal/ssh-hooks"
    [[ -f $hooks/receipts/copilot.legacy.pending && -f $hooks/receipts/copilot.pending ]] ||
        fail 'legacy migration ownership journal was lost'
    [[ ! -f $TEST_STATE/copilot/duplicate-at-install ]] || fail 'failed legacy removal allowed duplicate install'
    if [[ $interruption == retained && -f $TEST_STATE/copilot/installed ]]; then
        fail 'new registration installed while old registration remained'
    fi
    unset TEST_FAIL_LEGACY_UNINSTALL_ONCE TEST_FAIL_ONCE TEST_KEEP_LEGACY
    expect_success --allowed-clis copilot
    [[ ! -f $TEST_STATE/copilot/legacy-installed && -f $TEST_STATE/copilot/installed ]] ||
        fail 'legacy migration retry did not complete'
    [[ ! -e $hooks/receipts/copilot.legacy.pending ]] || fail 'completed migration left pending ownership'
done
pass 'legacy uninstall/install interruptions repair safely and verify old registration disappearance'

new_case selected selected
rm "$bin/claude"
cp "$work/fake-provider" "$bin/snap"
ln -s "$bin/snap" "$bin/claude"
expect_success --allowed-clis copilot
[[ -f $TEST_STATE/copilot/installed && ! -d $TEST_STATE/claude ]] || fail 'agent allowlist was ignored'
[[ ! -e $TEST_STATE/snap-probe ]] || fail 'blocked CLI runtime was probed'
[[ $(grep -c 'result=not-selected' "$work/install.stderr") == 4 ]] || fail 'unselected providers not reported'
rm "$bin/claude" "$bin/snap"
ln -s "$work/fake-provider" "$bin/claude"
printf '\n# selection upgrade fixture\n' >>"$work/upload-hook.sh"
expect_success --allowed-clis claude
[[ -f $TEST_STATE/claude/installed ]] || fail 'newly selected provider was not installed'
before_updates=$(grep -c '^copilot plugin update ' "$TEST_STATE/calls" || :)
expect_success --allowed-clis copilot
after_updates=$(grep -c '^copilot plugin update ' "$TEST_STATE/calls" || :)
[[ $after_updates -gt $before_updates ]] || fail 'provider skipped during an upgrade was not updated later'
if run_install --allowed-clis 'copilot,unknown'; then fail 'unknown allowlisted agent accepted'; fi
grep -q invalid-agent-selection "$work/install.stderr" || fail 'invalid selection diagnostic missing'
if run_install --allowed-clis 'copilot,copilot'; then fail 'duplicate allowlisted agent accepted'; fi
grep -q invalid-agent-selection "$work/install.stderr" || fail 'duplicate selection diagnostic missing'
new_case empty-selection empty-selection
expect_success --allowed-clis ''
[[ ! -e $TEST_LOGIN_HOME/.intelligent-terminal ]] || fail 'empty agent selection changed the namespace'
env PATH="$bin" HOME="$TEST_RUNTIME_HOME" /bin/sh "$work/install-remote-hooks.sh" \
    --hook-source "$work/upload-hook.sh" --login-home "$TEST_LOGIN_HOME" >"$work/install.stdout" 2>"$work/install.stderr"
[[ ! -e $TEST_LOGIN_HOME/.intelligent-terminal ]] || fail 'missing allowlist changed the namespace'
pass 'agent selection is enforced and per-provider generations repair deferred upgrades'

new_case concurrent concurrent
run_install &
first=$!
run_install &
second=$!
wait "$first" || fail 'first concurrent installer failed'
wait "$second" || fail 'second concurrent installer failed'
[[ $(grep -c '^copilot plugin install ' "$TEST_STATE/calls") == 1 ]] || fail 'install lock did not serialize'
pass 'two installers share a process-safe install lock'

new_case snap snap
rm "$bin/copilot"
cp "$work/fake-provider" "$bin/snap"
ln -s "$bin/snap" "$bin/copilot"
export TEST_SNAP_MODE=unavailable
expect_success
grep -q 'provider=copilot result=unsupported-runtime' "$work/install.stderr" || fail 'confined runtime was assumed capable'
[[ -f $TEST_STATE/snap-probe && ! -e $TEST_STATE/copilot/installed ]] || fail 'Snap was not actually probed'
export TEST_SNAP_MODE=available
expect_success
grep -q 'provider=copilot result=installed' "$work/install.stderr" || fail 'accessible Snap runtime rejected by package type'
unset TEST_SNAP_MODE
rm "$bin/copilot" "$bin/snap"
ln -s "$work/fake-provider" "$bin/copilot"
pass 'Snap capability is probed in its namespace rather than inferred from packaging'

new_case no-cli no-cli
for provider in claude copilot codex gemini opencode; do mv "$bin/$provider" "$bin/$provider.off"; done
expect_success
[[ $(grep -c 'result=not-found' "$work/install.stderr") == 5 ]] || fail 'no-CLI results missing'
[[ $(tmux -N -S "$TEST_LOGIN_HOME/.intelligent-terminal/run/tmux-hooks.sock" show-options -gqv @it-hook-channel) == '3 ready -' ]] ||
    fail 'empty installed-provider readiness snapshot is ambiguous'
[[ $(tmux -N -S "$TEST_LOGIN_HOME/.intelligent-terminal/run/tmux-hooks.sock" show-options -gqv @it-ssh-hooks-installed-clis) == - ]] ||
    fail 'missing empty installed-provider status'
[[ $(tmux -N -S "$TEST_LOGIN_HOME/.intelligent-terminal/run/tmux-hooks.sock" show-options -gqv @it-ssh-hooks-unavailable-clis) == - ]] ||
    fail 'absent providers must not be reported as detected-unavailable'
for provider in claude copilot codex gemini opencode; do mv "$bin/$provider.off" "$bin/$provider"; done
pass 'no CLI is reported explicitly without installing an agent or package'

new_case normal normal
mkdir -p "$TEST_LOGIN_HOME/.intelligent-terminal/run"
normal_socket=${sockets[${#sockets[@]}-1]}
tmux -S "$normal_socket" -f /dev/null new-session -d -s user-session cat
if run_install; then fail 'foreign tmux server accepted'; fi
tmux -N -S "$normal_socket" has-session -t '=user-session' || fail 'user tmux session was changed'
grep -q 'foreign-socket' "$work/install.stderr" || fail 'foreign socket diagnostic missing'
pass 'a same-path foreign tmux server is neither reused nor killed'

new_case namespace namespace
if run_install --socket "$work/other.sock"; then fail 'different socket namespace accepted'; fi
grep -q socket-namespace-mismatch "$work/install.stderr" || fail 'socket namespace diagnostic missing'
long_name=$(printf '%080d' 0)
new_case "$long_name" long
if run_install; then fail 'oversized UNIX socket path accepted'; fi
grep -q socket-path-too-long "$work/install.stderr" || fail 'socket length diagnostic missing'
pass 'socket namespace and UNIX path length never fall back to another location'

TEST_LOGIN_HOME=$home_fresh
TEST_STATE=$state_fresh
export TEST_LOGIN_HOME TEST_STATE
before=$(readlink "$TEST_LOGIN_HOME/.intelligent-terminal/ssh-hooks/current")
WTA_TMUX_HOOKS_DISABLED=1 expect_success
[[ $(readlink "$TEST_LOGIN_HOME/.intelligent-terminal/ssh-hooks/current") == "$before" ]] ||
    fail 'opt-out mutated installation'
grep -q 'it-remote-hooks: disabled' "$work/install.stderr" || fail 'opt-out result missing'
printf 'All managed installer tests passed.\n'
