[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$EvidenceDirectory,
    [Parameter(Mandatory, ParameterSetName = 'Conversation')][guid]$ConversationId,
    [Parameter(Mandatory, ParameterSetName = 'Record')][ValidateSet('workspace.get')][string]$Method,
    [Parameter(Mandatory, ParameterSetName = 'Record')][guid]$RecordId
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '..\tests\helpers\AgentCenterWorkFlow.ps1')
$root = [IO.Path]::GetFullPath($EvidenceDirectory).TrimEnd('\') + '\'
$runtime = Get-Content -LiteralPath (Join-Path $root 'runtime.json') -Raw | ConvertFrom-Json
if ($runtime.scenario -cne 'WorkFlow' -or -not ([IO.Path]::GetFullPath($runtime.stateRoot)).StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Conversation inspection requires the exact isolated WorkFlow authority.'
}
$fixture = Get-Content -LiteralPath (Join-Path $root 'work-flow.json') -Raw | ConvertFrom-Json
$connection = Open-WorkFlowConnection -StateRoot $runtime.stateRoot
try {
    if ($connection.Welcome.storeId -cne $fixture.storeId) { throw 'Conversation authority changed.' }
    if ($PSCmdlet.ParameterSetName -eq 'Record') {
        $response = Invoke-WorkFlowRequest $connection $Method -Params @{ workspaceId = $RecordId.ToString() }
        ConvertTo-WorkFlowObserverJson -Value $response.data
    }
    else {
        $response = Invoke-WorkFlowRequest $connection 'events.subscribe' -Params @{ scope = @{ kind = 'Conversation'; id = $ConversationId.ToString() } }
        ConvertTo-WorkFlowObserverJson -Value $response.data.snapshot
    }
}
finally { $connection.Pipe.Dispose() }
