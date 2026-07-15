# ItE2E.psm1 — module loader. Dot-sources Private then Public, exports public functions.

$ErrorActionPreference = 'Stop'

# Load private (helpers) first, then public (primitives/observe/verify).
foreach ($scope in @('Private', 'Public')) {
    $dir = Join-Path $PSScriptRoot $scope
    if (-not (Test-Path $dir)) { continue }
    Get-ChildItem $dir -Filter *.ps1 -File | Sort-Object Name | ForEach-Object {
        . $_.FullName
    }
}

# Export every function defined by the Public files (and the few private ones tests use).
$publicFns = @(
    # Harness
    'Resolve-ItApp', 'Resolve-WtComClsid', 'Get-ItTestPackage', 'Start-Terminal', 'Start-TerminalClean', 'Stop-Terminal',
    'Reset-TerminalState', 'Backup-WtConfig', 'Restore-WtConfig', 'Clear-WtConfig', 'Get-WtProcessesForApp', 'Stop-AppInstances', 'Stop-StaleItInstances', 'Start-TerminalFre', 'Get-DescendantWtaIds',
    # Core (useful in tests)
    'Wait-Until', 'Test-Until', 'Invoke-Native', 'Write-ItLog', 'ConvertFrom-JsonSafe',
    # Wt
    'Invoke-WtCli', 'Invoke-WtCliRaw', 'Get-WtWindows', 'Get-WtTabs', 'Get-WtPanes', 'Get-ActivePane',
    'Get-WtPaneStatus', 'New-WtTab', 'Split-WtPane', 'Close-WtPane', 'Set-WtPaneFocus',
    'Send-WtInput', 'Send-WtKeys', 'Get-WtCapture', 'Wait-WtPaneExit', 'Invoke-RunCommand', 'Send-WtEvent',
    # Settings / Fre
    'Get-WtSettingsObject', 'Set-WtSetting', 'Get-WtSetting', 'Set-WtAgent', 'Set-WtDelegateAgent',
    'Set-WtAutofix', 'Set-WtPanePosition', 'Set-WtSettings', 'ConvertFrom-JsonC',
    'Get-WtStateObject', 'Set-WtState', 'Invoke-FrePass', 'Reset-Fre', 'Get-FreCompleted',
    'Invoke-FrePassViaUi', 'Test-FreShowing',
    'Get-WtExecutionPolicyState', 'Set-WtExecutionPolicy', 'Restore-WtExecutionPolicy',
    'Test-WtExecutionPolicyControllable', 'Test-WtPwshBlocksShellIntegration',
    # Hooks (agent session-management hook install state on disk)
    'Get-CopilotHooksInstalled', 'Backup-CopilotConfig', 'Restore-CopilotConfig', 'Remove-CopilotHooksEntry',
    # Policy (agent GPO via HKCU)
    'Get-WtAgentPolicyState', 'Set-WtAgentPolicy', 'Restore-WtAgentPolicy', 'Test-WtAgentPolicyControllable',
    # Ui
    'Get-UiTree', 'Find-UiElement', 'Get-UiElement', 'Test-UiElementEnabled', 'Invoke-UiElement', 'Invoke-UiClick', 'Set-UiValue', 'Get-UiValue',
    'Wait-UiElement', 'Test-UiElementExists', 'Save-UiScreenshot', 'Get-WtWindowHwnds', 'Test-WinAppAvailable',
    'Send-WtWindowKey', 'Set-WtWindowForeground', 'Test-WtWindowKeyFocusable', 'Open-WtSettings', 'Test-CommandPaletteOpen', 'Invoke-SettingsNav',
    # Observe
    'Get-ItLogDir', 'Initialize-LogOffsets', 'Get-ItLogText', 'Start-WtEventListener', 'Get-WtEvents',
    'Wait-WtEvent', 'Stop-WtEventListener', 'Get-ContextBundle', 'ConvertTo-ContextText',
    # Agent / Autofix / Sessions
    'Open-AgentPane', 'Set-AgentPaneFocus', 'Wait-AgentReady', 'Get-WtaLocalizedTextRegex', 'Get-WtReswTextValues', 'Get-WtReswTextRegex', 'Get-RecommendationCardRegex', 'Send-AgentPrompt', 'Wait-AgentState',
    'Test-AgentPaneOpen', 'Stop-AgentPane', 'Restore-AgentPane', 'Get-AgentPaneSession', 'Get-AgentPaneText',
    'Send-AgentKey', 'Send-AgentShiftEnter', 'Clear-AgentInput', 'Send-AgentWin32Key', 'Send-AgentAltV',
    'Open-AgentCommandMenu', 'Get-AgentMenuSelection', 'Invoke-AgentMenuItem',
    'Test-AgentPopupShown', 'Wait-AgentPermission', 'Resolve-AgentPermission', 'Assert-AgentPaneText',
    'Set-ClipboardImage',
    'Wait-Autofix', 'Wait-WtCommandFailure', 'Send-AutofixState', 'Invoke-FailingCommand', 'Get-WtSessions', 'Invoke-Wta',
    'Open-SessionList', 'Close-SessionList', 'Test-SessionListShown', 'Get-SessionViewRenderRegex', 'Get-SessionRows',
    'Get-SessionListSelection', 'Select-SessionRow', 'Resume-Session', 'Get-SessionListJson',
    # Verify
    'Assert-Setting', 'Assert-State', 'Assert-Ui', 'Assert-Xaml', 'Assert-Script', 'Assert-Pane',
    'Assert-WtEvent', 'Assert-Log', 'Assert-AI', 'Test-AIClaim', 'Invoke-AgentJudge', 'Get-JsonObjectFromText'
)
Export-ModuleMember -Function $publicFns -Alias *
