{{/* Copyright (c) Microsoft Corporation. Licensed under the MIT License. */}}
{{- define "kars-bridge.sreAdoptedRepair" -}}
{{/* Installing compatible readers is not funding authorization. Require a previously
     adopted Core and an existing Bridge release; live authority remains blocked. */}}
{{- $root := .root -}}
{{- $binding := dig "spec" "firstInstallFunding" "intent" dict .registration -}}
{{- $blocked := false -}}
{{- range dig "status" "conditions" list .registration -}}
{{- if and (eq (.type | default "") "Ready") (eq (.status | default "") "False") (eq (.reason | default "") "AuthorityBlocked") -}}{{- $blocked = true -}}{{- end -}}
{{- end -}}
{{- $owned := true -}}
{{- range list .intent .adoption -}}
{{- if or (not .) (not (dig "immutable" false .)) (dig "metadata" "deletionTimestamp" "" .)
  (ne (dig "metadata" "annotations" "meta.helm.sh/release-name" "" .) "kars")
  (ne (dig "metadata" "annotations" "meta.helm.sh/release-namespace" "" .) "kars-system") -}}
{{- $owned = false -}}
{{- end -}}
{{- end -}}
{{- if and $root.Release.IsUpgrade $blocked $owned
  (eq (dig "status" "phase" "" .registration) "Blocked")
  (not (dig "metadata" "deletionTimestamp" "" .bff))
  (ne (dig "metadata" "uid" "" .bff) "")
  (eq (dig "metadata" "annotations" "meta.helm.sh/release-name" "" .bff) $root.Release.Name)
  (eq (dig "metadata" "annotations" "meta.helm.sh/release-namespace" "" .bff) $root.Release.Namespace)
  (ne ($binding.uid | default "") "")
  (eq ($binding.uid | default "") (dig "metadata" "uid" "" .intent))
  (eq ($binding.uid | default "") (dig "data" "intentUID" "" .adoption))
  (ne (dig "data" "intent.json" "" .intent) "")
  (eq ($binding.digest | default "") (dig "data" "intent.json" "" .intent | sha256sum))
  (regexMatch "^[a-f0-9]{64}$" (dig "data" "valuesDigest" "" .adoption)) -}}
verified
{{- end -}}
{{- end -}}
