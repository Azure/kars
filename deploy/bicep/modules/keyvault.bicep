// kars - Key Vault Module

@description('Key Vault name')
param name string

@description('Azure region')
param location string

@description('Recover soft-deleted vault')
param recover bool = false

resource keyVault 'Microsoft.KeyVault/vaults@2023-07-01' = {
  name: name
  location: location
  properties: {
    sku: {
      family: 'A'
      name: 'standard'
    }
    tenantId: subscription().tenantId
    enableRbacAuthorization: true
    enableSoftDelete: true
    softDeleteRetentionInDays: 30
    enablePurgeProtection: true
    createMode: recover ? 'recover' : 'default'
    networkAcls: {
      defaultAction: 'Deny'
      bypass: 'AzureServices'
    }
  }
}

output keyVaultName string = keyVault.name
output vaultUri string = keyVault.properties.vaultUri
