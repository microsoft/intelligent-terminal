param(
    [Parameter(Mandatory)][string]$Executable,
    [Parameter(Mandatory)][string]$BaseUrl,
    [Parameter(Mandatory)][string]$SessionId,
    [Parameter(Mandatory)][string]$Config,
    [Parameter(Mandatory)][string]$Data,
    [Parameter(Mandatory)][string]$Cache,
    [Parameter(Mandatory)][string]$State,
    [Parameter(Mandatory)][string]$CredentialFile
)

$env:XDG_CONFIG_HOME = $Config
$env:OPENCODE_CONFIG_DIR = Join-Path $Config 'opencode'
$env:XDG_DATA_HOME = $Data
$env:XDG_CACHE_HOME = $Cache
$env:XDG_STATE_HOME = $State
$env:OPENCODE_DISABLE_AUTOUPDATE = 'true'
$env:OPENCODE_SERVER_USERNAME = 'opencode'
$credential = Get-Content -LiteralPath $CredentialFile -Raw | ConvertTo-SecureString
Remove-Item -LiteralPath $CredentialFile -Force
$env:OPENCODE_SERVER_PASSWORD = [pscredential]::new('opencode', $credential).GetNetworkCredential().Password
foreach ($name in @('OPENCODE_CLIENT', 'OPENCODE_CONFIG', 'OPENCODE_CONFIG_CONTENT')) {
    Remove-Item -LiteralPath "Env:\$name" -ErrorAction SilentlyContinue
}
& $Executable --server $BaseUrl --session $SessionId
if ($LASTEXITCODE -ne 0) { throw "Native OpenCode TUI exited with $LASTEXITCODE" }
