// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//
// Controller managed identity for kars Entra Agent ID trust.
//
// Resource-group-scoped module (`scope: resourceGroup`) so the parent
// subscription-scope deployment can create the RG first. The MI's
// `principalId` flows back to the parent as the subject of the
// blueprint's federated identity credential.

@description('Managed identity name. Convention: kars-<cluster>-controller-mi.')
param name string

@description('Azure region.')
param location string

resource mi 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: name
  location: location
}

output clientId string = mi.properties.clientId
output principalId string = mi.properties.principalId
output resourceId string = mi.id
