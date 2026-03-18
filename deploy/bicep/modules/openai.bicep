// AzureClaw - Azure OpenAI Module

@description('Azure OpenAI account name')
param name string

@description('Azure region')
param location string

@description('Model deployment name')
param modelName string

@description('Model version')
param modelVersion string

@description('Authorized IP ranges (empty = allow all)')
param authorizedIpRanges array = []

resource openAi 'Microsoft.CognitiveServices/accounts@2024-10-01' = {
  name: name
  location: location
  kind: 'OpenAI'
  sku: {
    name: 'S0'
  }
  properties: {
    customSubDomainName: name
    publicNetworkAccess: 'Enabled'    // Needed for IP-based access; firewall restricts to authorizedIpRanges
    networkAcls: {
      defaultAction: empty(authorizedIpRanges) ? 'Allow' : 'Deny'
      ipRules: [for ip in authorizedIpRanges: {
        value: replace(ip, '/32', '')  // Cognitive Services rejects CIDR /32 notation
      }]
    }
    disableLocalAuth: true   // Force Entra ID auth only — no API keys
  }
}

resource modelDeployment 'Microsoft.CognitiveServices/accounts/deployments@2024-10-01' = {
  parent: openAi
  name: modelName
  sku: {
    name: 'Standard'
    capacity: 30
  }
  properties: {
    model: {
      format: 'OpenAI'
      name: modelName
      version: modelVersion
    }
  }
}

output endpoint string = openAi.properties.endpoint
output accountId string = openAi.id
