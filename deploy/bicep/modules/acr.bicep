// kars - ACR Module

@description('ACR name (must be globally unique, alphanumeric)')
param name string

@description('Azure region')
param location string

@description('Authorized IP CIDR ranges (empty = allow all)')
param authorizedIpRanges array = []

resource acr 'Microsoft.ContainerRegistry/registries@2023-07-01' = {
  name: name
  location: location
  sku: {
    name: 'Premium'   // Required for geo-replication, content trust, and firewall rules
  }
  properties: {
    adminUserEnabled: false
    publicNetworkAccess: 'Enabled'  // Needed for ACR build tasks; firewall restricts access
    networkRuleBypassOptions: 'AzureServices'  // Allow AKS kubelet + ACR tasks
    networkRuleSet: {
      defaultAction: empty(authorizedIpRanges) ? 'Allow' : 'Deny'
      ipRules: [for cidr in authorizedIpRanges: {
        action: 'Allow'
        value: cidr
      }]
    }
    policies: {
      trustPolicy: {
        type: 'Notary'
        status: 'enabled'
      }
      quarantinePolicy: {
        status: 'disabled'  // Requires external scanner integration; enable when ready
      }
    }
  }
}

output acrId string = acr.id
output loginServer string = acr.properties.loginServer
