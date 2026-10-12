{{/* Copyright (c) Microsoft Corporation.
Licensed under the MIT License. */}}

{{- define "kars.agentMesh.entraEnv" -}}
{{- $sidecar := .Values.entraSidecar | default dict -}}
{{- $auth := $sidecar.authConfig | default dict -}}
{{- if eq (default "Anonymous" $auth.meshAuthBackend) "EntraAgentIdentity" -}}
  {{- if not $sidecar.enabled -}}
    {{- fail "managed AgentMesh with EntraAgentIdentity requires entraSidecar.enabled=true and reviewed authConfig values" -}}
  {{- end -}}
  {{- include "kars.authConfig.bootstrap" . -}}
  {{- $env := include "kars.authConfig.env" . | fromJson -}}
  {{- $guid := "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}" -}}
  {{- $tenant := get $env "AzureAd__TenantId" -}}
  {{- if not (regexMatch (printf "^%s$" $guid) $tenant) -}}
    {{- fail "managed AgentMesh Entra verification requires a GUID tenant.tenantId" -}}
  {{- end -}}
  {{- if not (regexMatch "^https://login[.]microsoftonline[.]com/?$" (lower (get $env "AzureAd__Instance"))) -}}
    {{- fail "managed AgentMesh Entra verification requires tenant.authorityHost=https://login.microsoftonline.com/ (the official verifier requires the public tenant v2 issuer)" -}}
  {{- end -}}
  {{/* Use the effective sidecar projection, including case-insensitive explicit
       AgentMesh overrides, requestAppToken defaults and trimmed fallback scopes. */}}
  {{- $scopes := list -}}
  {{- $appToken := "" -}}
  {{- range $name, $value := $env -}}
    {{- if eq (lower $name) "downstreamapis__agentmesh__requestapptoken" -}}
      {{- $appToken = $value -}}
    {{- end -}}
    {{- if regexMatch "^downstreamapis__agentmesh__scopes__[0-9]+$" (lower $name) -}}
      {{- $scopes = append $scopes $value -}}
    {{- end -}}
  {{- end -}}
  {{- if ne $appToken "true" -}}
    {{- fail "managed AgentMesh Entra verification requires effective AgentMesh requestAppToken=true; delegated tokens are unsupported" -}}
  {{- end -}}
  {{- if ne (len $scopes) 1 -}}
    {{- fail "managed AgentMesh Entra verification requires exactly one effective AgentMesh scope: GUID/.default or api://GUID/.default" -}}
  {{- end -}}
  {{- $scope := first $scopes -}}
  {{- if not (regexMatch (printf "^(api://)?%s/[.]default$" $guid) $scope) -}}
    {{- fail "managed AgentMesh Entra verification requires an unambiguous v2 resource scope: GUID/.default or api://GUID/.default; configure the actual resource application (requestedAccessTokenVersion=2) and per-agent grants; do not infer its ID from blueprintClientId" -}}
  {{- end -}}
  {{/* A v2 JWT audience is the resource app GUID, not its OAuth scope or URI. */}}
- name: AGENTMESH_ENTRA_AUDIENCE
  value: {{ $scope | trimPrefix "api://" | trimSuffix "/.default" | lower | quote }}
- name: AGENTMESH_ENTRA_TENANT_ID
  value: {{ $tenant | lower | quote }}
{{- end -}}
{{- end -}}
