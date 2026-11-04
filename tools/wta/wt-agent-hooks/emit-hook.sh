#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

cli=$1
event=$2
export LC_ALL=C
[ -n "${WT_SESSION:-}" ] || exit 0

# Git Bash does not necessarily have a POSIX controlling tty. Use the owning
# Windows console instead; the PowerShell writer also preserves COM fallback.
platform=$(uname -s)
if [ "$platform" != Linux ] && command -v powershell.exe >/dev/null 2>&1; then
    exec powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass \
        -File "$WT_AGENT_HOOK_DIR/emit-hook.ps1" -CliSource "$cli" -Event "$event"
fi

payload=$(head -c 1048577)
[ "${#payload}" -le 1048576 ] || exit 0

if [ "${INTELLIGENT_TERMINAL:-}" = 1 ] &&
    { exec 9>/dev/tty; } 2>/dev/null; then
    # Base64 keeps arbitrary hook text from terminating or injecting an OSC.
    distro=
    if [ "$platform" = Linux ] && [[ "${WSL_DISTRO_NAME:-}" =~ ^[A-Za-z0-9._+-]{1,256}$ ]]; then
        distro=',"wsl_distro":"'"$WSL_DISTRO_NAME"'"'
    fi
    encoded=$(printf '{"event":"%s","cli_source":"%s"%s,"payload":%s}' \
        "$event" "$cli" "$distro" "${payload:-null}" | base64 | tr -d '\r\n')
    printf '\033]9001;AgentEvent64;%s\007' "$encoded" >&9
    exec 9>&-
elif command -v wtcli.exe >/dev/null 2>&1; then
    printf '%s' "$payload" | wtcli.exe agent-hook --cli-source "$cli" --event "$event"
fi
exit 0
