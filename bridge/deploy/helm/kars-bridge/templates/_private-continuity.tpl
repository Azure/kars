{{/* Copyright (c) Microsoft Corporation. Licensed under the MIT License. */}}
{{/* Same canonical encoding as Core continuity and private-activation.ts. */}}
{{- define "kars-bridge.privateCanonical" -}}
{{- $encoded := toRawJson . -}}
{{- range regexFindAll `\\\\|\\u2028|\\u2029|[^\\]+|\\.` $encoded -1 -}}
{{- if eq . `\u2028` -}}{{ "\u2028" }}{{- else if eq . `\u2029` -}}{{ "\u2029" }}{{- else -}}{{ . }}{{- end -}}
{{- end -}}
{{- end -}}
{{/* Add only documented API defaults, never merge arbitrary live settings into
     requested intent. Unknown fields and non-default values remain comparable. */}}
{{- define "kars-bridge.privateDefaults" -}}
{{- range $key, $value := .defaults -}}
{{- if not (hasKey $.object $key) -}}{{- $_ := set $.object $key $value -}}{{- end -}}
{{- end -}}
{{- end -}}
{{/* Exact integer milli-units for common API quantity spellings. Unsupported
     precision/ranges retain their original spelling rather than rounding. */}}
{{- define "kars-bridge.privateQuantity" -}}
{{- $text := toString . -}}
{{- $result := dict "literal" $text -}}
{{- if regexMatch "^[0-9]{1,12}([.][0-9]{1,3})?(m|Ki|Mi|Gi|Ti|k|M|G|T)?$" $text -}}
{{- $suffix := regexFind "[a-zA-Z]+$" $text -}}
{{- $number := trimSuffix $suffix $text -}}
{{- $parts := splitList "." $number -}}
{{- $fraction := "000" -}}{{- if eq (len $parts) 2 -}}{{- $fraction = substr 0 3 (printf "%s000" (last $parts)) -}}{{- end -}}
{{- $whole := regexReplaceAll "^0+" (first $parts) "" | default "0" | int64 -}}
{{- $decimal := regexReplaceAll "^0+" $fraction "" | default "0" | int64 -}}
{{- $milli := add (mul $whole 1000) $decimal -}}
{{- $units := dict "" 1 "m" 1 "k" 1000 "M" 1000000 "G" 1000000000 "T" 1000000000000 "Ki" 1024 "Mi" 1048576 "Gi" 1073741824 "Ti" 1099511627776 -}}
{{- $unit := int64 (get $units $suffix) -}}
{{- if eq $suffix "m" -}}
{{- if eq (mod $milli 1000) 0 -}}{{- $result = dict "milli" (div $milli 1000) -}}{{- end -}}
{{- else if le $milli (div 9000000000000000000 $unit) -}}
{{- $result = dict "milli" (mul $milli $unit) -}}
{{- end -}}
{{- end -}}
{{- toJson $result -}}
{{- end -}}
{{- define "kars-bridge.privateExecution" -}}
{{- $spec := deepCopy . -}}
{{- include "kars-bridge.privateDefaults" (dict "object" $spec "defaults" (dict "replicas" 1 "revisionHistoryLimit" 10 "progressDeadlineSeconds" 600 "strategy" (dict))) -}}
{{- include "kars-bridge.privateDefaults" (dict "object" $spec.strategy "defaults" (dict "type" "RollingUpdate")) -}}
{{- if eq $spec.strategy.type "RollingUpdate" -}}
{{- include "kars-bridge.privateDefaults" (dict "object" $spec.strategy "defaults" (dict "rollingUpdate" (dict))) -}}
{{- include "kars-bridge.privateDefaults" (dict "object" $spec.strategy.rollingUpdate "defaults" (dict "maxSurge" "25%" "maxUnavailable" "25%")) -}}
{{- end -}}
{{- $meta := $spec.template.metadata -}}
{{- if eq (get $meta "creationTimestamp" | toJson) "null" -}}{{- $_ := unset $meta "creationTimestamp" -}}{{- end -}}
{{- if hasKey $meta "annotations" -}}
{{- $_ := unset $meta.annotations "kars.azure.com/private-epoch" -}}
{{- if empty $meta.annotations -}}{{- $_ := unset $meta "annotations" -}}{{- end -}}
{{- end -}}
{{- $pod := $spec.template.spec -}}
{{- include "kars-bridge.privateDefaults" (dict "object" $pod "defaults" (dict "restartPolicy" "Always" "dnsPolicy" "ClusterFirst" "schedulerName" "default-scheduler" "terminationGracePeriodSeconds" 30 "securityContext" (dict))) -}}
{{- if and (hasKey $pod "serviceAccount") (eq $pod.serviceAccount ($pod.serviceAccountName | default "")) -}}{{- $_ := unset $pod "serviceAccount" -}}{{- end -}}
{{- range $container := concat ($pod.containers | default list) ($pod.initContainers | default list) -}}
{{- include "kars-bridge.privateDefaults" (dict "object" $container "defaults" (dict "terminationMessagePath" "/dev/termination-log" "terminationMessagePolicy" "File" "resources" (dict))) -}}
{{- range $category := list "requests" "limits" -}}
{{- range $resource, $quantity := get $container.resources $category | default dict -}}
{{- $_ := set (get $container.resources $category) $resource (include "kars-bridge.privateQuantity" $quantity) -}}
{{- end -}}
{{- end -}}
{{- range $container.ports | default list -}}{{- include "kars-bridge.privateDefaults" (dict "object" . "defaults" (dict "protocol" "TCP")) -}}{{- end -}}
{{- range $probe := list "readinessProbe" "livenessProbe" "startupProbe" -}}
{{- if hasKey $container $probe -}}
{{- $value := get $container $probe -}}
{{- include "kars-bridge.privateDefaults" (dict "object" $value "defaults" (dict "timeoutSeconds" 1 "periodSeconds" 10 "successThreshold" 1 "failureThreshold" 3 "initialDelaySeconds" 0)) -}}
{{- if hasKey $value "httpGet" -}}{{- include "kars-bridge.privateDefaults" (dict "object" $value.httpGet "defaults" (dict "scheme" "HTTP")) -}}{{- end -}}
{{- end -}}
{{- end -}}
{{- range $container.env | default list -}}
{{- if not (hasKey . "valueFrom") -}}{{- include "kars-bridge.privateDefaults" (dict "object" . "defaults" (dict "value" "")) -}}{{- end -}}
{{- end -}}
{{- end -}}
{{- include "kars-bridge.privateCanonical" $spec -}}
{{- end -}}

{{- define "kars-bridge.privateIdentity" -}}
{{- $identity := .identity -}}
{{- if or (not (kindIs "map" $identity)) (ne (len $identity) 3) -}}{{- fail .error -}}{{- end -}}
{{- range $key := list "name" "uid" "resourceVersion" -}}
{{- $value := get $identity $key -}}
{{- if or (not (kindIs "string" $value)) (empty $value) (gt (len $value) 253) -}}{{- fail $.error -}}{{- end -}}
{{- end -}}
{{- end -}}
{{/* The operator supplies replica intent from the completed migration receipt,
     NOT from Qualified namespace metadata. The live RV is an operation fence. */}}
{{- define "kars-bridge.privateConsumerGuard" -}}
{{- $root := .root -}}{{- $desired := .desired -}}{{- $fence := .fence -}}
{{- $namespace := $desired.metadata.namespace -}}{{- $name := $desired.metadata.name -}}
{{- $error := printf "KARS_PRIVATE_CONSUMER_UPGRADE_BLOCKED: %s/%s requires a completed migration, exact reviewed no-op fence and unchanged qualified execution" $namespace $name -}}
{{- $live := lookup "apps/v1" "Deployment" $namespace $name | default dict -}}
{{- $scope := lookup "v1" "Namespace" "" $namespace | default dict -}}
{{- $fields := dig "metadata" "annotations" dict $scope -}}
{{- $prefix := "kars.azure.com/private-" -}}
{{- $uid := dig "metadata" "uid" "" $live -}}
{{- $boundConsumer := or (not (empty $fence)) (hasKey (dig "spec" "template" "metadata" "annotations" dict $live) (print $prefix "epoch")) (hasKey $fields (print $prefix "parent-" $uid)) -}}
{{- $protected := $boundConsumer -}}
{{- range $key, $value := $fields -}}
{{- if hasPrefix $prefix $key -}}{{- $protected = true -}}{{- end -}}
{{- end -}}
{{- if and $live (not $scope) -}}{{- fail "KARS_PRIVATE_CONSUMER_CHECK_UNAVAILABLE: Existing Bridge Deployment namespace could not be reviewed" -}}{{- end -}}
{{- if $protected -}}
{{- if or (not $root.Release.IsUpgrade) (empty $live) (empty $scope) (empty $uid)
  (dig "metadata" "deletionTimestamp" "" $live) (dig "metadata" "deletionTimestamp" "" $scope)
  (ne (dig "metadata" "annotations" "meta.helm.sh/release-name" "" $live) $root.Release.Name)
  (ne (dig "metadata" "annotations" "meta.helm.sh/release-namespace" "" $live) $root.Release.Namespace)
  (ne (dig "metadata" "labels" "app.kubernetes.io/managed-by" "" $live) "Helm")
  (ne (get $fields (print $prefix "enabled")) "true") (ne (get $fields (print $prefix "state")) "Qualified")
  (get $fields (print $prefix "core-publication")) -}}{{- fail $error -}}{{- end -}}
{{- $rootName := get $fields (print $prefix "root-namespace") -}}
{{- $authority := lookup "v1" "Namespace" "" $rootName | default dict -}}
{{- $authorityFields := dig "metadata" "annotations" dict $authority -}}
{{- $raw := get $authorityFields (print $prefix "root-retirement") -}}
{{- if or (empty $raw) (gt (len $raw) 131072) -}}{{- fail $error -}}{{- end -}}
{{- $proof := fromJson $raw -}}
{{- if or (hasKey $proof "Error") (ne (len $proof) 4) (ne (toJson $proof.version) "2") (not (kindIs "string" $proof.retirement)) -}}{{- fail $error -}}{{- end -}}
{{- $retirement := fromJson $proof.retirement -}}{{- $activation := $proof.activation -}}
{{- if or (ne (include "kars-bridge.privateCanonical" $proof) $raw)
  (ne ($proof.retirement | sha256sum) $proof.retirementDigest)
  (ne (include "kars-bridge.privateCanonical" $retirement) $proof.retirement)
  (ne $retirement.phase "restoring") (ne (toJson $retirement.version) "1")
  (ne $activation.phase "qualified") (ne $activation.contract "kars.azure.com/private-consumption/v1")
  (ne $activation.root.namespace.name $rootName)
  (ne $activation.root.namespace.uid (dig "metadata" "uid" "" $authority))
  (dig "metadata" "deletionTimestamp" "" $authority)
  (ne (get $authorityFields (print $prefix "state")) "Qualified")
  (get $authorityFields (print $prefix "core-publication")) -}}{{- fail $error -}}{{- end -}}
{{- if or (gt (len (omit $activation "contract" "bundleRevision" "profile" "controllerUids" "root" "namespaces" "phase")) 0)
  (gt (len (omit $activation.root "namespace" "account" "deployment" "templateDigest" "replicaIntent" "budgetTls")) 0)
  (gt (len (omit $retirement "version" "attempt" "binding" "replicaIntent" "originalVersion" "pauseRoot" "phase" "captured" "exposedKeys" "baseline")) 0)
  (not (regexMatch "^[a-f0-9]{64}$" ($retirement.attempt | default "")))
  (not (kindIs "bool" $retirement.pauseRoot)) (empty $retirement.originalVersion)
  (not (kindIs "map" $retirement.captured)) (not (kindIs "slice" $retirement.exposedKeys))
  (not (regexMatch "^[a-f0-9]{64}$" ($activation.bundleRevision | default "")))
  (not (has $activation.profile (list "service-accounts" "kcm-certificate")))
  (ne (toJson $retirement.replicaIntent) (toJson $activation.root.replicaIntent))
  (not (kindIs "slice" $activation.namespaces)) (empty $activation.namespaces) (gt (len $activation.namespaces) 64) -}}{{- fail $error -}}{{- end -}}
{{- range $identity := list $activation.root.namespace $activation.root.account $activation.root.deployment -}}
{{- include "kars-bridge.privateIdentity" (dict "identity" $identity "error" $error) -}}
{{- end -}}
{{- if not (regexMatch "^(0|[1-9][0-9]*)$" (toJson $activation.root.replicaIntent)) -}}{{- fail $error -}}{{- end -}}
{{/* Reconstruct the existing retirementBinding, without inventing replica
     intent for companions. Also reject changed execution elsewhere in the
     retained activation; Qualified text alone is not completion evidence. */}}
{{- $bindings := dict -}}{{- $identities := dict -}}{{- $scopeFields := dict -}}{{- $hasRoot := false -}}
{{- $apis := dict "Deployment" "apps/v1" "ReplicaSet" "apps/v1" "StatefulSet" "apps/v1" "DaemonSet" "apps/v1" "ReplicationController" "v1" "Pod" "v1" "Job" "batch/v1" "CronJob" "batch/v1" -}}
{{- range $review := $activation.namespaces -}}
{{- include "kars-bridge.privateIdentity" (dict "identity" $review.namespace "error" $error) -}}
{{- if or (gt (len (omit $review "namespace" "consumers" "epoch")) 0) (hasKey $bindings $review.namespace.name)
  (not (regexMatch "^[a-f0-9]{64}$" $review.epoch)) (gt (len $review.consumers) 64) -}}{{- fail $error -}}{{- end -}}
{{- $reviewedNamespace := lookup "v1" "Namespace" "" $review.namespace.name | default dict -}}
{{- $reviewFields := dig "metadata" "annotations" dict $reviewedNamespace -}}
{{- if or (ne (dig "metadata" "uid" "" $reviewedNamespace) $review.namespace.uid) (dig "metadata" "deletionTimestamp" "" $reviewedNamespace)
  (ne (get $reviewFields (print $prefix "namespace-uid")) $review.namespace.uid)
  (ne (get $reviewFields (print $prefix "enabled")) "true") (ne (get $reviewFields (print $prefix "state")) "Qualified")
  (ne (get $reviewFields (print $prefix "epoch")) $review.epoch) (get $reviewFields (print $prefix "core-publication")) -}}{{- fail $error -}}{{- end -}}
{{- $_ := set $scopeFields $review.namespace.name $reviewFields -}}
{{- $ordered := dict -}}{{- $reviewParents := dict -}}
{{- range $consumer := $review.consumers -}}
{{- $_ := set $reviewParents (print $prefix "parent-" $consumer.object.uid) true -}}
{{- $api := get $apis $consumer.kind -}}
{{- include "kars-bridge.privateIdentity" (dict "identity" $consumer.object "error" $error) -}}
{{- if or (gt (len (omit $consumer "kind" "object" "templateDigest")) 0) (empty $api)
  (hasKey $identities $consumer.object.uid) -}}{{- fail $error -}}{{- end -}}
{{- $_ := set $identities $consumer.object.uid true -}}
{{- $object := lookup $api $consumer.kind $review.namespace.name $consumer.object.name | default dict -}}
{{- if or (ne (dig "metadata" "uid" "" $object) $consumer.object.uid) (dig "metadata" "deletionTimestamp" "" $object)
  (ne (get $reviewFields (print $prefix "parent-" $consumer.object.uid)) $review.epoch) -}}{{- fail $error -}}{{- end -}}
{{- $template := dig "spec" "template" dict $object | deepCopy -}}
{{- if eq $consumer.kind "CronJob" -}}{{- $template = dig "spec" "jobTemplate" "spec" "template" dict $object | deepCopy -}}{{- end -}}
{{- if eq $consumer.kind "Pod" -}}{{- $template = dict "metadata" (dict "labels" (dig "metadata" "labels" dict $object) "annotations" (dig "metadata" "annotations" dict $object)) "spec" $object.spec | deepCopy -}}{{- end -}}
{{- $annotations := dig "metadata" "annotations" dict $template -}}
{{- if and (hasKey $annotations (print $prefix "epoch")) (ne (get $annotations (print $prefix "epoch")) $review.epoch) -}}{{- fail $error -}}{{- end -}}
{{- $_ := unset $annotations (print $prefix "epoch") -}}
{{- if empty $annotations -}}{{- $_ := unset $template.metadata "annotations" -}}{{- end -}}
{{- if ne (include "kars-bridge.privateCanonical" $template | sha256sum) $consumer.templateDigest -}}
{{- include "kars-bridge.privateControllerRollout" (dict "scope" $review "consumer" $consumer "live" $object "namespace" $reviewedNamespace "error" $error) -}}
{{- end -}}
{{- if eq $consumer.object.uid $activation.root.deployment.uid -}}
{{- $hasRoot = true -}}
{{- if or (ne $consumer.kind "Deployment") (ne $consumer.object.name $activation.root.deployment.name)
  (ne $review.namespace.name $rootName) (ne $consumer.templateDigest $activation.root.templateDigest)
  (ne (dig "spec" "replicas" -1 $object | toJson) (toJson $activation.root.replicaIntent)) -}}{{- fail $error -}}{{- end -}}
{{- end -}}
{{- $bound := dict "kind" $consumer.kind "object" (pick $consumer.object "name" "uid") "templateDigest" $consumer.templateDigest -}}
{{- $_ := set $ordered (include "kars-bridge.privateCanonical" $bound) $bound -}}
{{- end -}}
{{- range $key, $value := $reviewFields -}}
{{- if and (hasPrefix (print $prefix "parent-") $key) (not (hasKey $reviewParents $key)) -}}{{- fail $error -}}{{- end -}}
{{- end -}}
{{- $consumers := list -}}{{- range $key := keys $ordered | sortAlpha -}}{{- $consumers = append $consumers (get $ordered $key) -}}{{- end -}}
{{- $_ := set $bindings $review.namespace.name (dict "namespace" (pick $review.namespace "name" "uid") "consumers" $consumers) -}}
{{- end -}}
{{- $namespaces := list -}}{{- range $key := keys $bindings | sortAlpha -}}{{- $namespaces = append $namespaces (get $bindings $key) -}}{{- end -}}
{{- $budget := fromJson "null" -}}
{{- with $activation.root.budgetTls -}}{{- $budget = dict "namespace" (pick .namespace "name" "uid") "secret" (pick .secret "name" "uid") -}}{{- end -}}
{{- $boundRoot := dict "namespace" (pick $activation.root.namespace "name" "uid") "account" (pick $activation.root.account "name" "uid") "deployment" (pick $activation.root.deployment "name" "uid") "templateDigest" $activation.root.templateDigest "replicaIntent" $activation.root.replicaIntent "budget" $budget -}}
{{- $binding := dict "contract" $activation.contract "bundleRevision" $activation.bundleRevision "profile" $activation.profile "controllerUids" $activation.controllerUids "root" $boundRoot "namespaces" $namespaces -}}
{{- if or (not $hasRoot) (ne (include "kars-bridge.privateCanonical" $binding | sha256sum) $retirement.binding) -}}{{- fail $error -}}{{- end -}}
{{- $account := lookup "v1" "ServiceAccount" $rootName $activation.root.account.name | default dict -}}
{{- if or (ne (dig "metadata" "uid" "" $account) $activation.root.account.uid) (dig "metadata" "deletionTimestamp" "" $account)
  (ne (len $retirement.captured) (len $activation.namespaces)) -}}{{- fail $error -}}{{- end -}}
{{- range $review := $activation.namespaces -}}
{{- if not (kindIs "slice" (get $retirement.captured $review.namespace.uid)) -}}{{- fail $error -}}{{- end -}}
{{- end -}}
{{- with $activation.root.budgetTls -}}
{{- $budgetNamespace := lookup "v1" "Namespace" "" .namespace.name | default dict -}}
{{- $secret := lookup "v1" "Secret" .namespace.name .secret.name | default dict -}}
{{- if or (ne (dig "metadata" "uid" "" $budgetNamespace) .namespace.uid) (dig "metadata" "deletionTimestamp" "" $budgetNamespace)
  (ne (dig "metadata" "uid" "" $secret) .secret.uid) (ne (dig "metadata" "resourceVersion" "" $secret) .secret.resourceVersion)
  (dig "metadata" "deletionTimestamp" "" $secret) (ne (get $secret "type") "kubernetes.io/tls")
  (ne (len ($retirement.baseline | default dict)) 3) (ne (dig "baseline" "secretUid" "" $retirement) .secret.uid)
  (eq (dig "baseline" "resourceVersion" "" $retirement) .secret.resourceVersion)
  (not (has (dig "baseline" "keyDigest" "" $retirement) $retirement.exposedKeys)) (has .keyDigest $retirement.exposedKeys) -}}{{- fail $error -}}{{- end -}}
{{- else -}}
{{- if or (hasKey $retirement "baseline") (not (empty $retirement.exposedKeys)) -}}{{- fail $error -}}{{- end -}}
{{- end -}}
{{- $expected := dict "enabled" "true" "state" "Qualified" "namespace-uid" $scope.metadata.uid
  "root-namespace" $activation.root.namespace.name "root-namespace-uid" $activation.root.namespace.uid
  "root-account" $activation.root.account.name "root-uid" $activation.root.account.uid
  "root-user" (printf "system:serviceaccount:%s:%s" $rootName $activation.root.account.name)
  "root-deployment" $activation.root.deployment.name "root-deployment-uid" $activation.root.deployment.uid
  "root-template-digest" $activation.root.templateDigest "bundle-revision" $activation.bundleRevision "profile" $activation.profile -}}
{{- with $activation.root.budgetTls -}}
{{- $expected = mergeOverwrite $expected (dict "budget-namespace" .namespace.name "budget-namespace-uid" .namespace.uid
  "budget-tls-name" .secret.name "budget-tls-uid" .secret.uid "budget-tls-version" .secret.resourceVersion "budget-key" .keyDigest
  "budget-qualified-bundle" $activation.bundleRevision "budget-qualified-key" .keyDigest "budget-qualified-secret" .secret.uid
  "budget-rotation-bundle" "" "budget-before-key" "") -}}
{{- end -}}
{{- range $name, $id := $activation.controllerUids -}}{{- $_ := set $expected (printf "%s-uid" $name) $id -}}{{- end -}}
{{- range $key, $value := $expected -}}
{{- if ne (get $fields (print $prefix $key)) $value -}}{{- fail $error -}}{{- end -}}
{{- range $reviewName, $reviewFields := $scopeFields -}}
{{- if and (ne $key "namespace-uid") (ne (get $reviewFields (print $prefix $key)) $value) -}}{{- fail $error -}}{{- end -}}
{{- end -}}
{{- end -}}
{{- $found := 0 -}}
{{- range $review := $activation.namespaces -}}
{{- if eq $review.namespace.name $namespace -}}
{{- if or (ne $review.namespace.uid $scope.metadata.uid) (not (regexMatch "^[a-f0-9]{64}$" $review.epoch)) (ne (get $fields (print $prefix "epoch")) $review.epoch) -}}{{- fail $error -}}{{- end -}}
{{- $parents := dict -}}
{{- range $consumer := $review.consumers -}}
{{- $_ := set $parents (print $prefix "parent-" $consumer.object.uid) $review.epoch -}}
{{- if and (eq $consumer.kind "Deployment") (eq $consumer.object.name $name) -}}
{{- $boundConsumer = true -}}
{{- $found = add1 $found -}}
{{- $template := deepCopy $live.spec.template -}}
{{- $annotations := dig "metadata" "annotations" dict $template -}}
{{- if and (hasKey $annotations (print $prefix "epoch")) (ne (get $annotations (print $prefix "epoch")) $review.epoch) -}}{{- fail $error -}}{{- end -}}
{{- $_ := unset $annotations (print $prefix "epoch") -}}
{{- if empty $annotations -}}{{- $_ := unset $template.metadata "annotations" -}}{{- end -}}
{{- if or (ne $consumer.object.uid $uid) (ne (include "kars-bridge.privateCanonical" $template | sha256sum) $consumer.templateDigest) -}}{{- fail $error -}}{{- end -}}
{{- end -}}
{{- end -}}
{{- range $key, $value := $parents -}}{{- if ne (get $fields $key) $value -}}{{- fail $error -}}{{- end -}}{{- end -}}
{{- range $key, $value := $fields -}}{{- if and (hasPrefix (print $prefix "parent-") $key) (not (hasKey $parents $key)) -}}{{- fail $error -}}{{- end -}}{{- end -}}
{{- end -}}
{{- end -}}
{{- if $boundConsumer -}}
{{- if or (not (kindIs "map" $fence)) (ne (len $fence) 3)
  (not (kindIs "string" $fence.uid)) (ne $fence.uid $uid)
  (not (kindIs "string" $fence.resourceVersion)) (empty $fence.resourceVersion)
  (ne $fence.resourceVersion (dig "metadata" "resourceVersion" "" $live))
  (not (regexMatch "^(0|[1-9][0-9]*)$" (toJson $fence.replicas))) (gt (int64 $fence.replicas) 2147483647)
  (ne (toJson $fence.replicas) (dig "spec" "replicas" -1 $live | toJson))
  (ne (toJson $fence.replicas) (toJson $desired.spec.replicas))
  (ne (int $found) 1) (ne (include "kars-bridge.privateExecution" $live.spec) (include "kars-bridge.privateExecution" $desired.spec)) -}}{{- fail $error -}}{{- end -}}
{{/* Keep every live execution field, including API defaults and epoch. Helm's
     normal patch carries UID/RV; replacement/force/rollback are not supported. */}}
{{- toYaml (dict "apiVersion" "apps/v1" "kind" "Deployment" "metadata" (omit $live.metadata "managedFields" "creationTimestamp" "generation") "spec" $live.spec) -}}
{{- else -}}
{{- toYaml $desired -}}
{{- end -}}
{{- else -}}
{{- toYaml $desired -}}
{{- end -}}
{{- end -}}
