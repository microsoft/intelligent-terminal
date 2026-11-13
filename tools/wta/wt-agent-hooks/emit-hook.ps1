# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

param(
    [Parameter(Mandatory = $true)][Alias('cli-source')][string]$CliSource,
    [Parameter(Mandatory = $true)][string]$Event
)

$ErrorActionPreference = 'Stop'

function Write-HookFailure([string]$Operation, [System.Exception]$Exception) {
    if (-not $env:WTA_HOOK_LOG_DIR) { return }
    try {
        [IO.Directory]::CreateDirectory($env:WTA_HOOK_LOG_DIR) | Out-Null
        [IO.File]::AppendAllText(
            (Join-Path $env:WTA_HOOK_LOG_DIR 'hook-trace.log'),
            ('{0:o} {1} {2} failed event={3} error={4} hresult={5}' -f
                [DateTime]::UtcNow, $CliSource, $Operation, $Event, $Exception.GetType().Name, $Exception.HResult) + "`n")
    }
    catch {
        # A diagnostic failure must not block the provider's hook.
    }
}

if (-not $env:WT_SESSION) { return }

try {
    $inputStream = [Console]::OpenStandardInput()
    $buffer = New-Object byte[] (1024 * 1024 + 1)
    $count = 0
    do {
        $read = $inputStream.Read($buffer, $count, $buffer.Length - $count)
        $count += $read
    } while ($read -gt 0 -and $count -lt $buffer.Length)
    if ($count -eq $buffer.Length) {
        throw [IO.InvalidDataException]::new('Hook input exceeds the size limit.')
    }
    $inputJson = [Text.UTF8Encoding]::new($false, $true).GetString($buffer, 0, $count)

    if ($env:INTELLIGENT_TERMINAL -eq '1') {
        [string]$sessionId = switch ($CliSource) {
            copilot { $env:COPILOT_SESSION_ID }
            claude { $env:CLAUDE_SESSION_ID }
            codex { $env:CODEX_SESSION_ID }
            gemini { $env:GEMINI_SESSION_ID }
        }

        $params = @{
            event = $Event
            cli_source = $CliSource
        }
        # Windows PowerShell can serialize an absent switch result as {}.
        if (-not [string]::IsNullOrEmpty($sessionId)) {
            $params.agent_session_id = $sessionId
        }
        if ($CliSource -eq 'antigravity' -and $env:WTA_HOOK_CWD -like '/*' -and $env:WSL_DISTRO_NAME) {
            $params.wsl_distro = $env:WSL_DISTRO_NAME
        }
        $json = ConvertTo-Json -InputObject $params -Compress
        $body = if ([string]::IsNullOrWhiteSpace($inputJson)) { 'null' } else { $inputJson }
        $json = $json.Substring(0, $json.Length - 1) + ',"payload":' + $body + '}'
        $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($json))
        $sequence = ([char]27) + ']9001;AgentEvent64;' + $encoded + ([char]7)
        try {
            if (-not ('AgentHookConsole' -as [type])) {
                Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class AgentHookConsole {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern SafeFileHandle CreateFile(string name, uint access, uint share, IntPtr security, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetConsoleMode(SafeFileHandle handle, out uint mode);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetConsoleMode(SafeFileHandle handle, uint mode);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool WriteConsole(SafeFileHandle handle, string text, uint count, out uint written, IntPtr reserved);
    public static bool Write(string text) {
        using (var handle = CreateFile("CONOUT$", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero)) {
            uint mode;
            if (handle.IsInvalid || !GetConsoleMode(handle, out mode)) return false;
            if ((mode & 4) == 0 && !SetConsoleMode(handle, mode | 4)) return false;
            uint written;
            if (!WriteConsole(handle, text, (uint)text.Length, out written, IntPtr.Zero))
                throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
            if (written != text.Length) throw new System.IO.IOException("Incomplete hook console write.");
            return true;
        }
    }
}
'@
            }
            if ([AgentHookConsole]::Write($sequence)) { return }
        }
        catch {
            Write-HookFailure 'VT delivery' $_.Exception
        }
    }

    if ($env:WT_COM_CLSID) {
        $bridge = if ($env:WTCLI_PATH) { $env:WTCLI_PATH } else { 'wtcli.exe' }
        $utf8 = [Text.UTF8Encoding]::new($false)
        $OutputEncoding = $utf8
        $inputJson | & $bridge agent-hook --cli-source $CliSource --event $Event 2>$null
    }
}
catch {
    Write-HookFailure 'hook delivery' $_.Exception
}
return
