// AzureClaw Infrastructure - Main Bicep Template
// Deploys: AKS (Azure Linux) + ACR + Key Vault + Azure OpenAI + Monitor

targetScope = 'resourceGroup'

@description('Base name for all resources')
param baseName string = 'azureclaw'

@description('Azure region for deployment')
param location string = resourceGroup().location

@description('AKS node count for sandbox pool')
@minValue(1)
@maxValue(100)
param nodeCount int = 3

@description('VM size for sandbox nodes')
param vmSize string = 'Standard_D4s_v5'

@description('Enable Kata VM isolation for confidential sandbox level')
param enableConfidential bool = false

@description('Enable Kata Containers for pod-level VM isolation')
param enableKata bool = false

@description('Enable FIPS 140-2 validated crypto')
param enableFips bool = false

@description('Authorized IP CIDR ranges for AKS API server and service firewalls (empty = no restriction)')
param authorizedIpRanges array = []

@description('Azure OpenAI model deployment name')
param openAiModelName string = 'gpt-4.1'

@description('Azure OpenAI model version')
param openAiModelVersion string = '2025-04-14'

@description('Deploy Azure OpenAI resource (set false when using --foundry-endpoint)')
param deployAoai bool = true

// ─── Azure Container Registry ───────────────────────────────────────────────

// ACR names must be alphanumeric — strip hyphens from baseName
var acrName = '${replace(baseName, '-', '')}acr'

module acr 'modules/acr.bicep' = {
  name: '${baseName}-acr'
  params: {
    name: acrName
    location: location
    authorizedIpRanges: authorizedIpRanges
  }
}

// ─── Azure Key Vault ────────────────────────────────────────────────────────

module keyVault 'modules/keyvault.bicep' = {
  name: '${baseName}-kv'
  params: {
    name: '${baseName}-kv'
    location: location
    recover: false
  }
}

// ─── Azure OpenAI (optional — skipped when using external Foundry endpoint) ─

module openAi 'modules/openai.bicep' = if (deployAoai) {
  name: '${baseName}-aoai'
  params: {
    name: '${baseName}-aoai'
    location: location
    modelName: openAiModelName
    modelVersion: openAiModelVersion
    authorizedIpRanges: authorizedIpRanges
    restore: false  // Set true only if AOAI was soft-deleted (not purged)
  }
}

// ─── Azure Monitor (Log Analytics + Application Insights) ───────────────────

module monitor 'modules/monitor.bicep' = {
  name: '${baseName}-monitor'
  params: {
    name: '${baseName}-monitor'
    location: location
  }
}

// ─── Azure Kubernetes Service ────────────────────────────────────────────────

module aks 'modules/aks.bicep' = {
  name: '${baseName}-aks'
  params: {
    name: '${baseName}-aks'
    location: location
    nodeCount: nodeCount
    vmSize: enableConfidential ? 'Standard_DC4as_v5' : vmSize
    enableFips: enableFips
    logAnalyticsWorkspaceId: monitor.outputs.logAnalyticsWorkspaceId
    acrId: acr.outputs.acrId
    keyVaultName: keyVault.outputs.keyVaultName
    openAiAccountId: deployAoai ? openAi.outputs.accountId : ''
    authorizedIpRanges: authorizedIpRanges
    enableKata: enableKata
  }
}

// ─── Outputs ─────────────────────────────────────────────────────────────────

output aksClusterName string = aks.outputs.clusterName
output acrLoginServer string = acr.outputs.loginServer
output keyVaultUri string = keyVault.outputs.vaultUri
output keyVaultName string = keyVault.outputs.keyVaultName
output openAiEndpoint string = deployAoai ? openAi.outputs.endpoint : ''
output logAnalyticsWorkspaceId string = monitor.outputs.logAnalyticsWorkspaceId
output sandboxIdentityClientId string = aks.outputs.sandboxIdentityClientId
