BeforeAll {
    $fixture = Join-Path $PSScriptRoot '..\fixtures\Emit-SidebarSessionHooks.ps1'
    $tokens = $null
    $errors = $null
    $ast = [Management.Automation.Language.Parser]::ParseFile($fixture, [ref]$tokens, [ref]$errors)
    if ($errors) { throw 'Sidebar hook fixture does not parse.' }
    $writer = $ast.Find({
        param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq 'Write-SidebarHookReceipt'
    }, $true)
    if (-not $writer) { throw 'Sidebar receipt publisher was not found.' }
    . ([scriptblock]::Create($writer.Extent.Text))
    $script:receiptRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot (
        '..\artifacts\sidebar-receipt-' + [guid]::NewGuid().ToString('N'))))
    New-Item -ItemType Directory -Path $script:receiptRoot | Out-Null
}

AfterAll {
    Remove-Item -LiteralPath $script:receiptRoot -Recurse -Force
}

Describe 'Sidebar hook receipt publication' -Tag Unit {
    It 'publishes only closed complete JSON and preserves the receipt schema' {
        $script:finalReceipt = Join-Path $script:receiptRoot 'complete.json'
        Mock Set-Content {
            Test-Path -LiteralPath $script:finalReceipt | Should -BeFalse
            $LiteralPath | Should -Match '\.pending$'
            [IO.Path]::GetDirectoryName($LiteralPath) | Should -Be $script:receiptRoot
            [IO.File]::WriteAllText($LiteralPath, ($Value -join [Environment]::NewLine),
                [Text.UTF8Encoding]::new($false))
            Test-Path -LiteralPath $LiteralPath | Should -BeTrue
            Test-Path -LiteralPath $script:finalReceipt | Should -BeFalse
        }
        Write-SidebarHookReceipt -ReceiptPath $script:finalReceipt -EventCount 3 -PaneSessionId 'fixture-pane'
        $receipt = Get-Content -LiteralPath $script:finalReceipt -Raw | ConvertFrom-Json
        $receipt.events | Should -Be 3
        $receipt.pane_session_id | Should -Be 'fixture-pane'
        @(Get-ChildItem -LiteralPath $script:receiptRoot -Filter '*.pending') | Should -HaveCount 0
        Should -Invoke Set-Content -Times 1 -Exactly
    }

    It 'does not publish a partial receipt when writing fails' {
        $final = Join-Path $script:receiptRoot 'failed-write.json'
        Mock Set-Content {
            [IO.File]::WriteAllText($LiteralPath, '{"events":')
            throw [IO.IOException]::new('Fixture write failure')
        }
        { Write-SidebarHookReceipt -ReceiptPath $final -EventCount 2 -PaneSessionId 'fixture-pane' } |
            Should -Throw '*Fixture write failure*'
        Test-Path -LiteralPath $final | Should -BeFalse
        @(Get-ChildItem -LiteralPath $script:receiptRoot -Filter '*.pending') | Should -HaveCount 0
    }

    It 'fails without replacing a preexisting final receipt and cleans owned staging' {
        $final = Join-Path $script:receiptRoot 'existing.json'
        [IO.File]::WriteAllText($final, 'original receipt')
        $hash = (Get-FileHash -LiteralPath $final).Hash
        { Write-SidebarHookReceipt -ReceiptPath $final -EventCount 2 -PaneSessionId 'fixture-pane' } |
            Should -Throw
        (Get-FileHash -LiteralPath $final).Hash | Should -Be $hash
        @(Get-ChildItem -LiteralPath $script:receiptRoot -Filter '*.pending') | Should -HaveCount 0
    }

    It 'preserves the original write failure when staging <CleanupFault> fails' -TestCases @(
        @{ CleanupFault = 'remove' },
        @{ CleanupFault = 'lookup' }
    ) {
        param($CleanupFault)
        $directory = Join-Path $script:receiptRoot "failed-$CleanupFault"
        New-Item -ItemType Directory -Path $directory | Out-Null
        $final = Join-Path $directory 'receipt.json'
        $script:publicationException = [IO.IOException]::new('Original publication failure')
        $script:pendingReceipt = $null
        Mock Set-Content {
            $script:pendingReceipt = $LiteralPath
            [IO.File]::WriteAllText($LiteralPath, '{"events":')
            throw $script:publicationException
        }
        if ($CleanupFault -eq 'remove') {
            Mock Remove-Item { throw [IO.IOException]::new('Staging cleanup failure') } -ParameterFilter {
                $LiteralPath.EndsWith('.pending')
            }
        }
        else {
            Mock Test-Path { throw [IO.IOException]::new('Staging lookup failure') } -ParameterFilter {
                $LiteralPath.EndsWith('.pending')
            }
        }
        $failure = $null
        try { Write-SidebarHookReceipt -ReceiptPath $final -EventCount 1 -PaneSessionId 'fixture-pane' }
        catch { $failure = $_ }
        $failure | Should -Not -BeNullOrEmpty
        $failure.Exception | Should -BeOfType ([AggregateException])
        $failure.Exception.InnerExceptions | Should -HaveCount 2
        [object]::ReferenceEquals($failure.Exception.InnerExceptions[0], $script:publicationException) | Should -BeTrue
        $failure.Exception.InnerExceptions[1].Message | Should -Match 'Staging (cleanup|lookup) failure'
        [IO.File]::Exists($final) | Should -BeFalse
        [IO.File]::Exists($script:pendingReceipt) | Should -BeTrue
    }

    It 'preserves a failed move and the existing final receipt when staging removal fails' {
        $directory = Join-Path $script:receiptRoot 'failed-move'
        New-Item -ItemType Directory -Path $directory | Out-Null
        $final = Join-Path $directory 'receipt.json'
        [IO.File]::WriteAllText($final, 'original receipt')
        $hash = (Get-FileHash -LiteralPath $final).Hash
        Mock Remove-Item { throw [IO.IOException]::new('Staging cleanup failure') } -ParameterFilter {
            $LiteralPath.EndsWith('.pending')
        }
        $failure = $null
        try { Write-SidebarHookReceipt -ReceiptPath $final -EventCount 1 -PaneSessionId 'fixture-pane' }
        catch { $failure = $_ }
        $failure | Should -Not -BeNullOrEmpty
        $failure.Exception | Should -BeOfType ([AggregateException])
        $failure.Exception.InnerExceptions | Should -HaveCount 2
        $failure.Exception.InnerExceptions[0].GetBaseException() | Should -BeOfType ([IO.IOException])
        $failure.Exception.InnerExceptions[1].Message | Should -Match 'Staging cleanup failure'
        (Get-FileHash -LiteralPath $final).Hash | Should -Be $hash
        @(Get-ChildItem -LiteralPath $directory -Filter 'receipt.json.*.pending') |
            Should -HaveCount 1
    }

    It 'publishes unique receipts independently with non-ASCII file data' {
        $pane = 'fixture-' + [char]0x00E9 + [char]0x4E2D
        foreach ($count in @(0, 2)) {
            $final = Join-Path $script:receiptRoot ("unique-$count.json")
            Write-SidebarHookReceipt -ReceiptPath $final -EventCount $count -PaneSessionId $pane
            $receipt = Get-Content -LiteralPath $final -Raw | ConvertFrom-Json
            $receipt.events | Should -Be $count
            $receipt.pane_session_id | Should -Be $pane
        }
        @(Get-ChildItem -LiteralPath $script:receiptRoot -Filter '*.pending') | Should -HaveCount 0
    }
}
