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
                    next[values.Length] = args.NewValue.ToString();
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
