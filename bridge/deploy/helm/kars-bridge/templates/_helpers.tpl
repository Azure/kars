{{/* Copyright (c) Microsoft Corporation.
Licensed under the MIT License. */}}{{/* Common labels + names for the kars Bridge chart. */}}
{{- define "kars-bridge.labels" -}}
app.kubernetes.io/name: kars-bridge
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: kars
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
{{- end -}}

{{- define "kars-bridge.namespace" -}}
{{- .Values.namespace | default "kars-system" -}}
{{- end -}}

{{- define "kars-bridge.coreNamespace" -}}
{{- $core := .Values.core | default dict -}}
{{- $core.namespace | default "kars-system" -}}
{{- end -}}

{{- define "kars-bridge.serviceAccountName" -}}
{{- .Values.rbac.serviceAccountName | default "kars-bridge" -}}
{{- end -}}

{{/* Retained parent evidence also binds non-consuming consumers without a
     template epoch. Ordinary Helm updates must preserve that qualification. */}}
{{- define "kars-bridge.privateConsumerGuard" -}}
{{- $namespace := include "kars-bridge.namespace" .root -}}
{{- $live := lookup "apps/v1" "Deployment" $namespace .name | default dict -}}
{{- if $live -}}
{{- $uid := required "Existing Bridge Deployment lacks its API UID" (dig "metadata" "uid" "" $live) -}}
{{- $scope := lookup "v1" "Namespace" "" $namespace | default dict -}}
{{- if not $scope -}}
{{- fail "KARS_PRIVATE_CONSUMER_CHECK_UNAVAILABLE: Existing Bridge Deployment namespace could not be reviewed" -}}
{{- end -}}
{{- $fields := dig "metadata" "annotations" (dict) $scope -}}
{{- $annotations := dig "spec" "template" "metadata" "annotations" (dict) $live -}}
{{- if or (hasKey $annotations "kars.azure.com/private-epoch") (hasKey $fields (printf "kars.azure.com/private-parent-%s" $uid)) -}}
{{- fail (printf "KARS_PRIVATE_CONSUMER_UPGRADE_BLOCKED: %s/%s has retained private qualification. Preserve its template, epoch and namespace evidence; a reviewed retirement/requalification transaction is required before upgrading." $namespace .name) -}}
{{- end -}}
{{- end -}}
{{- end -}}
