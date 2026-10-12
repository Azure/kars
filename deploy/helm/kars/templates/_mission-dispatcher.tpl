{{/* Copyright (c) Microsoft Corporation.
Licensed under the MIT License. */}}
{{- define "kars.missionDispatcher.validate" -}}
{{- $mission := .Values.missionDispatcher | default dict -}}
{{- if and (hasKey $mission "enabled") (not (kindIs "bool" $mission.enabled)) -}}
{{- fail "missionDispatcher.enabled must be a boolean" -}}
{{- end -}}
{{- if $mission.enabled -}}
{{- if not (and (kindIs "string" $mission.authMode) (has $mission.authMode (list "development" "entra"))) -}}
{{- fail "missionDispatcher requires explicit authMode=development or authMode=entra" -}}
{{- end -}}
{{- if eq $mission.authMode "entra" -}}
{{- if not (and (kindIs "bool" .Values.entraSidecar.enabled) .Values.entraSidecar.enabled) -}}
{{- fail "missionDispatcher authMode=entra requires entraSidecar.enabled=true" -}}
{{- end -}}
{{- $entraEnabled := (.Values.azure.entraAuth | default dict).enabled -}}
{{- if not (and (kindIs "bool" $entraEnabled) $entraEnabled) -}}
{{- fail "missionDispatcher authMode=entra requires azure.entraAuth.enabled=true" -}}
{{- end -}}
{{- $s := include "kars.authConfig.spec" . | fromJson -}}
{{- if ne $s.meshAuthBackend "EntraAgentIdentity" -}}
{{- fail "missionDispatcher authMode=entra requires meshAuthBackend=EntraAgentIdentity" -}}
{{- end -}}
{{- $guid := "^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$" -}}
{{- range $name, $value := dict "tenantId" $s.tenant.tenantId "blueprintClientId" $s.agentId.blueprintClientId "managedIdentityClientId" $s.controller.managedIdentityClientId -}}
{{- if not (and (kindIs "string" $value) (regexMatch $guid $value)) -}}
{{- fail (printf "missionDispatcher authMode=entra requires %s to be a lowercase GUID in entraSidecar.authConfig" $name) -}}
{{- end -}}
{{- end -}}
{{- if not $s.agentId.sponsorUserObjectIds -}}
{{- fail "missionDispatcher authMode=entra requires nonempty sponsorUserObjectIds" -}}
{{- end -}}
{{- range $s.agentId.sponsorUserObjectIds -}}
{{- if not (and (kindIs "string" .) (regexMatch $guid .)) -}}
{{- fail "missionDispatcher authMode=entra requires sponsorUserObjectIds to contain lowercase GUIDs" -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- $image := $mission.image | default dict -}}
{{- if not $image.repository -}}{{- fail "missionDispatcher.image.repository is required" -}}{{- end -}}
{{- if and $image.digest (not (regexMatch "^sha256:[a-f0-9]{64}$" $image.digest)) -}}
{{- fail "missionDispatcher.image.digest must be a sha256 digest" -}}
{{- end -}}
{{- end -}}
{{- end -}}
