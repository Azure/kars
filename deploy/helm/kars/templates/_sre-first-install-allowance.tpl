{{/* Copyright (c) Microsoft Corporation.
Licensed under the MIT License. */}}
{{/* Carry only the consumed, still-authorized cold-install receipt across Helm
     adoption. Never recreate a revoked catalog entry or invent a new allowance. */}}
{{- define "kars.sre.firstInstallAllowances" -}}
{{- $result := deepCopy (.desired | default list) -}}
{{- $binding := dig "spec" "firstInstallFunding" dict .registration -}}
{{- $ledger := dig "status" "ledger" dict .account -}}
{{- $receipt := $ledger.unboundedTokens | default dict -}}
{{- $stored := index (.catalog.data | default dict) "unbounded-token-allowances.json" | default "[]" -}}
{{- $live := $stored | fromJsonArray -}}
{{- $context := . -}}
{{- range $entry := $live -}}
{{- with $context -}}
{{- if eq (dig "root" "resource" "name" "" $entry) "sre" -}}
{{- if or (not $binding) (not $receipt) (ne ($entry | toJson) ($receipt | toJson))
  (not .intent.immutable) .intent.metadata.deletionTimestamp .catalog.metadata.deletionTimestamp
  (ne (dig "metadata" "annotations" "meta.helm.sh/release-name" "" .catalog) .release)
  (ne (dig "metadata" "annotations" "meta.helm.sh/release-namespace" "" .catalog) .namespace)
  (ne (dig "metadata" "annotations" "meta.helm.sh/release-name" "" .intent) .release)
  (ne (dig "metadata" "annotations" "meta.helm.sh/release-namespace" "" .intent) .namespace)
  (ne ($binding.intent.uid | default "") (.intent.metadata.uid | default "missing"))
  (ne ($binding.intent.digest | default "") (index .intent.data "intent.json" | sha256sum))
  (ne ((index .intent.data "intent.json" | fromJson).funding | toJson) (dict "unboundedTokens" true | toJson))
  (ne ($binding.grant.unboundedTokens | default false) true)
  (hasKey $binding.grant "expiresAt") (hasKey $binding.grant.limits "tokens")
  (ne ($entry.id | default "") (printf "first-install-%s" $binding.intent.digest))
  (ne ($entry.configUid | default "") (.catalog.metadata.uid | default "missing"))
  (ne ($entry.accountUid | default "") (.account.metadata.uid | default "missing"))
  (ne ($entry.root | toJson) ($binding.grant.root | toJson))
  (ne ($entry.originalLimits | toJson) ($binding.grant.limits | toJson))
  (ne ($ledger.root | toJson) ($binding.grant.root | toJson))
  (ne (dig "spec" "root" dict .account | toJson) ($binding.grant.root | toJson))
  (ne (dig "spec" "limits" dict .account | toJson) ($binding.grant.limits | toJson))
  (ne (dig "spec" "sandbox" "uid" "" .registration) (dig "metadata" "uid" "missing" .source))
  (ne (dig "spec" "inferenceBudgetRef" "account" "uid" "" .source) (.account.metadata.uid | default "missing"))
  (ne (dig "spec" "inferenceBudgetRef" "root" dict .source | toJson) ($binding.grant.root | toJson))
  .source.metadata.deletionTimestamp .registration.metadata.deletionTimestamp .account.metadata.deletionTimestamp -}}
{{- fail "Cold unbounded funding must retain its exact live intent, source, account and consumed catalog receipt" -}}
{{- end -}}
{{- $present := false -}}
{{- range $configured := $result -}}
{{- if or (eq $configured.id $entry.id) (eq $configured.accountUid $entry.accountUid) -}}
{{- if ne ($configured | toJson) ($entry | toJson) -}}{{- fail "Cannot replace the cold-install unbounded receipt" -}}{{- end -}}
{{- $present = true -}}
{{- end -}}
{{- end -}}
{{- if not $present -}}{{- $result = append $result $entry -}}{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- $encoded := $result | toJson -}}
{{- if eq $encoded ($live | toJson) -}}
{{- $stored -}}
{{- else -}}
{{- $encoded -}}
{{- end -}}
{{- end -}}
