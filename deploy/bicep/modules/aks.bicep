// AzureClaw - AKS Module
// Deploys AKS cluster with Azure Container Linux node pools

@description('AKS cluster name')
param name string

@description('Azure region')
param location string

@description('Node count for sandbox pool')
param nodeCount int

@description('VM size for nodes')
param vmSize string

@description('Enable confidential computing')
param enableConfidential bool

@description('Enable FIPS')
param enableFips bool

@description('Log Analytics workspace ID for Container Insights')
param logAnalyticsWorkspaceId string

@description('ACR resource ID for image pull')
param acrId string

@description('Key Vault name for CSI driver')
param keyVaultName string

resource aks 'Microsoft.ContainerService/managedClusters@2024-09-01' = {
  name: name
  location: location
  identity: {
    type: 'SystemAssigned'
  }
  properties: {
    dnsPrefix: name
    kubernetesVersion: '1.31'
    networkProfile: {
      networkPlugin: 'azure'
      networkPolicy: 'cilium'
      networkDataplane: 'cilium'
    }
    agentPoolProfiles: [
      {
        name: 'system'
        count: 2
        vmSize: 'Standard_D2s_v5'
        osType: 'Linux'
        osSKU: 'AzureLinux'
        mode: 'System'
        enableFIPS: enableFips
      }
      {
        name: 'clawpool'
        count: nodeCount
        vmSize: vmSize
        osType: 'Linux'
        osSKU: 'AzureLinux'       // AzureContainerLinux when GA; AzureLinux (v4) as fallback
        mode: 'User'
        enableFIPS: enableFips
        enableEncryptionAtHost: true
        nodeTaints: [
          'azureclaw.azure.com/sandbox=true:NoSchedule'
        ]
        nodeLabels: {
          'azureclaw.azure.com/pool': 'sandbox'
        }
        kubeletConfig: {
          seccompDefault: true
        }
      }
    ]
    addonProfiles: {
      omsagent: {
        enabled: true
        config: {
          logAnalyticsWorkspaceResourceID: logAnalyticsWorkspaceId
        }
      }
      azureKeyvaultSecretsProvider: {
        enabled: true
        config: {
          enableSecretRotation: 'true'
          rotationPollInterval: '2m'
        }
      }
      azurepolicy: {
        enabled: true
      }
    }
    securityProfile: {
      defender: {
        securityMonitoring: {
          enabled: true
        }
      }
      workloadIdentity: {
        enabled: true
      }
    }
    oidcIssuerProfile: {
      enabled: true
    }
    autoUpgradeProfile: {
      upgradeChannel: 'stable'
      nodeOSUpgradeChannel: 'SecurityPatch'
    }
  }
}

// Grant AKS pull access to ACR
resource acrPull 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(aks.id, acrId, 'acrpull')
  scope: resourceGroup()
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '7f951dda-4ed3-4680-a7ca-43fe172d538d')  // AcrPull
    principalId: aks.properties.identityProfile.kubeletidentity.objectId
    principalType: 'ServicePrincipal'
  }
}

output clusterName string = aks.name
output clusterFqdn string = aks.properties.fqdn
output kubeletIdentityObjectId string = aks.properties.identityProfile.kubeletidentity.objectId
