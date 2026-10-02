{{/* Copyright (c) Microsoft Corporation.
Licensed under the MIT License. */}}
{{- define "kars.inferenceCredentials.validate" -}}
{{- $credentials := .Values.inferenceRouter.azure.openai.credentials | default dict -}}
{{- $apiKey := $credentials.apiKey | default "" -}}
{{- $existing := $credentials.existingSecret | default "" -}}
{{- if or $apiKey $existing -}}
{{- if and $apiKey $existing -}}
{{- fail "inferenceRouter.azure.openai.credentials: choose apiKey or existingSecret, not both" -}}
{{- end -}}
{{- if and $apiKey (not (trim $apiKey)) -}}
{{- fail "inferenceRouter.azure.openai.credentials.apiKey must not be blank" -}}
{{- end -}}
{{- if and $existing (or (gt (len $existing) 253) (not (regexMatch "^[a-z0-9]([-a-z0-9]*[a-z0-9])?([.][a-z0-9]([-a-z0-9]*[a-z0-9])?)*$" $existing))) -}}
{{- fail "inferenceRouter.azure.openai.credentials.existingSecret must be a Secret name" -}}
{{- end -}}
{{- if eq $existing "kars-inference-bootstrap" -}}
{{- fail "inferenceRouter.azure.openai.credentials.existingSecret cannot use the Helm-managed bootstrap Secret name" -}}
{{- end -}}
{{- $key := $credentials.key | default "api-key" -}}
{{- if or (gt (len $key) 253) (not (regexMatch "^[A-Za-z0-9._-]+$" $key)) -}}
{{- fail "inferenceRouter.azure.openai.credentials.key must be a Secret data key" -}}
{{- end -}}
{{- if not (or (.Values.foundry.endpoint | default "") .Values.inferenceRouter.azure.openai.endpoint) -}}
{{- fail "API-key inference requires foundry.endpoint or inferenceRouter.azure.openai.endpoint" -}}
{{- end -}}
{{- range .Values.controller.extraEnv -}}
{{- if eq .name "AZURE_OPENAI_API_KEY" -}}
{{- fail "API-key inference credentials cannot also be set through controller.extraEnv" -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
