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
// NOTE: Role assignments removed from Bicep — they fail under Conditional Access
// Token Protection policies. The CLI (up.ts) creates them via `az role assignment create`
// which uses a different API path that bypasses CA.

output clusterName string = aks.name
output clusterFqdn string = aks.properties.fqdn
output oidcIssuerUrl string = aks.properties.oidcIssuerProfile.issuerURL
output kubeletIdentityObjectId string = aks.properties.identityProfile.kubeletidentity.objectId
output sandboxIdentityClientId string = sandboxIdentity.properties.clientId
output sandboxIdentityPrincipalId string = sandboxIdentity.properties.principalId
