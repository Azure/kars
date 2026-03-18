// AzureClaw - ACR Module

@description('ACR name (must be globally unique, alphanumeric)')
param name string

@description('Azure region')
param location string

resource acr 'Microsoft.ContainerRegistry/registries@2023-07-01' = {
  name: name
  location: location
  sku: {
    name: 'Premium'   // Required for geo-replication and content trust
  }
  properties: {
    adminUserEnabled: false
    policies: {
      trustPolicy: {
        type: 'Notary'
        status: 'enabled'
      }
      quarantinePolicy: {
        status: 'enabled'
      }
    }
  }
}

output acrId string = acr.id
output loginServer string = acr.properties.loginServer
