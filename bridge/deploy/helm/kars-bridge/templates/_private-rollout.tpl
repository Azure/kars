{{/* Copyright (c) Microsoft Corporation. Licensed under the MIT License. */}}
{{/* Verify the projector receipt used by private-activation-reconciliation.ts.
     Keep the original qualification digest; never bless live drift or RS history. */}}
{{- define "kars-bridge.privateControllerRollout" -}}
{{- $scope := .scope -}}{{- $consumer := .consumer -}}{{- $live := .live -}}{{- $namespace := .namespace -}}{{- $error := .error -}}
{{- $prefix := "kars.azure.com/private-" -}}
{{- $fields := dig "metadata" "annotations" dict $namespace -}}
{{- $raw := get $fields (print $prefix "reconciled-" $consumer.object.uid) -}}
{{- $workspace := get $fields "kars.azure.com/sandbox-namespace" -}}
{{- if or (ne $consumer.kind "Deployment") (ne ($live.kind | default "") "Deployment") (ne ($live.apiVersion | default "") "apps/v1")
  (ne (dig "metadata" "uid" "" $live) $consumer.object.uid) (ne (dig "metadata" "name" "" $live) $consumer.object.name)
  (ne (dig "metadata" "namespace" "" $live) $scope.namespace.name) (dig "metadata" "deletionTimestamp" "" $live)
  (ne (dig "metadata" "uid" "" $namespace) $scope.namespace.uid) (dig "metadata" "deletionTimestamp" "" $namespace)
  (empty $scope.epoch) (ne (get $fields (print $prefix "epoch")) $scope.epoch)
  (ne (get $fields (print $prefix "parent-" $consumer.object.uid)) $scope.epoch)
  (ne (dig "spec" "template" "metadata" "annotations" (print $prefix "epoch") "" $live) $scope.epoch)
  (ne (get $fields "kars.azure.com/namespace-claim-version") "v1")
  (ne (get $fields "kars.azure.com/sandbox-name") $consumer.object.name)
  (ne $scope.namespace.name (print "kars-" $consumer.object.name))
  (not (kindIs "string" $workspace)) (empty $workspace)
  (hasKey $fields "kars.azure.com/namespace-prestage") (dig "metadata" "ownerReferences" list $namespace)
  (not (kindIs "string" $raw)) (empty $raw) (gt (len $raw) 8192) -}}{{- fail $error -}}{{- end -}}
{{- $receipt := fromJson $raw -}}
{{- if or (hasKey $receipt "Error") (ne (len $receipt) 6)
  (gt (len (omit $receipt "epoch" "baseline" "structure" "template" "sandbox_uid" "generation")) 0) -}}{{- fail $error -}}{{- end -}}
{{- $sandbox := lookup "kars.azure.com/v1alpha1" "KarsSandbox" $workspace $consumer.object.name | default dict -}}
{{- $generation := dig "metadata" "generation" 0 $sandbox -}}
{{- $bound := dig "metadata" "annotations" dict $sandbox -}}
{{- if or (ne (dig "metadata" "name" "" $sandbox) $consumer.object.name)
  (ne (dig "metadata" "namespace" "" $sandbox) $workspace) (dig "metadata" "deletionTimestamp" "" $sandbox)
  (empty (dig "metadata" "uid" "" $sandbox))
  (ne (dig "metadata" "uid" "" $sandbox) (get $fields "kars.azure.com/sandbox-uid"))
  (and (hasKey $bound "kars.azure.com/namespace-uid") (ne (get $bound "kars.azure.com/namespace-uid") $scope.namespace.uid))
  (not (regexMatch "^[1-9][0-9]*$" (toJson $generation))) (gt (int64 $generation) 9007199254740991)
  (ne (toJson $receipt.generation) (toJson $generation)) (ne $receipt.sandbox_uid $sandbox.metadata.uid)
  (ne $receipt.epoch $scope.epoch) (ne $receipt.baseline $consumer.templateDigest) -}}{{- fail $error -}}{{- end -}}
{{- $template := deepCopy $live.spec.template -}}
{{- $_ := unset $template.metadata.annotations (print $prefix "epoch") -}}
{{- if empty $template.metadata.annotations -}}{{- $_ := unset $template.metadata "annotations" -}}{{- end -}}
{{- if ne $receipt.template (include "kars-bridge.privateCanonical" $template | sha256sum) -}}{{- fail $error -}}{{- end -}}
{{- $annotations := dig "metadata" "annotations" dict $template -}}
{{- range $key := list "kars.azure.com/inference-budget-ca-version" "kars.azure.com/inference-providers-version" "kars.azure.com/services-credential-version" "kars.azure.com/sre-private-credential-uid" "kars.azure.com/sre-tls-expiry" -}}
{{- if and (hasKey $annotations $key) (not (kindIs "string" (get $annotations $key))) -}}{{- fail $error -}}{{- end -}}
{{- $_ := unset $annotations $key -}}
{{- end -}}
{{- if empty $annotations -}}{{- $_ := unset $template.metadata "annotations" -}}{{- end -}}
{{- if ne $receipt.structure (include "kars-bridge.privateCanonical" $template | sha256sum) -}}{{- fail $error -}}{{- end -}}
{{- end -}}
