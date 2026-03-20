targetScope = 'resourceGroup'
param sandboxWiPrincipalId string
param projectMiPrincipalId string
param foundryAccountName string = 'lsb-azureai'

// Azure AI User role ID — has Microsoft.CognitiveServices/* wildcard data actions
var azureAiUser = '53ca6127-db72-4b80-b1b0-d745d6d5456d'

resource aiServices 'Microsoft.CognitiveServices/accounts@2024-10-01' existing = {
  name: foundryAccountName
}

// 1. Sandbox WI → Azure AI User on the AI Services resource (pod API access)
resource sandboxRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(sandboxWiPrincipalId)) {
  name: guid(aiServices.id, sandboxWiPrincipalId, 'azure-ai-user')
  scope: aiServices
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', azureAiUser)
    principalId: sandboxWiPrincipalId
    principalType: 'ServicePrincipal'
  }
}

// 2. Project MI → Azure AI User on the resource group (Memory Store internal model calls)
resource projectMiRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(projectMiPrincipalId)) {
  name: guid(resourceGroup().id, projectMiPrincipalId, 'azure-ai-user')
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', azureAiUser)
    principalId: projectMiPrincipalId
    principalType: 'ServicePrincipal'
  }
}
