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

{{- define "kars.inferenceBudget.contracts.validate" -}}
{{- $budget := .Values.inferenceBudget | default dict -}}
{{- if not (kindIs "slice" $budget.contracts) -}}
{{- fail "inferenceBudget.contracts must be an explicit list" -}}
{{- end -}}
{{- if not $budget.contracts -}}
{{- $profile := (.Values.global | default dict).installationProfile | default "" -}}
{{- $funding := (.Values.sre | default dict).firstInstallFunding | default dict -}}
{{- if not (and (eq $profile "kind-beta") (eq ($budget.calendarPolicy | default "enforce") "record-only")
  (eq ($funding | toJson) (dict "unboundedTokens" true | toJson))) -}}
{{- fail "An empty inferenceBudget.contracts list requires kind-beta, record-only calendar policy and exactly unboundedTokens: true first-install funding; finite or monetary budgets require genuine provider contracts" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "kars.inferenceBudget.catalogData" -}}
{{- $encoded := .desired | toJson -}}
{{- if .current -}}
{{- $annotations := .current.metadata.annotations | default dict -}}
{{- $labels := .current.metadata.labels | default dict -}}
{{- if or .current.metadata.deletionTimestamp
    (ne (get $annotations "meta.helm.sh/release-name") .release)
    (ne (get $annotations "meta.helm.sh/release-namespace") .namespace)
    (ne (get $labels "app.kubernetes.io/managed-by") "Helm")
    (ne (get $annotations "kars.azure.com/inference-budget-contracts") "v1") -}}
{{- fail "Cannot reuse an inference budget catalog outside this release's ownership" -}}
{{- end -}}
{{- $stored := index (.current.data | default dict) "contracts.json" | default "" -}}
{{- if $stored -}}
{{- if eq ($stored | mustFromJson | toJson) $encoded -}}
{{- $encoded = $stored -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- $encoded -}}
{{- end -}}

{{- define "kars.inferenceBudget.tlsChecksum" -}}
{{- $checksum := dict "certificate" .identity.certificate "privateKey" .identity.privateKey | toJson | sha256sum -}}
{{- $data := (.existing | default dict).data | default dict -}}
{{- if and (eq (get $data "tls.crt") (.identity.certificate | b64enc))
    (eq (get $data "tls.key") (.identity.privateKey | b64enc)) .controller -}}
{{- range $object := list .existing .controller -}}
{{- $annotations := $object.metadata.annotations | default dict -}}
{{- if or (not $object.metadata.uid) $object.metadata.deletionTimestamp
    (ne (get $annotations "meta.helm.sh/release-name") $.release)
    (ne (get $annotations "meta.helm.sh/release-namespace") $.namespace)
    (ne (get ($object.metadata.labels | default dict) "app.kubernetes.io/managed-by") "Helm") -}}
{{- fail "Cannot retain the budget TLS checksum outside this release's live ownership" -}}
{{- end -}}
{{- end -}}
{{- $retained := dig "spec" "template" "metadata" "annotations" "checksum/inference-budget-tls" "" .controller -}}
{{- if $retained -}}
{{- if not (regexMatch "^[a-f0-9]{64}$" $retained) -}}
{{- fail "The retained budget TLS checksum is invalid" -}}
{{- end -}}
{{- $checksum = $retained -}}
{{- end -}}
{{- end -}}
{{- $checksum -}}
{{- end -}}
