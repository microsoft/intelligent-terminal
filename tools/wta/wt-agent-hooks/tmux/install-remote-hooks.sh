#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

set -eu
LC_ALL=C
export LC_ALL
umask 077
version=3.0.0
owner='Intelligent Terminal managed SSH hooks v1'
plugin=it-ssh-hooks
market=it-ssh-local
hook_source=
login_home=
socket=
session=it-hooks
attach=
agents=
log() { printf '%s\n' "it-remote-hooks: $*" >&2; }
fatal_reported=
die() { fatal_reported=1; log "$1"; exit 1; }
while [ "$#" -gt 0 ]; do
    case $1 in
        --attach-control) attach=1; shift ;;
        --hook-source|--login-home|--socket|--session|--agents|--allowed-clis)
            [ "$#" -ge 2 ] || die invalid-arguments
            case $1 in
                --hook-source) hook_source=$2 ;;
                --login-home) login_home=$2 ;;
                --socket) socket=$2 ;;
                --session) session=$2 ;;
                --agents|--allowed-clis) agents=$2 ;;
            esac
            shift 2 ;;
        *) die invalid-arguments ;;
    esac
done
[ -z "${WTA_TMUX_HOOKS_DISABLED:-}" ] || { log disabled; exit 0; }
case $agents in
    '') log no-agents-selected; exit 0 ;;
    ,*|*,|*,,*|*[!a-z,]*) die invalid-agent-selection ;;
esac
previous_ifs=$IFS
IFS=,
set -- $agents
IFS=$previous_ifs
selected_once=,
for selected in "$@"; do
    case $selected in claude|copilot|codex|gemini|opencode) ;; *) die invalid-agent-selection ;; esac
    case $selected_once in *,"$selected",*) die invalid-agent-selection ;; esac
    selected_once=$selected_once$selected,
done
unset IT_SSH_HOOK_ROUTE IT_SSH_HOOK_SOCKET IT_SSH_HOOK_SESSION
for utility in sh tmux timeout mktemp rm rmdir head wc base64 stat id getent flock \
    sha256sum awk sed readlink mkdir mv ln cp find sort cat chmod dirname; do
    command -v "$utility" >/dev/null 2>&1 || die "required-utility-unavailable:$utility"
done
[ "$session" = it-hooks ] || die invalid-session
[ -n "$hook_source" ] && [ -f "$hook_source" ] && [ ! -L "$hook_source" ] || die invalid-hook-source
[ "$(wc -c <"$hook_source")" -le 1048576 ] || die oversized-hook-source
sh -n "$hook_source" 2>/dev/null || die invalid-hook-source
tmux_version=$(timeout --kill-after=1 5 tmux -V 2>/dev/null) || die tmux-version-query-failed
case $tmux_version in 'tmux '*) tmux_version=${tmux_version#tmux } ;; *) die unsupported-tmux-version ;; esac
major=${tmux_version%%.*}
minor=${tmux_version#*.}
minor=${minor%%[!0-9]*}
case $major:$minor in *[!0-9:]*|:*|*:) die unsupported-tmux-version ;; esac
[ "${#major}" -le 6 ] && [ "${#minor}" -le 6 ] || die unsupported-tmux-version
[ "$major" -gt 3 ] || { [ "$major" -eq 3 ] && [ "$minor" -ge 4 ]; } || die unsupported-tmux-version
uid=$(id -u)
record=$(timeout --kill-after=1 5 getent passwd "$uid") || die login-identity-unavailable
actual_home=$(printf '%s\n' "$record" | awk -F: 'NF == 7 { print $6 }')
[ -n "$actual_home" ] || die login-identity-unavailable
[ -n "$login_home" ] || login_home=$actual_home
[ "$login_home" = "$actual_home" ] || die login-home-mismatch
case $login_home in /*) ;; *) die invalid-login-home ;; esac
case $login_home in *'
'*|*'	'*) die invalid-login-home ;; esac
[ "$(readlink -f -- "$login_home")" = "$login_home" ] || die noncanonical-login-home

secure_directory() {
    [ -d "$1" ] && [ ! -L "$1" ] || die unsafe-managed-directory
    [ "$(stat -c %u -- "$1")" = "$uid" ] || die managed-directory-owner-mismatch
    permissions=$(stat -c %a -- "$1")
    [ "$((0$permissions & 0022))" -eq 0 ] || die writable-managed-directory
}
secure_directory "$login_home"
root=$login_home/.intelligent-terminal
hooks=$root/ssh-hooks
run=$root/run
for directory in "$root" "$run"; do
    mkdir -p -- "$directory"
    secure_directory "$directory"
done
[ -n "$socket" ] || socket=$run/tmux-hooks.sock
[ "$socket" = "$run/tmux-hooks.sock" ] || die socket-namespace-mismatch
[ "${#socket}" -le 107 ] || die socket-path-too-long
[ ! -L "$socket" ] || die unsafe-socket
[ ! -L "$run/tmux-hooks.install.lock" ] || die unsafe-install-lock
if [ -e "$run/tmux-hooks.install.lock" ]; then
    [ -f "$run/tmux-hooks.install.lock" ] &&
        [ "$(stat -c %u "$run/tmux-hooks.install.lock")" = "$uid" ] || die unsafe-install-lock
fi
exec 9>>"$run/tmux-hooks.install.lock"
flock -w 5 9 || die install-lock-timeout
if [ -e "$hooks" ]; then
    secure_directory "$hooks"
    [ -f "$hooks/owner" ] && [ ! -L "$hooks/owner" ] &&
        [ "$(cat "$hooks/owner")" = "$owner" ] || die foreign-managed-directory
else
    mkdir -- "$hooks"
    printf '%s\n' "$owner" >"$hooks/owner"
fi

work=$(mktemp -d "$hooks/.work.XXXXXXXXXXXX") || die staging-unavailable
readonly work
phase=transport
cleanup() {
    cleanup_status=$?
    if [ "$cleanup_status" -ne 0 ] && [ -z "$fatal_reported" ]; then
        log "setup-command-failed:$phase"
    fi
    rm -rf -- "$work" || log staging-cleanup-failed
    return "$cleanup_status"
}
trap cleanup 0
trap 'exit 1' HUP INT TERM
for directory in releases receipts; do
    [ -e "$hooks/$directory" ] || mkdir -- "$hooks/$directory"
    secure_directory "$hooks/$directory"
done
tm() { timeout --kill-after=1 5 tmux -N -S "$socket" "$@" < /dev/null 9>&- 2>"$work/tmux-error"; }

if [ -e "$socket" ]; then
    [ -S "$socket" ] && [ "$(stat -c %u -- "$socket")" = "$uid" ] || die socket-owner-mismatch
fi
if [ -f "$run/tmux-hooks.owner" ]; then
    [ ! -L "$run/tmux-hooks.owner" ] &&
        [ "$(cat "$run/tmux-hooks.owner")" = "$owner:$uid" ] || die foreign-socket-marker
elif [ -e "$socket" ]; then
    die foreign-socket
else
    printf '%s\n' "$owner:$uid" >"$work/socket-owner"
    mv -T -- "$work/socket-owner" "$run/tmux-hooks.owner"
fi
if tm list-sessions >"$work/sessions"; then
    protocol=$(tm show-options -gqv @it-ssh-hooks-protocol) || die socket-protocol-query-failed
    if [ "$protocol" != 3 ]; then
        # The session environment is committed by new-session itself, allowing
        # recovery if the creating client exits before its following set-option.
        reservation=$(tm show-environment -t '=it-hooks' IT_SSH_HOOK_OWNER) || die foreign-server
        [ "$reservation" = "IT_SSH_HOOK_OWNER=$uid" ] || die foreign-server
        tm set-option -g @it-ssh-hooks-protocol 3 || die server-protocol-update-failed
    fi
else
    error=$(cat "$work/tmux-error")
    case $error in
        *"no server running"*|*"No such file or directory"*|*"Connection refused"*) ;;
        *) die socket-connect-failed ;;
    esac
    timeout --kill-after=1 5 tmux -S "$socket" -f /dev/null \
        new-session -d -s it-hooks -e "IT_SSH_HOOK_OWNER=$uid" 'exec /bin/sh -c "read -r ignored"' \
        \; set-option -g @it-ssh-hooks-protocol 3 \
        \; set-option -g exit-unattached off < /dev/null 9>&- >"$work/create-output" 2>"$work/tmux-error" ||
        die dedicated-server-create-failed
fi
tm has-session -t '=it-hooks' || die dedicated-session-missing
server_session=$(tm list-sessions -F '#{session_id} #{==:#{session_name},it-hooks}' |
    awk '$2 == 1 { print $1 }')
[ -n "$server_session" ] || die dedicated-session-missing
tm set-option -g exit-unattached off || die server-lifetime-update-failed
tm set-option -t "$server_session" destroy-unattached off || die session-lifetime-update-failed
server_pid=$(tm display-message -p '#{pid}') || die server-identity-query-failed

json_string() {
    awk 'BEGIN { printf "\"" } {
        if (NR > 1) printf "\\n"
        for (i=1;i<=length($0);i++) {
            c=substr($0,i,1)
            if (c=="\\" || c=="\"") printf "\\%s",c
            else if (c=="\t") printf "\\t"
            else if (c=="\r") printf "\\r"
            else printf "%s",c
        }
    } END { printf "\"" }'
}
shell_quote() { printf "'"; printf '%s' "$1" | sed "s/'/'\\\\''/g"; printf "'"; }

generate_provider() (
    provider=$1
    directory=$2/$provider
    mkdir -p -- "$directory/$plugin/hooks"
    command_path=$(shell_quote "$hooks/current/it-agent-hook.sh")
    printf '{"hooks":{' >"$directory/$plugin/hooks/hooks.json"
    separator=
    case $provider in
        claude|copilot)
            catalog='SessionStart:agent.session.start SessionEnd:agent.session.end Notification:agent.notification UserPromptSubmit:agent.prompt.submit StopFailure:agent.error Stop:agent.stop' ;;
        codex)
            catalog='SessionStart:agent.session.start PermissionRequest:agent.notification UserPromptSubmit:agent.prompt.submit Stop:agent.stop' ;;
        gemini)
            catalog='SessionStart:agent.session.start SessionEnd:agent.session.end BeforeAgent:agent.prompt.submit BeforeTool:agent.tool.starting Notification:agent.notification AfterAgent:agent.stop' ;;
    esac
    for pair in $catalog; do
        native=${pair%%:*}
        event=${pair#*:}
        command=$(printf '%s' "sh $command_path --cli-source $provider --event $event; exit 0" | json_string)
        key=command
        extra=
        [ "$provider" != copilot ] || { key=bash; extra=',"timeoutSec":5'; }
        [ "$provider" != claude ] || extra=',"shell":"bash"'
        matcher='"matcher":".*",'
        if [ "$provider" = codex ]; then
            matcher=
            [ "$native" != SessionStart ] || matcher='"matcher":"startup|resume",'
        fi
        printf '%s"%s":[{%s"hooks":[{"type":"command","%s":%s%s}]}]' \
            "$separator" "$native" "$matcher" "$key" "$command" "$extra" >>"$directory/$plugin/hooks/hooks.json"
        separator=,
    done
    printf '}}\n' >>"$directory/$plugin/hooks/hooks.json"
    case $provider in
        claude) manifest=.claude-plugin/plugin.json; marketplace=.claude-plugin/marketplace.json ;;
        copilot) manifest=plugin.json; marketplace=.github/plugin/marketplace.json ;;
        codex) manifest=.codex-plugin/plugin.json; marketplace=.agents/plugins/marketplace.json ;;
        gemini) manifest=gemini-extension.json; marketplace= ;;
    esac
    mkdir -p -- "$(dirname "$directory/$plugin/$manifest")"
    printf '{"name":"%s","version":"%s","description":"Managed Intelligent Terminal SSH hooks"' "$plugin" "$version" \
        >"$directory/$plugin/$manifest"
    [ "$provider" != copilot ] || printf ',"hooks":"hooks/hooks.json"' >>"$directory/$plugin/$manifest"
    if [ "$provider" = gemini ]; then
        printf ',"settings":[' >>"$directory/$plugin/$manifest"
        separator=
        for variable in IT_SSH_HOOK_ROUTE IT_SSH_HOOK_SOCKET IT_SSH_HOOK_SESSION WTA_TMUX_HOOKS_DISABLED; do
            printf '%s{"name":"%s","envVar":"%s","sensitive":false}' "$separator" "$variable" "$variable" \
                >>"$directory/$plugin/$manifest"
            separator=,
        done
        printf ']' >>"$directory/$plugin/$manifest"
    fi
    printf '}\n' >>"$directory/$plugin/$manifest"
    if [ -n "$marketplace" ]; then
        mkdir -p -- "$(dirname "$directory/$marketplace")"
        if [ "$provider" = codex ]; then
            printf '{"name":"%s","interface":{"displayName":"Intelligent Terminal SSH"},"plugins":[{"name":"%s","source":{"source":"local","path":"./%s"},"policy":{"installation":"AVAILABLE","authentication":"ON_INSTALL"},"category":"Productivity"}]}\n' "$market" "$plugin" "$plugin" >"$directory/$marketplace"
        else
            printf '{"name":"%s","owner":{"name":"Intelligent Terminal"},"plugins":[{"name":"%s","version":"%s","source":"./%s"}]}\n' "$market" "$plugin" "$version" "$plugin" >"$directory/$marketplace"
        fi
    fi
)

# Parse only CLI management responses, never agent hook payloads. Unknown or
# ambiguous schemas fail closed rather than guessing ownership or enablement.
json_status() {
    IT_QUERY_NAME=$1 IT_QUERY_ROOT=$2 IT_QUERY_MODE=$3 IT_QUERY_MARKET=${4:-0} \
    IT_QUERY_EXACT=${5:-0} \
    IT_QUERY_JSONC=${6:-0} \
    IT_QUERY_RELEASES="$hooks/releases/" IT_QUERY_PROVIDER="$provider" IT_QUERY_VERSION="${verify_version:-}" awk '
    function bad() { failed=1; exit 2 }
    function space( closing) {
        while(pos<=length(text)) {
            if(substr(text,pos,1) ~ /[ \t\r\n]/) pos++
            else if(ENVIRON["IT_QUERY_JSONC"]=="1" && substr(text,pos,2)=="//") {
                while(pos<=length(text) && substr(text,pos,1)!="\n") pos++
            } else if(ENVIRON["IT_QUERY_JSONC"]=="1" && substr(text,pos,2)=="/*") {
                closing=index(substr(text,pos+2),"*/")
                if(!closing) bad()
                pos+=closing+3
            } else break
        }
    }
    function hex4( h,n,j) {
        h=substr(text,pos,4); pos+=4; n=0
        if(length(h)!=4 || h ~ /[^0-9a-fA-F]/) bad()
        for(j=1;j<=4;j++) n=n*16+index("0123456789abcdef",tolower(substr(h,j,1)))-1
        return n
    }
    function utf8(n) {
        if(n<128) return sprintf("%c",n)
        if(n<2048) return sprintf("%c%c",192+int(n/64),128+n%64)
        if(n<65536) return sprintf("%c%c%c",224+int(n/4096),128+int(n/64)%64,128+n%64)
        return sprintf("%c%c%c%c",240+int(n/262144),128+int(n/4096)%64,128+int(n/64)%64,128+n%64)
    }
    function string( out,c,e,n,low) {
        if (substr(text,pos++,1)!="\"") bad()
        out=""
        while (pos<=length(text)) {
            c=substr(text,pos++,1)
            if (c=="\"") return out
            if (c=="\\") {
                e=substr(text,pos++,1)
                if (e=="u") {
                    n=hex4()
                    if(n>=55296 && n<=56319) {
                        if(substr(text,pos,2)!="\\u") bad()
                        pos+=2; low=hex4()
                        if(low<56320 || low>57343) bad()
                        n=65536+(n-55296)*1024+low-56320
                    } else if(n>=56320 && n<=57343) bad()
                    c=utf8(n)
                } else if(e=="\"" || e=="\\" || e=="/") c=e
                else if(e=="n") c="\n"
                else if(e=="r") c="\r"
                else if(e=="t") c="\t"
                else if(e=="b" || e=="f") c="?"
                else bad()
            } else if(c ~ /[[:cntrl:]]/) bad()
            out=out c
        }
        bad()
    }
    function value(depth, key, c,k,s,d,identity,enabled,trusted,rest,parts,ours,keys) {
        if(depth>32) bad()
        space(); c=substr(text,pos,1)
        if(c=="{") {
            pos++; space()
            names[depth]=""; ids[depth]=""; states[depth]=""; paths[depth]=0; versions[depth]=""
            if(substr(text,pos,1)!="}") while(1) {
                k=string(); space()
                if(k in keys) bad()
                keys[k]=1
                if(substr(text,pos++,1)!=":") bad()
                value(depth+1,k); space(); c=substr(text,pos++,1)
                if(c=="}") break
                if(c!=",") bad()
                space()
                if(ENVIRON["IT_QUERY_JSONC"]=="1" && substr(text,pos,1)=="}") { pos++; break }
            } else pos++
            identity=names[depth]; if(identity=="") identity=ids[depth]
            if(identity==wanted || (mode=="plugin" && index(identity,wanted "@")==1)) {
                matches++; enabled=states[depth]; trusted=paths[depth]
                if(mode=="manifest") result=versions[depth]==required_version ? "valid" : "stale"
                else if(mode=="market") result=trusted ? "enabled" : "foreign"
                else if(enabled=="false") result="disabled"
                else if(enabled!="true") result="unknown"
                else if(exact=="1" && identity!=wanted) result="foreign"
                else if(trusted || (market=="1" && identity==wanted "@it-ssh-local")) {
                    result=(required_version!="" && versions[depth]!=required_version) ? "stale" : "enabled"
                }
                else result="foreign"
            }
        } else if(c=="[") {
            pos++; space()
            if(substr(text,pos,1)!="]") while(1) {
                value(depth+1,""); space(); c=substr(text,pos++,1)
                if(c=="]") break
                if(c!=",") bad()
                space()
                if(ENVIRON["IT_QUERY_JSONC"]=="1" && substr(text,pos,1)=="]") { pos++; break }
            } else pos++
        } else if(c=="\"") {
            s=string()
            if(key=="name") names[depth-1]=s
            if(key=="id") ids[depth-1]=s
            if(key=="version") versions[depth-1]=s
            ours=(key=="source" || key=="installedFrom" || key=="path" || key=="installPath") &&
                (s==root || (exact!="1" && index(s,root "/")==1))
            if(exact!="1" && (key=="source" || key=="installedFrom" || key=="path" || key=="installPath") && index(s,releases)==1) {
                rest=substr(s,length(releases)+1); split(rest,parts,"/")
                if(length(parts[1])==64 && parts[1] !~ /[^0-9a-f]/ && parts[2]==provider) ours=1
            }
            if(ours) for(d=0;d<depth;d++) paths[d]=1
        } else {
            s=""
            while(pos<=length(text) && substr(text,pos,1) !~ /[ \t\r\n,\]}]/) s=s substr(text,pos++,1)
            if(s!="true" && s!="false" && s!="null" && s !~ /^-?(0|[1-9][0-9]*)([.][0-9]+)?([eE][+-]?[0-9]+)?$/) bad()
            if((provider=="gemini" && key=="isActive") || (provider!="gemini" && key=="enabled")) states[depth-1]=s
        }
    }
    { text=text $0 "\n"; if(length(text)>1048576) bad() }
    END {
        if(failed) exit 2
        wanted=ENVIRON["IT_QUERY_NAME"]; root=ENVIRON["IT_QUERY_ROOT"]
        mode=ENVIRON["IT_QUERY_MODE"]; market=ENVIRON["IT_QUERY_MARKET"]
        exact=ENVIRON["IT_QUERY_EXACT"]
        releases=ENVIRON["IT_QUERY_RELEASES"]; provider=ENVIRON["IT_QUERY_PROVIDER"]
        required_version=(mode=="plugin" || mode=="manifest") ? ENVIRON["IT_QUERY_VERSION"] : ""
        pos=1; value(0,""); space()
        if(pos<=length(text) || matches>1) exit 2
        print matches ? result : "absent"
    }' "$work/query"
}

phase=publish
hook_hash=$(sha256sum <"$hook_source")
installer_hash=$(sha256sum <"$0")
generation=$(printf '%s\n' "${hook_hash%% *}" "${installer_hash%% *}" "$version" "$hooks" | sha256sum)
generation=${generation%% *}
case $generation in ''|*[!0-9a-f]*) die invalid-generation ;; esac
release=$hooks/releases/$generation
if [ -e "$hooks/current" ] || [ -L "$hooks/current" ]; then
    [ -L "$hooks/current" ] || die foreign-current-path
    previous=$(readlink "$hooks/current")
    case $previous in releases/*) previous=${previous#releases/} ;; *) die foreign-current-path ;; esac
    case $previous in ''|*[!0-9a-f]*) die foreign-current-path ;; esac
    [ -f "$hooks/releases/$previous/manifest.sha256" ] || die missing-owned-manifest
    timeout --kill-after=1 5 sh -c 'cd "$1" && sha256sum -c manifest.sha256 --quiet' sh \
        "$hooks/releases/$previous" >"$work/verify-output" 2>&1 ||
        die modified-managed-files
fi
for metadata in "$hooks/manifest" "$hooks/pending"; do
    if [ -e "$metadata" ] || [ -L "$metadata" ]; then
        [ -f "$metadata" ] && [ ! -L "$metadata" ] &&
            [ "$(head -n 1 "$metadata")" = "$owner" ] || die foreign-install-metadata
    fi
done
if [ -e "$release" ]; then
    secure_directory "$release"
    timeout --kill-after=1 5 sh -c 'cd "$1" && sha256sum -c manifest.sha256 --quiet' sh \
        "$release" >"$work/verify-output" 2>&1 || die modified-managed-release
else
    mkdir "$work/release"
    cp -- "$hook_source" "$work/release/it-agent-hook.sh"
    chmod 700 "$work/release/it-agent-hook.sh"
    for provider in claude copilot codex gemini; do generate_provider "$provider" "$work/release"; done
    (cd "$work/release" && find . -type f ! -name manifest.sha256 -print | sort |
        while IFS= read -r file; do sha256sum "$file"; done) >"$work/release/manifest.sha256"
    mv -T -- "$work/release" "$release"
fi
printf '%s\n%s\n' "$owner" "$generation" >"$work/pending"
mv -T -- "$work/pending" "$hooks/pending"
ln -s "releases/$generation" "$work/current"
mv -Tf -- "$work/current" "$hooks/current"

probe='
    umask 077
    for utility in sh tmux timeout mktemp rm rmdir head wc base64 stat id; do
        command -v "$utility" >/dev/null 2>&1 || exit 1
    done
    test -r "$1" && test -S "$2" || exit 1
    socket_uid=$(stat -c %u -- "$2") || exit 1
    current_uid=$(id -u) || exit 1
    test "$socket_uid" = "$current_uid" || exit 1
    encoded=$(printf x | base64 --wrap=6000) || exit 1
    test "$encoded" = eA== || exit 1
    empty=$(timeout --kill-after=0.1 1 head -c 1 </dev/null) || exit 1
    test -z "$empty" || exit 1
    tmux -N -S "$2" has-session -t "=it-hooks" >/dev/null 2>&1 || exit 1
    connected_pid=$(tmux -N -S "$2" display-message -p "#{pid}") || exit 1
    test "$connected_pid" = "$3" || exit 1
    directory=$(mktemp -d "${TMPDIR:-/tmp}/it-ssh-probe.XXXXXXXXXXXX") || exit 1
    trap '\''rm -f -- "$directory/probe"; rmdir -- "$directory"'\'' 0
    trap '\''exit 1'\'' HUP INT TERM
    printf "%s\n" "display-message -p -l it-ssh-probe-ok" >"$directory/probe" || exit 1
    output=$(tmux -N -S "$2" source-file - <"$directory/probe") || exit 1
    test "$output" = it-ssh-probe-ok
'
probe_runtime() {
    resolved=$(readlink -f -- "$cli") || return 1
    case $cli:$resolved in
        /snap/bin/*:*|*:*/snap)
            app=${cli##*/}
            command -v snap >/dev/null 2>&1 || return 1
            timeout --kill-after=1 5 snap run --shell "$app" -c "$probe" sh \
                "$hooks/current/it-agent-hook.sh" "$socket" "$server_pid" </dev/null 9>&- >"$work/probe-output" 2>"$work/probe-error" ;;
        */flatpak/*|*/AppImage*|*/appimage*) return 1 ;;
        *)
            timeout --kill-after=1 5 sh -c "$probe" sh "$hooks/current/it-agent-hook.sh" "$socket" "$server_pid" \
                </dev/null 9>&- >"$work/probe-output" 2>"$work/probe-error" ;;
    esac
}
cli_run() { timeout --kill-after=1 8 "$cli" "$@" </dev/null 9>&- >"$work/query" 2>"$work/cli-error"; }
receipt_valid() (
    [ -f "$1" ] && [ ! -L "$1" ] || exit 1
    [ "$(cat "$1")" != "$owner" ] || exit 0
    [ "$(head -n 1 "$1")" = "$owner" ] || exit 1
    digest=$(sed -n '2p' "$1")
    [ "${#digest}" -eq 64 ] && [ "$(wc -l <"$1")" -eq 2 ] || exit 1
    case $digest in *[!0-9a-f]*) exit 1 ;; esac
)
receipt_generation() { if [ -f "$1" ]; then sed -n '2p' "$1"; fi; }
write_receipt() {
    printf '%s\n%s\n' "$owner" "$generation" >"$work/receipt"
    mv -T -- "$work/receipt" "$1"
}
provider_result() {
    printf '%s %s\n' "$provider" "$1" >>"$work/results"
    log "provider=$provider result=$1"
}
copilot_source_status() (
    wanted=$1
    expected=$2
    exact=$3
    status=$4
    [ "$provider" = copilot ] && [ "$status" = foreign ] || { printf '%s\n' "$status"; exit 0; }
    if [ -n "$verify_version" ] &&
        [ "$(json_status "$wanted" "$expected" manifest 0 "$exact")" != valid ]; then
        printf '%s\n' stale
        exit 0
    fi
    # Some Copilot versions report source="installed" without its origin.
    # Read only its registration metadata to establish ownership; CLI output
    # above remains authoritative for enablement. Never print configuration.
    configuration=${COPILOT_HOME:-$HOME/.copilot}/config.json
    if [ -f "$configuration" ] && [ ! -L "$configuration" ] &&
        [ "$(stat -c %u -- "$configuration")" = "$uid" ] &&
        [ "$(wc -c <"$configuration")" -le 1048576 ]; then
        cp -- "$configuration" "$work/query"
        registered=$(json_status "$wanted" "$expected" market 0 "$exact" 1) || exit 1
        if [ "$registered" = enabled ]; then printf '%s\n' enabled; exit 0; fi
    fi
    printf '%s\n' "$status"
)
query_plugin() {
    if [ "$provider" = gemini ]; then
        cli_run extensions list --output-format json || return 1
    else
        cli_run plugin list --json || return 1
    fi
    result=$(json_status "$plugin" "$hooks/current/$provider" plugin "${market_trusted:-0}") || return 1
    copilot_source_status "$plugin" "$hooks/current/$provider" 0 "$result"
}
query_legacy() (
    verify_version=2.0.0
    if [ "$provider" = gemini ]; then
        cli_run extensions list --output-format json || exit 1
    else
        cli_run plugin list --json || exit 1
    fi
    result=$(json_status it-tmux-hooks "$root/plugins/it-tmux-hooks" plugin 0 1) || exit 1
    copilot_source_status it-tmux-hooks "$root/plugins/it-tmux-hooks" 1 "$result"
)
legacy_bundle_owned() (
    legacy=$root/plugins/it-tmux-hooks
    for directory in "$root/plugins" "$legacy" "$legacy/scripts"; do
        [ -d "$directory" ] && [ ! -L "$directory" ] &&
            [ "$(stat -c %u -- "$directory")" = "$uid" ] || exit 1
        permissions=$(stat -c %a -- "$directory")
        [ "$((0$permissions & 0022))" -eq 0 ] || exit 1
    done
    for file in "$legacy/.it-managed" "$legacy/plugin.json" "$legacy/scripts/it-agent-hook.sh"; do
        [ -f "$file" ] && [ ! -L "$file" ] && [ "$(stat -c %u -- "$file")" = "$uid" ] || exit 1
        permissions=$(stat -c %a -- "$file")
        [ "$((0$permissions & 0022))" -eq 0 ] && [ "$(wc -c <"$file")" -le 1048576 ] || exit 1
    done
    [ "$(cat "$legacy/.it-managed")" = 'Intelligent Terminal managed remote hook bundle v2' ] || exit 1
    verify_version=2.0.0
    cp -- "$legacy/plugin.json" "$work/query"
    [ "$(json_status it-tmux-hooks "$legacy" manifest 0 1)" = valid ] || exit 1
    sh -n "$legacy/scripts/it-agent-hook.sh" 2>/dev/null || exit 1
    awk '
        index($0,"IT_AGENT_HOOK/2") { v2=1 }
        index($0,"IT_AGENT_HOOK/3") { v3=1 }
        END { exit !(v2 && !v3) }
    ' "$legacy/scripts/it-agent-hook.sh"
)

: >"$work/results"
failed=
phase=providers
for provider in claude copilot codex gemini opencode; do
    verify_version=
    case ,$agents, in *,"$provider",*) ;; *) provider_result not-selected; continue ;; esac
    cli=$(command -v "$provider" || :)
    if [ -z "$cli" ]; then provider_result not-found; continue; fi
    if [ "$provider" = opencode ]; then
        provider_result unsupported-registration-api
        continue
    fi
    if ! probe_runtime; then provider_result unsupported-runtime; continue; fi
    receipt=$hooks/receipts/$provider
    transaction=$hooks/receipts/$provider.pending
    legacy_transaction=$hooks/receipts/$provider.legacy.pending
    legacy_migrated=$hooks/receipts/$provider.legacy.migrated
    for state_file in "$receipt" "$transaction" "$legacy_transaction" "$legacy_migrated"; do
        if { [ -e "$state_file" ] || [ -L "$state_file" ]; } && ! receipt_valid "$state_file"; then die foreign-receipt; fi
    done
    market_trusted=0
    state=$(query_plugin) || { provider_result unsupported-status-api; continue; }
    legacy_state=$(query_legacy) || { provider_result unsupported-status-api; continue; }
    if [ "$legacy_state" != absent ]; then
        if [ "$legacy_state" = disabled ]; then provider_result user-disabled; continue; fi
        if [ "$provider" != copilot ] || [ "$legacy_state" != enabled ] ||
            receipt_valid "$legacy_migrated" || ! legacy_bundle_owned; then
            provider_result legacy-conflict
            continue
        fi
        case $state in
            absent)
                if receipt_valid "$receipt" && ! receipt_valid "$transaction"; then
                    provider_result user-removed; continue
                fi ;;
            enabled)
                if ! receipt_valid "$receipt" && ! receipt_valid "$transaction"; then
                    provider_result legacy-conflict; continue
                fi ;;
            *) provider_result legacy-conflict; continue ;;
        esac
        # Retire the verified old registration before installing the new name.
        # Journals preserve ownership across failures on either side of that gap.
        write_receipt "$legacy_transaction"
        [ "$state" != absent ] || write_receipt "$transaction"
        if ! cli_run plugin uninstall it-tmux-hooks; then
            provider_result legacy-migration-failed; failed=1; continue
        fi
        legacy_state=$(query_legacy) || { provider_result legacy-migration-failed; failed=1; continue; }
        [ "$legacy_state" = absent ] ||
            { provider_result legacy-migration-failed; failed=1; continue; }
        state=$(query_plugin) || { provider_result verification-failed; failed=1; continue; }
    fi
    case $state in
        disabled) provider_result user-disabled; continue ;;
        unknown) provider_result unsupported-status-schema; continue ;;
        enabled|foreign)
            if ! receipt_valid "$receipt" && ! receipt_valid "$transaction"; then
                provider_result foreign-plugin
                continue
            fi ;;
        absent)
            if receipt_valid "$receipt" && ! receipt_valid "$transaction"; then
                provider_result user-removed
                continue
            fi ;;
        *) provider_result unsupported-status-schema; continue ;;
    esac
    if [ "$provider" = claude ] || [ "$provider" = codex ]; then
        market_receipt=$hooks/receipts/$provider.market
        for state_file in "$market_receipt" "$market_receipt.pending"; do
            if { [ -e "$state_file" ] || [ -L "$state_file" ]; } && ! receipt_valid "$state_file"; then
                die foreign-marketplace-receipt
            fi
        done
        cli_run plugin marketplace list --json ||
            { provider_result unsupported-marketplace-api; continue; }
        market_state=$(json_status "$market" "$hooks/current/$provider" market) ||
            { provider_result unsupported-marketplace-schema; continue; }
        case $market_state in
            absent)
                write_receipt "$market_receipt.pending"
                if ! cli_run plugin marketplace add "$hooks/current/$provider"; then
                    provider_result marketplace-install-failed; failed=1; continue
                fi ;;
            enabled)
                if ! receipt_valid "$market_receipt" && ! receipt_valid "$market_receipt.pending"; then
                    provider_result foreign-marketplace; continue
                fi
                if [ "$(receipt_generation "$market_receipt")" != "$generation" ]; then
                    cli_run plugin marketplace update "$market" ||
                        { provider_result marketplace-update-failed; failed=1; continue; }
                fi ;;
            *) provider_result foreign-marketplace; continue ;;
        esac
        cli_run plugin marketplace list --json ||
            { provider_result marketplace-verification-failed; failed=1; continue; }
        [ "$(json_status "$market" "$hooks/current/$provider" market)" = enabled ] ||
            { provider_result marketplace-verification-failed; failed=1; continue; }
        write_receipt "$market_receipt"
        rm -f -- "$market_receipt.pending"
        market_trusted=1
        state=$(query_plugin) || { provider_result unsupported-status-api; continue; }
    fi
    case $state in
        disabled) provider_result user-disabled; continue ;;
        foreign|unknown) provider_result foreign-plugin; continue ;;
        absent)
            write_receipt "$transaction"
            if [ "$provider" = gemini ]; then
                cli_run extensions install "$hooks/current/gemini/$plugin" --consent --skip-settings ||
                    { provider_result install-failed; failed=1; continue; }
            elif [ "$provider" = copilot ]; then
                cli_run plugin install "$hooks/current/copilot/$plugin" ||
                    { provider_result install-failed; failed=1; continue; }
            else
                cli_run plugin install "$plugin@$market" ||
                    { provider_result install-failed; failed=1; continue; }
            fi ;;
        enabled)
            if [ "$(receipt_generation "$receipt")" != "$generation" ]; then
                write_receipt "$transaction"
                if [ "$provider" = gemini ]; then
                    cli_run extensions update "$plugin" ||
                        { provider_result update-failed; failed=1; continue; }
                else
                    specification=$plugin
                    [ "$provider" = copilot ] || specification=$plugin@$market
                    cli_run plugin update "$specification" ||
                        { provider_result update-failed; failed=1; continue; }
                fi
            fi ;;
        *) provider_result unsupported-status-schema; continue ;;
    esac
    verify_version=$version
    state=$(query_plugin) || { provider_result verification-failed; failed=1; continue; }
    [ "$state" = enabled ] || { provider_result verification-failed; failed=1; continue; }
    if receipt_valid "$legacy_transaction"; then
        legacy_state=$(query_legacy) || { provider_result legacy-migration-failed; failed=1; continue; }
        [ "$legacy_state" = absent ] ||
            { provider_result legacy-conflict; failed=1; continue; }
    fi
    write_receipt "$receipt"
    rm -f -- "$transaction"
    if receipt_valid "$legacy_transaction"; then
        write_receipt "$legacy_migrated"
        rm -f -- "$legacy_transaction"
        log "provider=$provider legacy-migration=complete"
    fi
    provider_result installed
done

phase=manifest
if [ -z "$failed" ]; then
    { printf '%s\n%s\nversion %s\n' "$owner" "$generation" "$version"; cat "$work/results"; } >"$work/manifest"
    mv -T -- "$work/manifest" "$hooks/manifest"
    rm -f -- "$hooks/pending"
else
    log partial-setup-retry-required
fi
setup=transport-ready
[ -z "$failed" ] || setup=transport-partial
tm set-option -g @it-ssh-hooks-setup "$setup" || die setup-status-update-failed
installed=
unavailable=
while IFS=' ' read -r result_provider result_state; do
    case $result_provider in claude|copilot|codex|gemini|opencode) ;; *) die invalid-provider-result ;; esac
    case $result_state in
        installed) installed=${installed:+$installed,}$result_provider ;;
        not-found|not-selected) ;;
        *) unavailable=${unavailable:+$unavailable,}$result_provider ;;
    esac
done <"$work/results"
[ -n "$installed" ] || installed=-
[ -n "$unavailable" ] || unavailable=-
channel_state=ready
[ -z "$failed" ] || channel_state=partial
tm set-option -g @it-ssh-hooks-installed-clis "$installed" \
    \; set-option -g @it-ssh-hooks-unavailable-clis "$unavailable" \
    \; set-option -g @it-hook-channel "3 $channel_state $installed" || die channel-status-update-failed
log "$setup; plugin installation is not proof of hook-runtime readiness"
cleanup
trap - 0 HUP INT TERM
flock -u 9
exec 9>&-
if [ -n "$attach" ]; then
    exec tmux -N -S "$socket" -C attach-session -t '=it-hooks'
fi
[ -z "$failed" ]
