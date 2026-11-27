Describe 'Symbol publishing DNS recovery' -Tag Unit {
    BeforeAll {
        $path = Join-Path $PSScriptRoot '..\..\..\build\pipelines\templates-v2\job-publish-symbols-using-symbolrequestprod-api.yml'
        $yaml = [IO.File]::ReadAllText($path)
        $blocks = [regex]::Matches($yaml, '(?ms)^  - pwsh: \|-\r?\n(?<body>.*?)^    displayName: Publish Symbols using internal REST API')
        if ($blocks.Count -ne 1) { throw 'Expected one symbol publication script.' }

        # Execute the real task body with no credentials and a reserved, offline destination.
        $body = ($blocks[0].Groups['body'].Value -split '\r?\n' |
            Where-Object { $_ -notmatch 'Invoke-RestMethod:Headers' } |
            ForEach-Object { $_ -replace '^      ', '' }) -join "`n"
        $body = $body -replace '(?m)^\$BaseUri = .+$', '$BaseUri = "https://symbols.invalid/projects/offline/requests"'
        $body = $body.Replace('${{ parameters.symbolExpiryTime }}', '15').
            Replace('${{ parameters.includePublicSymbolServer }}', 'true').
            Replace('$(XES_APPXMANIFESTVERSION)', '0.0.0.0')
        $script:publish = [scriptblock]::Create('$PSDefaultParameterValues = @{}' + "`n" + $body)
        $script:baseUri = 'https://symbols.invalid/projects/offline/requests'
        $script:requestUri = "$script:baseUri/IntelligentTerminal_0.0.0.0"
        $script:dnsFailure = [System.Net.Http.HttpRequestException]::new(
            'Offline DNS failure',
            [System.Net.Sockets.SocketException]::new([int][System.Net.Sockets.SocketError]::NoData))
    }

    BeforeEach {
        $script:calls = [Collections.Generic.List[object]]::new()
        $script:failures = @{}
        Mock Invoke-RestMethod {
            param($Uri, $Body, $Method, $ContentType, $MaximumRedirection)
            $script:calls.Add([pscustomobject]@{
                Uri = [string]$Uri
                Body = $Body
                Method = $Method
                ContentType = $ContentType
                MaximumRedirection = $MaximumRedirection
            })
            $failure = $script:failures[$script:calls.Count]
            if ($null -ne $failure) { throw $failure }
            [pscustomobject]@{ requestName = 'IntelligentTerminal_0.0.0.0' }
        }
        Mock Start-Sleep {}
        Mock Write-Warning {}
    }

    It 'creates before publishing and preserves the request payloads' {
        & $script:publish | Out-Null

        $script:calls.Count | Should -Be 2
        $script:calls[0].Uri | Should -BeExactly $script:baseUri
        $script:calls[1].Uri | Should -BeExactly $script:requestUri
        $create = $script:calls[0].Body | ConvertFrom-Json
        $create.requestName | Should -BeExactly 'IntelligentTerminal_0.0.0.0'
        $create.expirationTime | Should -Not -BeNullOrEmpty
        $publish = $script:calls[1].Body | ConvertFrom-Json
        $publish.publishToInternalServer | Should -BeTrue
        $publish.publishToPublicServer | Should -BeTrue
        foreach ($call in $script:calls) {
            $call.Method | Should -Be 'POST'
            $call.ContentType | Should -BeExactly 'application/json'
        }
        Should -Invoke Start-Sleep -Times 0 -Exactly
        Should -Invoke Write-Warning -Times 0 -Exactly
    }

    It 'retries a DNS failure while creating without publishing early' {
        $script:failures[1] = $script:dnsFailure
        & $script:publish | Out-Null

        $script:calls.Count | Should -Be 3
        $script:calls[0].Uri | Should -BeExactly $script:baseUri
        $script:calls[1].Uri | Should -BeExactly $script:baseUri
        $script:calls[2].Uri | Should -BeExactly $script:requestUri
        [string]::Equals($script:calls[0].Body, $script:calls[1].Body, [StringComparison]::Ordinal) | Should -BeTrue
        Should -Invoke Start-Sleep -Times 1 -Exactly -ParameterFilter { $Seconds -eq 5 }
        Should -Invoke Write-Warning -Times 1 -Exactly -ParameterFilter { $Message -like '*name resolution*' }
    }

    It 'retries publication DNS failure without recreating the request' {
        $script:failures[2] = $script:dnsFailure
        & $script:publish | Out-Null

        $script:calls.Count | Should -Be 3
        $script:calls[0].Uri | Should -BeExactly $script:baseUri
        $script:calls[1].Uri | Should -BeExactly $script:requestUri
        $script:calls[2].Uri | Should -BeExactly $script:requestUri
        [string]::Equals($script:calls[1].Body, $script:calls[2].Body, [StringComparison]::Ordinal) | Should -BeTrue
        Should -Invoke Start-Sleep -Times 1 -Exactly -ParameterFilter { $Seconds -eq 5 }
    }

    It 'propagates persistent DNS failure after three attempts' {
        $script:failures = @{ 1 = $script:dnsFailure; 2 = $script:dnsFailure; 3 = $script:dnsFailure }
        { & $script:publish } | Should -Throw '*Offline DNS failure*'

        $script:calls.Count | Should -Be 3
        @($script:calls | Where-Object Uri -NE $script:baseUri).Count | Should -Be 0
        Should -Invoke Start-Sleep -Times 2 -Exactly -ParameterFilter { $Seconds -eq 5 }
        Should -Invoke Write-Warning -Times 2 -Exactly
    }

    It 'does not retry <Kind> at request <At>' -TestCases @(
        @{ Kind = 'authorization'; At = 1 }
        @{ Kind = 'server error'; At = 2 }
        @{ Kind = 'timeout'; At = 2 }
        @{ Kind = 'connection reset'; At = 2 }
    ) {
        param($Kind, $At)
        $failure = switch ($Kind) {
            'authorization' { [System.Net.Http.HttpRequestException]::new('Offline non-DNS failure', $null, [System.Net.HttpStatusCode]::Unauthorized) }
            'server error' { [System.Net.Http.HttpRequestException]::new('Offline non-DNS failure', $null, [System.Net.HttpStatusCode]::ServiceUnavailable) }
            'timeout' { [System.Threading.Tasks.TaskCanceledException]::new('Offline non-DNS failure') }
            'connection reset' {
                [System.Net.Http.HttpRequestException]::new('Offline non-DNS failure',
                    [System.Net.Sockets.SocketException]::new([int][System.Net.Sockets.SocketError]::ConnectionReset))
            }
        }
        $script:failures[$At] = $failure
        { & $script:publish } | Should -Throw '*Offline non-DNS failure*'

        $script:calls.Count | Should -Be $At
        Should -Invoke Start-Sleep -Times 0 -Exactly
        Should -Invoke Write-Warning -Times 0 -Exactly
    }

    It 'disables redirects so DNS recovery cannot replay an accepted earlier hop' {
        & $script:publish | Out-Null

        foreach ($call in $script:calls) {
            $call.MaximumRedirection | Should -Be 0
        }
    }
}
