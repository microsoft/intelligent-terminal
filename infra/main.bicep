targetScope = 'resourceGroup'

@description('A short lowercase alphanumeric prefix used when generating globally unique resource names.')
@minLength(3)
@maxLength(12)
param resourcePrefix string = 'itremote'

@description('azd environment name. Names are combined with a deterministic suffix.')
@minLength(1)
@maxLength(20)
param environmentName string

@description('Azure region for all regional resources.')
param location string = resourceGroup().location

@description('Microsoft Entra tenant that owns the API app registration and all MVP users.')
param tenantId string

@description('Entra API audience, normally api://<the API app registration application ID>.')
param apiAudience string

@description('Delegated Entra scope required on every API access token.')
param apiScope string = 'RemoteAgentControl.Access'

@description('Initial image used only while provisioning. azd deploy replaces it with the service image after ACR and role assignment creation.')
param containerImage string = 'mcr.microsoft.com/azuredocs/containerapps-helloworld:latest'

@description('Minimum Container Apps replicas. Keep 0 for the cost-conscious MVP dev default; use 1 or more for production availability.')
@minValue(0)
@maxValue(10)
param minReplicas int = 0

@description('Use Standard for production; Free is intended only for constrained development environments.')
@allowed([
  'Free_F1'
  'Standard_S1'
])
param webPubSubSkuName string = 'Standard_S1'

@description('Tags applied to every resource.')
param tags object = {
  application: 'intelligent-terminal'
  component: 'remote-agent-control'
  environment: environmentName
}

var suffix = take(uniqueString(subscription().id, resourceGroup().id, environmentName), 8)
var compactPrefix = toLower(replace(resourcePrefix, '-', ''))
var acrName = take('${compactPrefix}${suffix}', 50)
var cosmosName = take('${compactPrefix}-${suffix}', 44)
var webPubSubName = take('${compactPrefix}-wps-${suffix}', 63)
var identityName = take('${compactPrefix}-api-${suffix}', 128)
var workspaceName = take('${compactPrefix}-logs-${suffix}', 63)
var appInsightsName = take('${compactPrefix}-appi-${suffix}', 260)
var environmentResourceName = take('${compactPrefix}-cae-${suffix}', 32)
var containerAppName = take('${compactPrefix}-control-${suffix}', 32)
var cosmosDatabaseName = 'remote-agent-control'
var cosmosContainerName = 'hosts'
var cosmosDiscoveryContainerName = 'host-discovery'

resource logAnalyticsWorkspace 'Microsoft.OperationalInsights/workspaces@2023-09-01' = {
  name: workspaceName
  location: location
  tags: tags
  properties: {
    sku: {
      name: 'PerGB2018'
    }
    retentionInDays: 30
    features: {
      enableLogAccessUsingOnlyResourcePermissions: true
    }
  }
}

resource applicationInsights 'Microsoft.Insights/components@2020-02-02' = {
  name: appInsightsName
  location: location
  tags: tags
  kind: 'web'
  properties: {
    Application_Type: 'web'
    WorkspaceResourceId: logAnalyticsWorkspace.id
    // This negative property must remain false to keep client IP masking enabled.
    DisableIpMasking: false
    publicNetworkAccessForIngestion: 'Enabled'
    publicNetworkAccessForQuery: 'Enabled'
  }
}

resource containerRegistry 'Microsoft.ContainerRegistry/registries@2023-07-01' = {
  name: acrName
  location: location
  tags: tags
  sku: {
    name: 'Basic'
  }
  properties: {
    adminUserEnabled: false
    publicNetworkAccess: 'Enabled'
    zoneRedundancy: 'Disabled'
    policies: {
      quarantinePolicy: {
        status: 'disabled'
      }
      trustPolicy: {
        type: 'Notary'
        status: 'disabled'
      }
    }
  }
}

resource apiIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: identityName
  location: location
  tags: tags
}

resource cosmosAccount 'Microsoft.DocumentDB/databaseAccounts@2024-11-15' = {
  name: cosmosName
  location: location
  tags: tags
  kind: 'GlobalDocumentDB'
  properties: {
    databaseAccountOfferType: 'Standard'
    disableLocalAuth: true
    publicNetworkAccess: 'Enabled'
    minimalTlsVersion: 'Tls12'
    disableKeyBasedMetadataWriteAccess: true
    enableAutomaticFailover: false
    consistencyPolicy: {
      defaultConsistencyLevel: 'Session'
    }
    locations: [
      {
        locationName: location
        failoverPriority: 0
        isZoneRedundant: false
      }
    ]
    capabilities: [
      {
        name: 'EnableServerless'
      }
    ]
    backupPolicy: {
      type: 'Continuous'
    }
  }
}

resource cosmosDatabase 'Microsoft.DocumentDB/databaseAccounts/sqlDatabases@2024-11-15' = {
  parent: cosmosAccount
  name: cosmosDatabaseName
  properties: {
    resource: {
      id: cosmosDatabaseName
    }
  }
}

resource cosmosContainer 'Microsoft.DocumentDB/databaseAccounts/sqlDatabases/containers@2024-11-15' = {
  parent: cosmosDatabase
  name: cosmosContainerName
  properties: {
    resource: {
      id: cosmosContainerName
      partitionKey: {
        paths: [
          '/hostId'
        ]
        kind: 'Hash'
        version: 2
      }
      defaultTtl: -1
      indexingPolicy: {
        indexingMode: 'consistent'
        automatic: true
        includedPaths: [
          {
            path: '/*'
          }
        ]
      }
    }
  }
}

resource cosmosDiscoveryContainer 'Microsoft.DocumentDB/databaseAccounts/sqlDatabases/containers@2024-11-15' = {
  parent: cosmosDatabase
  name: cosmosDiscoveryContainerName
  properties: {
    resource: {
      id: cosmosDiscoveryContainerName
      partitionKey: {
        paths: [
          '/principalId'
        ]
        kind: 'Hash'
        version: 2
      }
      indexingPolicy: {
        indexingMode: 'consistent'
        automatic: true
        includedPaths: [
          {
            path: '/*'
          }
        ]
      }
    }
  }
}

resource webPubSub 'Microsoft.SignalRService/webPubSub@2024-03-01' = {
  name: webPubSubName
  location: location
  tags: tags
  sku: {
    name: webPubSubSkuName
    tier: startsWith(webPubSubSkuName, 'Standard') ? 'Standard' : 'Free'
    capacity: 1
  }
  properties: {
    disableLocalAuth: true
    disableAadAuth: false
    publicNetworkAccess: 'Enabled'
    tls: {
      clientCertEnabled: false
    }
  }
}

resource containerAppEnvironment 'Microsoft.App/managedEnvironments@2025-01-01' = {
  name: environmentResourceName
  location: location
  tags: tags
  properties: {
    appLogsConfiguration: {
      destination: 'log-analytics'
      logAnalyticsConfiguration: {
        customerId: logAnalyticsWorkspace.properties.customerId
        sharedKey: logAnalyticsWorkspace.listKeys().primarySharedKey
      }
    }
  }
}

resource containerApp 'Microsoft.App/containerApps@2025-01-01' = {
  name: containerAppName
  location: location
  tags: union(tags, {
    'azd-service-name': 'remote-agent-control'
  })
  identity: {
    type: 'UserAssigned'
    userAssignedIdentities: {
      '${apiIdentity.id}': {}
    }
  }
  properties: {
    managedEnvironmentId: containerAppEnvironment.id
    configuration: {
      activeRevisionsMode: 'Single'
      ingress: {
        external: true
        targetPort: 8080
        transport: 'http'
        allowInsecure: false
      }
      registries: [
        {
          server: containerRegistry.properties.loginServer
          identity: apiIdentity.id
        }
      ]
    }
    template: {
      containers: [
        {
          name: 'remote-agent-control'
          image: containerImage
          env: [
            {
              name: 'ASPNETCORE_ENVIRONMENT'
              value: 'Production'
            }
            {
              name: 'AZURE_CLIENT_ID'
              value: apiIdentity.properties.clientId
            }
            {
              name: 'APPLICATIONINSIGHTS_CONNECTION_STRING'
              value: applicationInsights.properties.ConnectionString
            }
            {
              name: 'RemoteAgentControl__AzureAd__TenantId'
              value: tenantId
            }
            {
              name: 'RemoteAgentControl__AzureAd__Audience'
              value: apiAudience
            }
            {
              name: 'RemoteAgentControl__AzureAd__RequiredScope'
              value: apiScope
            }
            {
              name: 'RemoteAgentControl__CosmosEndpoint'
              value: cosmosAccount.properties.documentEndpoint
            }
            {
              name: 'RemoteAgentControl__CosmosDatabaseName'
              value: cosmosDatabaseName
            }
            {
              name: 'RemoteAgentControl__CosmosContainerName'
              value: cosmosContainerName
            }
            {
              name: 'RemoteAgentControl__CosmosDiscoveryContainerName'
              value: cosmosDiscoveryContainerName
            }
            {
              name: 'RemoteAgentControl__WebPubSubEndpoint'
              value: 'https://${webPubSub.name}.webpubsub.azure.com'
            }
            {
              name: 'RemoteAgentControl__WebPubSubHubName'
              value: 'remote-agent'
            }
          ]
          resources: {
            cpu: json('0.5')
            memory: '1Gi'
          }
          probes: [
            {
              type: 'Liveness'
              httpGet: {
                path: '/health/live'
                port: 8080
              }
              initialDelaySeconds: 10
              periodSeconds: 15
            }
            {
              type: 'Readiness'
              httpGet: {
                path: '/health/ready'
                port: 8080
              }
              initialDelaySeconds: 10
              periodSeconds: 15
            }
          ]
        }
      ]
      scale: {
        minReplicas: minReplicas
        maxReplicas: 3
      }
    }
  }
  dependsOn: [
    acrPullAssignment
    cosmosDataContributorAssignment
    webPubSubOwnerAssignment
  ]
}

resource acrPullAssignment 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: containerRegistry
  name: guid(containerRegistry.id, apiIdentity.id, 'AcrPull')
  properties: {
    principalId: apiIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '7f951dda-4ed3-4680-a7ca-43fe172d538d')
  }
}

resource webPubSubOwnerAssignment 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: webPubSub
  name: guid(webPubSub.id, apiIdentity.id, 'WebPubSubServiceOwner')
  properties: {
    principalId: apiIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '12cf5a90-567b-43ae-8102-96cf46c7d9b4')
  }
}

resource cosmosDataContributorAssignment 'Microsoft.DocumentDB/databaseAccounts/sqlRoleAssignments@2024-11-15' = {
  parent: cosmosAccount
  name: guid(cosmosAccount.id, apiIdentity.id, 'CosmosDataContributor')
  properties: {
    principalId: apiIdentity.properties.principalId
    roleDefinitionId: '${cosmosAccount.id}/sqlRoleDefinitions/00000000-0000-0000-0000-000000000002'
    scope: '/'
  }
}

output AZURE_CONTAINER_APP_NAME string = containerApp.name
output AZURE_CONTAINER_APP_URL string = 'https://${containerApp.properties.configuration.ingress.fqdn}'
output AZURE_CONTAINER_REGISTRY_ENDPOINT string = containerRegistry.properties.loginServer
output REMOTE_AGENT_CONTROL_API_URL string = 'https://${containerApp.properties.configuration.ingress.fqdn}'
output REMOTE_AGENT_CONTROL_COSMOS_ENDPOINT string = cosmosAccount.properties.documentEndpoint
output REMOTE_AGENT_CONTROL_WEBPUBSUB_ENDPOINT string = 'https://${webPubSub.name}.webpubsub.azure.com'
output REMOTE_AGENT_CONTROL_MANAGED_IDENTITY_CLIENT_ID string = apiIdentity.properties.clientId
