{{/* Copyright (c) Microsoft Corporation.
Licensed under the MIT License. */}}
{{- define "kars.missionDispatcher.validate" -}}
{{- $mission := .Values.missionDispatcher | default dict -}}
{{- if and (hasKey $mission "enabled") (not (kindIs "bool" $mission.enabled)) -}}
{{- fail "missionDispatcher.enabled must be a boolean" -}}
{{- end -}}
{{- if $mission.enabled -}}
{{- if ne ($mission.authMode | default "") "development" -}}
{{- fail "missionDispatcher requires explicit authMode=development; Entra dispatch is not yet supported" -}}
{{- end -}}
{{- $image := $mission.image | default dict -}}
{{- if not $image.repository -}}{{- fail "missionDispatcher.image.repository is required" -}}{{- end -}}
{{- if and $image.digest (not (regexMatch "^sha256:[a-f0-9]{64}$" $image.digest)) -}}
{{- fail "missionDispatcher.image.digest must be a sha256 digest" -}}
{{- end -}}
{{- end -}}
{{- end -}}
