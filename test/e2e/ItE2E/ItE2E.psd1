@{
    RootModule        = 'ItE2E.psm1'
    ModuleVersion     = '0.1.0'
    GUID              = 'b6e2a1c4-2f5d-4a7b-9c8e-1f2a3b4c5d6e'
    Author            = 'Intelligent Terminal Team'
    CompanyName       = 'Microsoft'
    Description       = 'E2E test framework for Intelligent Terminal: drive (wtcli + winapp ui) and verify a deployed packaged build.'
    PowerShellVersion = '7.2'
    FunctionsToExport = '*'
    CmdletsToExport   = @()
    AliasesToExport   = '*'
    VariablesToExport = @()
    PrivateData       = @{ PSData = @{ Tags = @('IntelligentTerminal', 'E2E', 'Testing', 'WindowsTerminal') } }
}
