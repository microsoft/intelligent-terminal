# Policy.ps1 — Group Policy (GPO) test control for the Intelligent Terminal agent features.
#
# The C++ policy reader (src/cascadia/inc/AgentPolicy.h) reads its values from
#   Software\Policies\Microsoft\IntelligentTerminal
# under HKLM FIRST, then HKCU as a fallback (AgentPolicy.h _ReadDwordPolicy / _ReadMultiSzPolicy
# iterate { HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER } and return on the first hit). That HKCU
# fallback lets the tests drive a real GPO via the CurrentUser policy hive — the exact same code
# path and value semantics as a machine GPO, and without touching HKLM. This mirrors how Fre.ps1
# drives the WinPS execution policy via HKCU.
#
# CAVEAT — the Software\Policies subtree is ACL-restricted to administrators, so a standard user
# cannot write it out of the box. Run test/e2e/tools/Enable-WtAgentPolicyTesting.ps1 ONCE (it
# self-elevates) to grant the current user write access to just this key; AFTER that the suite
# drives policy values NON-elevated and the terminal under test stays a normal user process.
# Test-WtAgentPolicyControllable probes writability (and any HKLM override) so the GPO suite skips
# cleanly on a machine that hasn't been provisioned.
#
# The snapshot is read at app startup (AgentPolicy::Reload), so ALWAYS set policy BEFORE
# launching the terminal (Start-Terminal cold-starts, so it picks up a freshly-written value).
#
# Registry values (AgentPolicy.h:69, 55-65, 122-129):
#   AllowedAgents          REG_MULTI_SZ  built-in agent-id allowlist (nullopt = all allowed)
#   AllowCustomAgents      REG_DWORD     0 = Blocked, non-zero = Allowed
#   AllowAutoFix           REG_DWORD     0 = Blocked, non-zero = Allowed
#   AllowAgentSessionHooks REG_DWORD     0 = Blocked, non-zero = Allowed

$script:WtAgentPolicyKey = 'HKCU:\SOFTWARE\Policies\Microsoft\IntelligentTerminal'

# value name -> registry type used by Set/Restore.
$script:WtAgentPolicyValues = [ordered]@{
    AllowedAgents          = 'MultiString'
    AllowCustomAgents      = 'DWord'
    AllowAutoFix           = 'DWord'
    AllowAgentSessionHooks = 'DWord'
}

function Get-WtAgentPolicyState {
    <# Snapshot the HKCU agent-policy key (whole key + each value) for restore. #>
    [CmdletBinding()] param()
    $keyExisted = Test-Path $script:WtAgentPolicyKey
    $values = [ordered]@{}
    foreach ($name in $script:WtAgentPolicyValues.Keys) {
        $v = if ($keyExisted) { (Get-ItemProperty -Path $script:WtAgentPolicyKey -Name $name -ErrorAction SilentlyContinue).$name } else { $null }
        $values[$name] = [pscustomobject]@{ HadValue = ($null -ne $v); Value = $v }
    }
    [pscustomobject]@{ Key = $script:WtAgentPolicyKey; KeyExisted = $keyExisted; Values = $values }
}

function Set-WtAgentPolicy {
    <#
    .SYNOPSIS
        Force IntelligentTerminal agent GPO values via the CurrentUser policy hive. Set BEFORE
        launching the terminal — the policy snapshot is read at startup. Returns the prior state
        object; pass it to Restore-WtAgentPolicy (ALWAYS restore in a finally). Requires the policy
        hive to be writable (see Enable-WtAgentPolicyTesting.ps1 — grant once, then non-elevated).
    .PARAMETER Policy
        Hashtable keyed by registry value name:
          AllowAutoFix / AllowCustomAgents / AllowAgentSessionHooks -> 'Allowed' | 'Blocked' | 0/1
          AllowedAgents -> string[] of built-in agent ids (e.g. @('copilot'))
        A $null value clears (removes) that policy value.
    .EXAMPLE
        $prior = Set-WtAgentPolicy -Policy @{ AllowAutoFix = 'Blocked' }
        try { ... } finally { Restore-WtAgentPolicy -State $prior }
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][hashtable]$Policy)
    $state = Get-WtAgentPolicyState
    if (-not (Test-Path $script:WtAgentPolicyKey)) { New-Item -Path $script:WtAgentPolicyKey -Force | Out-Null }
    foreach ($name in $Policy.Keys) {
        if (-not $script:WtAgentPolicyValues.Contains($name)) { throw "Unknown agent policy value '$name'. Valid: $($script:WtAgentPolicyValues.Keys -join ', ')" }
        $raw = $Policy[$name]
        if ($null -eq $raw) { Remove-ItemProperty -Path $script:WtAgentPolicyKey -Name $name -ErrorAction SilentlyContinue; Write-ItLog -Level INFO -Message "Cleared agent policy $name"; continue }
        switch ($script:WtAgentPolicyValues[$name]) {
            'DWord' {
                $dw = if ($raw -is [int]) { $raw } elseif ($raw -eq 'Allowed') { 1 } elseif ($raw -eq 'Blocked') { 0 } else { throw "$name expects 'Allowed'/'Blocked'/0/1, got '$raw'" }
                Set-ItemProperty -Path $script:WtAgentPolicyKey -Name $name -Value $dw -Type DWord
                Write-ItLog -Level INFO -Message "Set agent policy $name = $dw ($raw)"
            }
            'MultiString' {
                Set-ItemProperty -Path $script:WtAgentPolicyKey -Name $name -Value ([string[]]$raw) -Type MultiString
                Write-ItLog -Level INFO -Message "Set agent policy $name = [$([string[]]$raw -join ', ')]"
            }
        }
    }
    $state
}

function Restore-WtAgentPolicy {
    <# Restore the snapshot returned by Set-WtAgentPolicy / Get-WtAgentPolicyState. #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$State)
    process {
        if (-not $State.KeyExisted) {
            if (Test-Path $State.Key) { Remove-Item -Path $State.Key -Recurse -Force -ErrorAction SilentlyContinue }
            Write-ItLog -Level INFO -Message "Restored agent policy: removed $($State.Key) (did not exist before)"
            return
        }
        foreach ($name in $State.Values.Keys) {
            $snap = $State.Values[$name]
            if ($snap.HadValue) {
                if ($script:WtAgentPolicyValues[$name] -eq 'DWord') { Set-ItemProperty -Path $State.Key -Name $name -Value ([int]$snap.Value) -Type DWord }
                else { Set-ItemProperty -Path $State.Key -Name $name -Value ([string[]]$snap.Value) -Type MultiString }
            }
            else { Remove-ItemProperty -Path $State.Key -Name $name -ErrorAction SilentlyContinue }
        }
        Write-ItLog -Level INFO -Message "Restored agent policy snapshot"
    }
}

function Test-WtAgentPolicyControllable {
    <#
    .SYNOPSIS
        Returns $true only when these helpers can deterministically drive the EFFECTIVE agent
        policy. Two conditions:
          (a) HKLM does not already set the same values — the reader checks HKLM first and
              returns on the first hit (AgentPolicy.h:80-88, 94-118), so a machine GPO would
              outrank the CurrentUser values we write.
          (b) We can actually WRITE the CurrentUser policy hive. Unlike the WinPS ExecutionPolicy
              key (under ShellIds, user-writable), the `Software\Policies` subtree is ACL-restricted
              to administrators. Run test/e2e/tools/Enable-WtAgentPolicyTesting.ps1 ONCE (it
              self-elevates) to grant the current user write access to just this key; afterwards
              this returns $true and the suite runs non-elevated. We probe with a sentinel value;
              denial => not controllable.
        The GPO suite must -Skip when this is false (non-elevated CI stays green), exactly like
        Feature.FreExecutionPolicy.Tests.ps1 skips when its policy isn't controllable.
    #>
    [CmdletBinding()] param()
    # (a) no HKLM override.
    $hklm = 'HKLM:\SOFTWARE\Policies\Microsoft\IntelligentTerminal'
    if (Test-Path $hklm) {
        $p = Get-ItemProperty -Path $hklm -ErrorAction SilentlyContinue
        foreach ($name in $script:WtAgentPolicyValues.Keys) { if ($null -ne $p.$name) { return $false } }
    }
    # (b) HKCU policy hive is writable (needs elevation).
    try {
        $existed = Test-Path $script:WtAgentPolicyKey
        if (-not $existed) { New-Item -Path $script:WtAgentPolicyKey -Force -ErrorAction Stop | Out-Null }
        Set-ItemProperty -Path $script:WtAgentPolicyKey -Name __wt_probe -Value 1 -Type DWord -ErrorAction Stop
        Remove-ItemProperty -Path $script:WtAgentPolicyKey -Name __wt_probe -ErrorAction SilentlyContinue
        if (-not $existed) { Remove-Item -Path $script:WtAgentPolicyKey -Recurse -Force -ErrorAction SilentlyContinue }
        return $true
    }
    catch { return $false }
}
