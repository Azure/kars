// AzureClaw - AKS Module
// Deploys AKS cluster with Azure Linux node pools
// Governance: Azure Policy add-on (no Defender for Cloud required)

@description('AKS cluster name')
param name string

@description('Azure region')
param location string

@description('Node count for sandbox pool')
param nodeCount int

@description('VM size for nodes')
param vmSize string

@description('Enable FIPS')
param enableFips bool

@description('Log Analytics workspace ID for Container Insights')
param logAnalyticsWorkspaceId string

@description('ACR resource ID for image pull')
param acrId string

@description('Key Vault name for CSI driver')
param keyVaultName string

@description('Azure OpenAI account resource ID (for RBAC)')
param openAiAccountId string

@description('Enable Kata Containers (pod sandboxing) for confidential isolation')
param enableKata bool = false

@description('Authorized IP CIDR ranges for API server (empty = no restriction)')
param authorizedIpRanges array = []

resource aks 'Microsoft.ContainerService/managedClusters@2024-09-01' = {
  name: name
  location: location
  identity: {
    type: 'SystemAssigned'
  }
  properties: {
    dnsPrefix: name
    kubernetesVersion: '1.33'
    apiServerAccessProfile: !empty(authorizedIpRanges) ? {
      authorizedIPRanges: authorizedIpRanges
    } : null
    networkProfile: {
      networkPlugin: 'azure'
      networkPolicy: 'cilium'
      networkDataplane: 'cilium'
    }
    agentPoolProfiles: concat([
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
      }
    ], enableKata ? [
      {
        name: 'katapool'
        count: nodeCount
        vmSize: 'Standard_D4s_v3'  // Must support nested virtualization
        osType: 'Linux'
        osSKU: 'AzureLinux'
        mode: 'User'
        enableFIPS: enableFips
        enableEncryptionAtHost: true
        workloadRuntime: 'KataMshvVmIsolation'
        nodeTaints: [
          'azureclaw.azure.com/sandbox=true:NoSchedule'
        ]
        nodeLabels: {
          'azureclaw.azure.com/pool': 'sandbox-kata'
        }
      }
    ] : [])
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
        enabled: true       // Azure Policy for Kubernetes — governance without Defender
      }
    }
    securityProfile: {
      workloadIdentity: {
        enabled: true
      }
    }
    oidcIssuerProfile: {
      enabled: true
    }
    autoUpgradeProfile: {
      upgradeChannel: 'stable'
      nodeOSUpgradeChannel: enableKata ? 'NodeImage' : 'SecurityPatch'  // SecurityPatch not supported on Kata distro
    }
  }
}

// ─── Workload Identity for sandbox inference router ─────────────────────────
// The inference router uses this identity to auth to Azure OpenAI (Entra ID, no API keys)

resource sandboxIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-sandbox-wi'
  location: location
}

resource federatedCredential 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = {
  parent: sandboxIdentity
  name: 'azureclaw-sandbox'
  properties: {
    issuer: aks.properties.oidcIssuerProfile.issuerURL
    subject: 'system:serviceaccount:azureclaw-system:azureclaw-sandbox'
    audiences: [
      'api://AzureADTokenExchange'
    ]
  }
}

// ─── RBAC Role Assignments ──────────────────────────────────────────────────

// Grant AKS kubelet pull access to ACR
resource acrPull 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(aks.id, acrId, 'acrpull')
  scope: resourceGroup()
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '7f951dda-4ed3-4680-a7ca-43fe172d538d')  // AcrPull
    principalId: aks.properties.identityProfile.kubeletidentity.objectId
    principalType: 'ServicePrincipal'
  }
}

// Grant sandbox identity "Cognitive Services OpenAI User" on the AOAI resource (only when AOAI is deployed)
resource openAiAccount 'Microsoft.CognitiveServices/accounts@2024-10-01' existing = if (!empty(openAiAccountId)) {
  name: last(split(openAiAccountId, '/'))
}

resource openAiRoleAssignment 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(openAiAccountId)) {
  name: guid(sandboxIdentity.id, openAiAccountId, 'openai-user')
  scope: openAiAccount
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '5e0bd9bd-7b93-4f28-af87-19fc36ad61bd')  // Cognitive Services OpenAI User
    principalId: sandboxIdentity.properties.principalId
    principalType: 'ServicePrincipal'
  }
}

// Grant sandbox identity "Key Vault Secrets User" on Key Vault
resource keyVaultRef 'Microsoft.KeyVault/vaults@2023-07-01' existing = {
  name: keyVaultName
}

resource kvRoleAssignment 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(sandboxIdentity.id, keyVaultRef.id, 'kv-secrets-user')
  scope: keyVaultRef
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '4633458b-17de-408a-b874-0445c86b69e6')  // Key Vault Secrets User
    principalId: sandboxIdentity.properties.principalId
    principalType: 'ServicePrincipal'
  }
}

output clusterName string = aks.name
output clusterFqdn string = aks.properties.fqdn
output oidcIssuerUrl string = aks.properties.oidcIssuerProfile.issuerURL
output kubeletIdentityObjectId string = aks.properties.identityProfile.kubeletidentity.objectId
output sandboxIdentityClientId string = sandboxIdentity.properties.clientId
output sandboxIdentityPrincipalId string = sandboxIdentity.properties.principalId
