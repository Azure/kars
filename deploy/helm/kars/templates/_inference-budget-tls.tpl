{{/* Copyright (c) Microsoft Corporation.
Licensed under the MIT License. */}}
{{- define "kars.inferenceBudget.tls.validate" -}}
{{- $budget := .Values.inferenceBudget | default dict -}}
{{- $tls := $budget.tls | default dict -}}
{{- $certificate := $tls.certificate | default "" -}}
{{- $privateKey := $tls.privateKey | default "" -}}
{{- if or $certificate $privateKey -}}
{{- if not ($budget.enabled | default false) -}}
{{- fail "inferenceBudget.tls requires inferenceBudget.enabled" -}}
{{- end -}}
{{- $name := required "inferenceBudget.tlsSecretName is required" $budget.tlsSecretName -}}
{{- if or (gt (len $name) 253) (not (regexMatch "^[a-z0-9]([-a-z0-9]*[a-z0-9])?([.][a-z0-9]([-a-z0-9]*[a-z0-9])?)*$" $name)) -}}
{{- fail "inferenceBudget.tlsSecretName must be a Secret name" -}}
{{- end -}}
{{- if or (not (contains "-----BEGIN CERTIFICATE-----" $certificate)) (contains "PRIVATE KEY" $certificate) -}}
{{- fail "inferenceBudget.tls.certificate must contain public PEM certificates" -}}
{{- end -}}
{{- if not (regexMatch "-----BEGIN (RSA |EC )?PRIVATE KEY-----" $privateKey) -}}
{{- fail "inferenceBudget.tls.privateKey must contain an unencrypted PEM private key" -}}
{{- end -}}
{{- $existing := lookup "v1" "Secret" .Release.Namespace $name -}}
{{- if $existing -}}
{{- $annotations := $existing.metadata.annotations | default dict -}}
{{- $labels := $existing.metadata.labels | default dict -}}
{{- if or $existing.metadata.deletionTimestamp (ne (get $annotations "meta.helm.sh/release-name") .Release.Name) (ne (get $annotations "meta.helm.sh/release-namespace") .Release.Namespace) (ne (get $labels "app.kubernetes.io/managed-by") "Helm") (ne (get $annotations "kars.azure.com/inference-budget-tls") "v1") (ne $existing.type "kubernetes.io/tls") -}}
{{- fail "inferenceBudget.tls cannot adopt or replace a Secret outside this release's budget TLS ownership" -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
