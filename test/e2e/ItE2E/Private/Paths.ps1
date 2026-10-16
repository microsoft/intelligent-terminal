# Paths.ps1 — resolve the deployed app, its binaries, data files, and COM CLSID.
# This is the brick that pins WHICH package we test and HOW to reach it.

# Fixed per-brand COM CLSIDs (src/cascadia/WindowsTerminal/TerminalProtocolComServer.h:19-27).
# wtcli's CLSIDFromString requires the canonical braced form WT sets in WT_COM_CLSID.
$script:ItBrandClsids = [ordered]@{
    Release = '{A2E4F6B8-1C3D-4E5F-A6B7-C8D9E0F1A2B3}'
    Preview = '{B3F5A7C9-2D4E-4F6A-B7C8-D9E0F1A2B3C4}'
    Canary  = '{C4A6B8D0-3E5F-4A7B-C8D9-E0F1A2B3C4D5}'
    Dev     = '{D5B7C9E1-4F6A-4B8C-D9E0-F1A2B3C4D5E6}'
}

$script:ItKnownFamilies = [ordered]@{
    Store = 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe'
    Dev   = 'IntelligentTerminal_rd9vj3e6a2mbr'
}

function Get-ItDevPackageFamilyName {
    $script:ItKnownFamilies.Dev
}

# Which brand CLSID a known package family registers (TerminalProtocolComServer.h brands).
# The Store package ships the Release brand; the dev sideload ships the Dev brand. This lets
# Resolve-WtComClsid probe the CORRECT brand for the package under test instead of blindly
# taking the first responder — critical when BOTH Store and Dev are installed, since otherwise
# probing Release first cold-launches the (window-less) Store server and misroutes wtcli.
$script:ItFamilyBrand = @{
    'Microsoft.IntelligentTerminal_8wekyb3d8bbwe' = 'Release'
    'IntelligentTerminal_rd9vj3e6a2mbr'           = 'Dev'
}

function Get-ItWorktreePackageInfo {
    param([Parameter(Mandatory)]$Package)
    $pfn = [string]$Package.PackageFamilyName
    if ($pfn -cnotmatch '^IntelligentTerminal\.Worktree\.([A-Za-z][A-Za-z0-9]{0,20})_rd9vj3e6a2mbr$') {
        throw 'Unsupported private worktree package family/token.'
    }
    $token = $Matches[1]
    $name = "IntelligentTerminal.Worktree.$token"
    $manifest = Get-AppxPackageManifest -Package $Package.PackageFullName -ErrorAction Stop
    if ($Package.Name -cne $name -or $Package.Publisher -cne 'CN=Intelligent Terminal Dev' -or
        $manifest.Package.Identity.Name -cne $name -or
        $manifest.Package.Identity.Publisher -cne 'CN=Intelligent Terminal Dev' -or
        [string]$manifest.Package.Identity.Version -cne [string]$Package.Version -or
        $Package.PackageFullName -cnotmatch ('^' + [regex]::Escape($name + '_' + $Package.Version) + '_(x64|arm64|x86)__rd9vj3e6a2mbr$')) {
        throw 'Registered private package identity/publisher/version does not match its manifest.'
    }
    $app = @($manifest.SelectNodes("//*[local-name()='Application' and @Id='App' and @Executable='WindowsTerminal.exe']"))
    $alias = "wtai-$token.exe"
    $aliases = @($manifest.SelectNodes("//*[local-name()='ExecutionAlias']"))
    $launch = @($app | ForEach-Object {
        $_.SelectNodes(".//*[local-name()='Extension' and @Category='windows.appExecutionAlias' and @Executable='wtai.exe']//*[local-name()='ExecutionAlias']")
    })
    $classes = @($manifest.SelectNodes("//*[local-name()='Extension' and @Category='windows.comServer']//*[local-name()='Class']"))
    if ($app.Count -ne 1 -or $launch.Count -ne 1 -or $launch[0].Alias -cne $alias -or
        @($aliases | Where-Object Alias -CEQ $alias).Count -ne 1 -or $classes.Count -ne 1 -or
        $classes[0].ParentNode.Executable -cne 'WindowsTerminal.exe' -or
        -not [object]::ReferenceEquals($app[0], $classes[0].SelectSingleNode("ancestor::*[local-name()='Application']"))) {
        throw 'Private manifest must register its tokenized alias and own Terminal protocol server.'
    }
    $guid = [guid]::Parse($classes[0].Id)
    $clsid = $guid.ToString('B').ToUpperInvariant()
    if ($guid -eq [guid]::Empty -or $clsid -in $script:ItBrandClsids.Values) {
        throw 'Private protocol CLSID must be unique, not an ordinary brand CLSID.'
    }
    foreach ($other in @(Get-AppxPackage -ErrorAction Stop | Where-Object PackageFullName -CNE $Package.PackageFullName)) {
        $otherManifest = Get-AppxPackageManifest -Package $other.PackageFullName -ErrorAction Stop
        foreach ($class in $otherManifest.SelectNodes("//*[local-name()='Extension' and @Category='windows.comServer']//*[local-name()='Class']")) {
            if ([guid]::Parse($class.Id) -eq $guid) { throw 'Private protocol CLSID is registered by another package.' }
        }
    }
    foreach ($binary in @('WindowsTerminal.exe', 'wtai.exe', 'wtcli.exe', 'wta.exe', 'TerminalApp.dll')) {
        if (-not $Package.InstallLocation -or
            -not (Test-Path -LiteralPath (Join-Path $Package.InstallLocation $binary) -PathType Leaf)) {
            throw "Private package requires co-located binary: $binary"
        }
    }
    [pscustomobject]@{ Alias = $alias; ComClsid = $clsid }
}

function Resolve-ItApp {
    <#
    .SYNOPSIS
        Build the immutable app descriptor used by every primitive: package identity,
        binaries (wtcli/wta/WindowsTerminal), data files (settings/state), log root.
    .PARAMETER Package
        'Store', 'Dev', or an explicit Intelligent Terminal PackageFamilyName.
        Ordinary Windows Terminal families are not test targets.
    .PARAMETER IfInstalled
        Return no descriptor when the explicitly selected package is not installed.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Package, [switch]$IfInstalled)

    if ($Package -eq 'Auto') { throw "'Auto' is not allowed for explicit package resolution." }
    if ($Package -match '^Microsoft\.WindowsTerminal') {
        throw 'The ordinary Windows Terminal package is not an Intelligent Terminal test target.'
    }
    $private = $Package -like 'IntelligentTerminal.Worktree.*'
    if ($private -and $Package -cnotmatch '^IntelligentTerminal\.Worktree\.[A-Za-z][A-Za-z0-9]{0,20}_rd9vj3e6a2mbr$') {
        throw 'Unsupported private worktree package family/token.'
    }

    $candidates = switch ($Package) {
        'Store' { @($script:ItKnownFamilies.Store) }
        'Dev' { @($script:ItKnownFamilies.Dev) }
        default { @($Package) }
    }

    $pkg = $null; $pfn = $null
    foreach ($c in $candidates) {
        $registered = @(Get-AppxPackage -ErrorAction Stop | Where-Object PackageFamilyName -eq $c)
        if ($private -and $registered.Count -gt 1) { throw 'Ambiguous registered private package.' }
        $p = $registered | Select-Object -First 1
        if ($p) {
            # Prefer a candidate whose InstallLocation is readable (co-located binaries).
            if (-not $pkg) { $pkg = $p; $pfn = $c }
            if ($p.InstallLocation -and (Test-Path $p.InstallLocation)) { $pkg = $p; $pfn = $c; break }
        }
    }
    if (-not $pkg) {
        if ($IfInstalled) { return }
        throw "No Intelligent Terminal package found for selector '$Package'. Install one first."
    }

    $install = $pkg.InstallLocation
    if ($private -and $pkg.PackageFamilyName -cne $Package) { throw 'Registered private package family does not exactly match the selector.' }
    $privateInfo = if ($private) { Get-ItWorktreePackageInfo -Package $pkg }
    $aumid = (Get-StartApps | Where-Object AppID -like "$pfn!*" | Select-Object -First 1 -ExpandProperty AppID)
    if (-not $aumid) { $aumid = "$pfn!App" }

    # Binaries: prefer co-located (have package identity, readable for Store).
    $wtcli = if ($install -and (Test-Path (Join-Path $install 'wtcli.exe'))) { Join-Path $install 'wtcli.exe' }
    else { (Get-Command wtcli -ErrorAction SilentlyContinue).Source }
    $wt = if ($install -and (Test-Path (Join-Path $install 'WindowsTerminal.exe'))) { Join-Path $install 'WindowsTerminal.exe' } else { $null }

    # A deployed package must use its own co-located WTA. Falling back to a
    # repository build can silently run Dev code while testing the Store package.
    $wta = if ($install) { Join-Path $install 'wta.exe' } else { $null }

    $localState = Join-Path $env:LOCALAPPDATA "Packages\$pfn\LocalState"
    $logRoot = Join-Path $env:LOCALAPPDATA "Packages\$pfn\LocalCache\Local\IntelligentTerminal\logs"

    # Launch entry point: tests activate via AUMID (shell:AppsFolder\<PFN>!App),
    # which is package-specific and therefore reliably hits THIS package even when
    # both the Store and a Dev build are installed (see Start-Terminal). The global
    # `wtai` AppExecutionAlias is owned by a single package, so it is ambiguous in
    # that scenario — it is kept here only as a last-resort fallback.
    $launchAlias = (Get-Command 'wtai' -ErrorAction SilentlyContinue | Select-Object -First 1).Source

    $descriptor = [pscustomobject]@{
        Package          = $pfn
        PackageFullName  = $pkg.PackageFullName
        Version          = [string]$pkg.Version
        AppUserModelId   = $aumid
        LaunchAlias      = $launchAlias
        InstallLocation  = $install
        WtcliPath        = $wtcli
        WtaPath          = $wta
        WindowsTerminal  = $wt
        SettingsPath     = Join-Path $localState 'settings.json'
        StatePath        = Join-Path $localState 'state.json'
        LocalStateDir    = $localState
        LogRootDir       = $logRoot
        ComClsid         = $null   # filled by Resolve-WtComClsid once WT is running
        Hwnd             = $null   # filled by harness after launch
        Pid              = $null
        LogStartOffset   = @{}     # per-file byte offset captured at test start
    }
    if ($private) {
        $descriptor.AppUserModelId = "$pfn!App"
        $descriptor.LaunchAlias = Join-Path $env:LOCALAPPDATA "Microsoft\WindowsApps\$pfn\$($privateInfo.Alias)"
        $descriptor | Add-Member PrivateProtocolClsid $privateInfo.ComClsid
    }
    $descriptor
}

function Get-RunnableWtaPath {
    <#
    .SYNOPSIS
        The packaged wta.exe in WindowsApps cannot be launched by an external process
        ("Access is denied"), unlike wtcli. For wta subcommands that do NOT require WT
        package identity (sessions, eval), run a copy of the exact packaged binary from a
        writable temp dir. Staging is isolated by package family, version, and
        binary hash so Store and Dev packages can never reuse one another's copy.
    #>
    [CmdletBinding()] param([Parameter(Mandatory)]$App)
    if ($App.PSObject.Properties.Name -contains 'WtaRunnable' -and $App.WtaRunnable -and (Test-Path $App.WtaRunnable)) { return $App.WtaRunnable }

    $runnable = $null
    if ($App.WtaPath -and $App.WtaPath -like '*WindowsApps*') {
        $safePackage = $App.Package -replace '[^A-Za-z0-9._-]', '_'
        $destContext = "$env:TEMP\ite2e-wta\$safePackage\$($App.Version)\<sha256>\wta.exe"
        try {
            $sourceHash = (Get-FileHash -LiteralPath $App.WtaPath -Algorithm SHA256 -ErrorAction Stop).Hash
            $dir = Join-Path $env:TEMP "ite2e-wta\$safePackage\$($App.Version)\$sourceHash"
            if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir -ErrorAction Stop | Out-Null }
            $dest = Join-Path $dir 'wta.exe'
            $destContext = $dest
            $destHash = if (Test-Path $dest) { (Get-FileHash -LiteralPath $dest -Algorithm SHA256 -ErrorAction Stop).Hash } else { $null }
            if ($destHash -ne $sourceHash) {
                Copy-Item -LiteralPath $App.WtaPath -Destination $dest -Force -ErrorAction Stop
                Write-ItLog -Level INFO -Message "Staged runnable wta -> $dest"
            }
            $stagedHash = (Get-FileHash -LiteralPath $dest -Algorithm SHA256 -ErrorAction Stop).Hash
            if ($stagedHash -ne $sourceHash) { throw "staged binary hash $stagedHash does not match source hash $sourceHash" }
            $runnable = $dest
        }
        catch {
            throw "Could not stage packaged wta '$($App.WtaPath)' to '$destContext': $($_.Exception.Message)"
        }
        $bundleSource = Join-Path $App.InstallLocation 'wt-agent-hooks'
        $bundleDest = Join-Path $dir 'wt-agent-hooks'
        if (Test-Path $bundleSource) {
            $swapId = [guid]::NewGuid().ToString('N')
            $bundleStage = "$bundleDest.staging.$swapId"
            $bundleBackup = "$bundleDest.backup.$swapId"
            $backupPresent = $false
            try {
                Copy-Item -LiteralPath $bundleSource -Destination $bundleStage -Recurse -Force -ErrorAction Stop
                if (Test-Path $bundleDest) {
                    Move-Item -LiteralPath $bundleDest -Destination $bundleBackup -ErrorAction Stop
                    $backupPresent = $true
                }
                try {
                    Move-Item -LiteralPath $bundleStage -Destination $bundleDest -ErrorAction Stop
                }
                catch {
                    $activationError = $_
                    if ($backupPresent) {
                        try {
                            if (Test-Path $bundleDest) { Remove-Item -LiteralPath $bundleDest -Recurse -Force -ErrorAction Stop }
                            Move-Item -LiteralPath $bundleBackup -Destination $bundleDest -ErrorAction Stop
                            $backupPresent = $false
                            Write-ItLog -Level WARN -Message "Hook bundle activation failed; restored prior bundle at '$bundleDest'."
                        }
                        catch {
                            Write-ItLog -Level WARN -Message "Hook bundle activation and rollback failed; prior bundle retained at '$bundleBackup': $_"
                        }
                    }
                    throw $activationError
                }
                if ($backupPresent) {
                    try {
                        Remove-Item -LiteralPath $bundleBackup -Recurse -Force -ErrorAction Stop
                        $backupPresent = $false
                    }
                    catch {
                        Write-ItLog -Level WARN -Message "Hook bundle activated, but backup cleanup failed; retained '$bundleBackup': $_"
                    }
                }
                Write-ItLog -Level INFO -Message "Staged hook bundle -> $bundleDest"
            }
            catch { Write-ItLog -Level WARN -Message "Could not refresh packaged hook bundle: $_" }
            finally {
                if (Test-Path $bundleStage) {
                    try { Remove-Item -LiteralPath $bundleStage -Recurse -Force -ErrorAction Stop }
                    catch { Write-ItLog -Level WARN -Message "Could not clean hook bundle staging directory '$bundleStage': $_" }
                }
            }
        }
    }
    elseif ($App.WtaPath -and (Test-Path $App.WtaPath)) { $runnable = $App.WtaPath }
    if (-not $runnable) { throw "No runnable wta.exe available for $($App.Package)." }
    $App | Add-Member -NotePropertyName WtaRunnable -NotePropertyValue $runnable -Force
    $runnable
}

function Resolve-WtComClsid {
    <#
    .SYNOPSIS
        Find the live COM CLSID for the running WT of this package by probing each
        per-brand CLSID until wtcli connects. Caches the result on the app object.
    .NOTES
        WT registers the server with CoRegisterClassObject(CLSCTX_LOCAL_SERVER), so the
        terminal MUST already be running. Returns the CLSID string, or throws.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [int]$TimeoutSec = 30
    )
    $private = $App.Package -like 'IntelligentTerminal.Worktree.*'
    $verified = if ($private) { Resolve-ItApp -Package $App.Package }
    if ($private -and ($verified.PackageFullName -cne $App.PackageFullName -or
        $verified.InstallLocation -cne $App.InstallLocation -or $verified.WtcliPath -cne $App.WtcliPath)) {
        throw 'Private package registration changed; refuse COM activation.'
    }
    if ($App.ComClsid) {
        if ($private -and $App.ComClsid -cne $verified.PrivateProtocolClsid) { throw 'Private protocol CLSID changed.' }
        return $App.ComClsid
    }
    $wtcli = $App.WtcliPath
    if (-not $wtcli) { throw "wtcli not resolved for $($App.Package)." }

    # Probe the brand that THIS package registers (Store=Release, Dev=Dev). When both packages
    # are installed they share the COM activation namespace, so blindly taking the first
    # responding brand can cold-launch the OTHER package's window-less server and latch onto the
    # wrong CLSID (→ later GetActivePane 0x80004005). For a KNOWN package family we therefore
    # probe ONLY its brand (the Wait-Until retry absorbs startup); an unknown/custom family falls
    # back to scanning every brand.
    $expectedBrand = $script:ItFamilyBrand[$App.Package]
    $probeOrder = if ($private) { @('Worktree') } elseif ($expectedBrand) { @($expectedBrand) } else { @($script:ItBrandClsids.Keys) }

    $found = Wait-Until -TimeoutSec $TimeoutSec -IntervalSec 1 -Because "a brand CLSID that wtcli can connect to" -Quiet -Condition {
        foreach ($brand in $probeOrder) {
            $clsid = if ($private) { $verified.PrivateProtocolClsid } else { $script:ItBrandClsids[$brand] }
            $r = Invoke-Native -FilePath $wtcli -Arguments @('--json', 'list-windows') -TimeoutSec 8 -Environment @{ WT_COM_CLSID = $clsid }
            if ($r.ExitCode -eq 0) {
                $j = $r.StdOut | ConvertFrom-JsonSafe
                if ($null -ne $j -and $null -ne $j.windows) {
                    Write-ItLog -Level INFO -Message "Resolved COM CLSID brand=$brand clsid=$clsid (expected=$expectedBrand)"
                    return $clsid
                }
            }
        }
        return $null
    }
    if (-not $found) { throw "Could not resolve a live WT_COM_CLSID for $($App.Package). Is the terminal running?" }
    $App.ComClsid = $found
    return $found
}
