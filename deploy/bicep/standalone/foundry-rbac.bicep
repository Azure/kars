// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//
// Grants `Azure AI User` on a Foundry resource to the kars blueprint
// service principal. Per-sandbox agent identities derive from the
// blueprint and INHERIT this role assignment — there is no need to
// add a separate role assignment for every new sandbox.
//
// This is the "blueprint-SP-scoped RBAC" pattern recommended by
// Microsoft's research/design-patterns documentation (see
// `docs/architecture/entra-agent-id/05-rbac-design.md`). Two
// rationales:
//
// 1. Operational simplicity: one role assignment covers an
//    unbounded number of agent identities. Rotating sandboxes
//    don't generate role-assignment churn on Foundry.
// 2. Audit clarity: Foundry sees the blueprint SP as the consenting
//    party; per-sandbox attribution still flows via the token's
//    `appid` / `azp` claim (the agent identity's appId), which kars'
//    inference-router pins on the sidecar's
//    `?AgentIdentity=<appId>` parameter.
//
// Scope: the Foundry resource group (default) or the specific
// Foundry resource. Resource-group scope is the common case
// because Foundry projects + AI Services accounts both live in
// the same RG and the parent role-assignment subsumes them.
//
// Pre-requisite: the blueprint SP must already exist. Get its
// object ID from `KarsAuthConfig/default.spec.agentId.blueprintObjectId`
// or from the output of `agent-id-trust.bicep`.

targetScope = 'resourceGroup'

@description('Blueprint service principal object ID (NOT appId). Read from `KarsAuthConfig.spec.agentId.blueprintObjectId` or the `blueprintSpObjectId` output of `agent-id-trust.bicep`.')
param blueprintSpObjectId string

@description('Optional: explicit Foundry resource name to scope the role assignment to a single AI Services account. When empty, the role is granted at the resource-group scope so all Foundry resources in this RG inherit.')
param foundryResourceName string = ''

@description('Role to grant. Defaults to `Azure AI User` (53ca6127-db72-4b80-b1b0-d745d6d5456d), the least-privilege role for inference + memory. Use `Cognitive Services User` for legacy AI Services compatibility.')
param roleDefinitionId string = '53ca6127-db72-4b80-b1b0-d745d6d5456d'

// `Azure AI User` role guid (Foundry-aware least-privilege role).
var roleDefId = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roleDefinitionId)

// Stable GUID derived from (RG, blueprint SP, role) so re-deployments
// upsert idempotently without producing duplicate role assignments.
var rgScopedAssignmentName = guid(resourceGroup().id, blueprintSpObjectId, roleDefinitionId)
var resourceScopedAssignmentName = guid(resourceGroup().id, foundryResourceName, blueprintSpObjectId, roleDefinitionId)

// RG-scope assignment (default — covers every Foundry account in
// this resource group).
resource rgScopedAssignment 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (empty(foundryResourceName)) {
  name: rgScopedAssignmentName
  scope: resourceGroup()
  properties: {
    principalId: blueprintSpObjectId
    principalType: 'ServicePrincipal'
    roleDefinitionId: roleDefId
    description: 'kars blueprint SP — inherited by all derived agent identities (rg-scoped)'
  }
}

// Resource-scope assignment (when operator explicitly targets one
// Foundry account; least-privilege tightening for sensitive tenants).
resource foundryAccount 'Microsoft.CognitiveServices/accounts@2024-10-01' existing = if (!empty(foundryResourceName)) {
  name: foundryResourceName
}

resource resourceScopedAssignment 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(foundryResourceName)) {
  name: resourceScopedAssignmentName
  scope: foundryAccount
  properties: {
    principalId: blueprintSpObjectId
    principalType: 'ServicePrincipal'
    roleDefinitionId: roleDefId
    description: 'kars blueprint SP — inherited by all derived agent identities (resource-scoped)'
  }
}

output assignedScope string = empty(foundryResourceName)
  ? resourceGroup().id
  : '${resourceGroup().id}/providers/Microsoft.CognitiveServices/accounts/${foundryResourceName}'
output assignmentName string = empty(foundryResourceName) ? rgScopedAssignmentName : resourceScopedAssignmentName
output roleDefinitionId string = roleDefinitionId
