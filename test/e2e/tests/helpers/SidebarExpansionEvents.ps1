function Set-TestSidebarScope {
    param([Parameter(Mandatory)]$App, [Nullable[bool]]$AgentsOnly, [Nullable[bool]]$Recent)
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    if (-not (Test-WtWindowKeyFocusable -App $App)) {
        throw 'External desktop restrictions prevent the owned-foreground gate required to dismiss the filter flyout.'
    }
    foreach ($entry in @(
        @{ Id = 'AgentsOnlyFilterMenuItem'; Value = $AgentsOnly },
        @{ Id = 'RecentAgentSessionsFilterMenuItem'; Value = $Recent }
    )) {
        if ($null -eq $entry.Value) { continue }
        Invoke-UiClick -App $App -Selector FilterTabsButton | Out-Null
        $item = Wait-Until -TimeoutSec 30 -Because "$($entry.Id) is visible in the owned filter flyout" -Condition {
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$App.Hwnd)
            if ($root.Current.ProcessId -ne $App.Pid) { throw 'Filter flyout window ownership changed.' }
            $peer = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::AutomationIdProperty, $entry.Id))
            if ($peer -and -not $peer.Current.IsOffscreen -and $peer.Current.ProcessId -eq $App.Pid) { $peer }
        }
        $item.Current.ControlType | Should -Be ([Windows.Automation.ControlType]::MenuItem)
        $checked = $item.GetCurrentPattern(
            [Windows.Automation.TogglePattern]::Pattern).Current.ToggleState -eq [Windows.Automation.ToggleState]::On
        if ($checked -ne [bool]$entry.Value) {
            $item.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern).Toggle()
        }
        else { Send-WtWindowKey -App $App -Vk 0x1B -RequireForeground | Out-Null }
    }
}

function Initialize-TestSidebarExpansionEvents {
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    if ('ItE2E.SidebarExpansionEvents' -as [type]) { return }
    $references = @(
        Get-ChildItem -LiteralPath (Join-Path $PSHOME 'ref') -Filter '*.dll' -File |
            Select-Object -ExpandProperty FullName
    ) + @(
        [Windows.Automation.Automation].Assembly.Location,
        [Windows.Automation.ExpandCollapseState].Assembly.Location
    )
    Add-Type -ReferencedAssemblies $references -TypeDefinition @'
using System;
using System.Windows.Automation;
namespace ItE2E {
    public sealed class SidebarExpansionEvents : IDisposable {
        readonly AutomationElement element;
        readonly AutomationPropertyChangedEventHandler handler;
        readonly object gate = new object();
        string[] values = new string[0];
        public SidebarExpansionEvents(AutomationElement element) {
            this.element = element;
            handler = (sender, args) => {
                lock(gate) {
                    var next = new string[values.Length + 1];
                    for(int i = 0; i < values.Length; i++) next[i] = values[i];
                    next[values.Length] = ((ExpandCollapseState)Convert.ToInt32(args.NewValue)).ToString();
                    values = next;
                }
            };
            Automation.AddAutomationPropertyChangedEventHandler(element, TreeScope.Element,
                handler, ExpandCollapsePattern.ExpandCollapseStateProperty);
        }
        public string[] Snapshot() { lock(gate) return (string[])values.Clone(); }
        public void Dispose() {
            Automation.RemoveAutomationPropertyChangedEventHandler(element, handler);
        }
    }
}
'@
}
