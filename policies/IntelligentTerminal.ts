version 1.2  utf-3
  (c) 2026 Microsoft Corporation  
http://www.w3.org/2001/XMLSchema  http://www.w3.org/2001/XMLSchema-instance  revision 1.0  schemaVersion 1.0   http://schemas.microsoft.com/GroupPolicy/2006/07/PolicyDefinitions
  
    Windows Terminal   Microsoft.Policies.IntelligentTerminal
     windows  Microsoft.Policies.Windows
  
            minRequiredRevision 1.0
   supportedOn
    definition
                     SUPPORTED_IntelligentTerminal_1_21" displayName="$(string.SUPPORTED_IntelligentTerminal_1_21)
                     SUPPORTED_IntelligentTerminal_AgentPolicy" displayName="$(string.SUPPORTED_IntelligentTerminal_AgentPolicy)" />
    
  
             IntelligentTerminal  displayName(string.IntelligentTerminal)
       parentCategory    windows.WindowsComponents

             DisabledProfileSources  class Both  displayName="$(string.DisabledProfileSources)" explainText="$(string.DisabledProfileSourcesText)" presentation="$(presentation.DisabledProfileSources)" key="Software\Policies\Microsoft\IntelligentTerminal">
      parentCategory ref="IntelligentTerminal
      supportedOn ref="SUPPORTED_IntelligentTerminal_1_21
      
        multiText id="DisabledProfileSources" valueName="DisabledProfileSources" required: true
      
   AllowedAgents class Both displayName="$(string.AllowedAgents)" explainText="$(string.AllowedAgentsText)" presentation="$(presentation.AllowedAgents)" key="Software\Policies\Microsoft\IntelligentTerminal
      parentCategory ref="IntelligentTerminal"
      supportedOn ref="SUPPORTED_IntelligentTerminal_AgentPolicy" 
      
        id="AllowedAgents" valueName="AllowedAgents" required: true
      
       policy name="AllowCustomAgents" class="Both" displayName="$(string.AllowCustomAgents)" explainText="$(string.AllowCustomAgentsText)" key="Software\Policies\Microsoft\IntelligentTerminal" valueName="AllowCustomAgents">
       parentCategory ref="IntelligentTerminal" />
       supportedOn ref="SUPPORTED_IntelligentTerminal_AgentPolicy" />
      
            name="AllowAutoFix" class="Both" displayName="$(string.AllowAutoFix)" explainText="$(string.AllowAutoFixText)" key="Software\Policies\Microsoft\IntelligentTerminal" valueName="AllowAutoFix">
       parentCategory ref="IntelligentTerminal" 
       supportedOn ref="SUPPORTED_IntelligentTerminal_AgentPolicy" />
      
        name="AllowAgentSessionHooks" class="Both" displayName="$(string.AllowAgentSessionHooks)" explainText="$(string.AllowAgentSessionHooksText)" key="Software\Policies\Microsoft\IntelligentTerminal" valueName="AllowAgentSessionHooks">
       parentCategory ref="IntelligentTerminal" 
       supportedOn ref="SUPPORTED_IntelligentTerminal_AgentPolicy" 
      enabledValue
         decimal=( .)
        enabledValue
      disabledValue
        decimal value="0" 
       disabledValue
     . policy
   . policies
policyDefinitions
