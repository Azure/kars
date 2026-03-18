// AzureClaw Infrastructure - Main Bicep Template
// Deploys: AKS (Azure Container Linux) + ACR + Key Vault + Azure OpenAI + Monitor

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

@description('Enable confidential computing (AMD SEV-SNP)')
param enableConfidential bool = false

@description('Enable FIPS 140-2 validated crypto')
param enableFips bool = false

@description('Azure OpenAI model deployment name')
param openAiModelName string = 'gpt-4.1'

@description('Azure OpenAI model version')
param openAiModelVersion string = '2025-04-14'

// ─── Azure Container Registry ───────────────────────────────────────────────

module acr 'modules/acr.bicep' = {
  name: '${baseName}-acr'
  params: {
    name: '${baseName}acr'
    location: location
  }
}

// ─── Azure Key Vault ────────────────────────────────────────────────────────

module keyVault 'modules/keyvault.bicep' = {
  name: '${baseName}-kv'
  params: {
    name: '${baseName}-kv'
    location: location
  }
}

// ─── Azure OpenAI ───────────────────────────────────────────────────────────

module openAi 'modules/openai.bicep' = {
  name: '${baseName}-aoai'
  params: {
    name: '${baseName}-aoai'
    location: location
    modelName: openAiModelName
    modelVersion: openAiModelVersion
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
    enableConfidential: enableConfidential
    enableFips: enableFips
    logAnalyticsWorkspaceId: monitor.outputs.logAnalyticsWorkspaceId
    acrId: acr.outputs.acrId
    keyVaultName: keyVault.outputs.keyVaultName
  }
}

// ─── Outputs ─────────────────────────────────────────────────────────────────

output aksClusterName string = aks.outputs.clusterName
output acrLoginServer string = acr.outputs.loginServer
output keyVaultUri string = keyVault.outputs.vaultUri
output openAiEndpoint string = openAi.outputs.endpoint
output logAnalyticsWorkspaceId string = monitor.outputs.logAnalyticsWorkspaceId
