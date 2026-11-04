{{/* Copyright (c) Microsoft Corporation. Licensed under the MIT License. */}}
{{/* Match the existing CLI's canonical JSON, including literal Unicode line
     separators. Consume escaped backslash pairs first so literal \\u2028 text
     is not decoded. Go's toRawJson already preserves HTML characters. */}}
{{- define "kars.sre.privateCanonical" -}}
{{- $encoded := toRawJson . -}}
{{- if or (contains `\u2028` $encoded) (contains `\u2029` $encoded) -}}
{{- range regexFindAll `\\\\|\\u2028|\\u2029|[^\\]+|\\.` $encoded -1 -}}
{{- if eq . `\u2028` -}}{{ "\u2028" }}{{- else if eq . `\u2029` -}}{{ "\u2029" }}{{- else -}}{{ . }}{{- end -}}
{{- end -}}
{{- else -}}{{ $encoded }}{{- end -}}
{{- end -}}
{{- define "kars.sre.privateConsumerApi" -}}
{{- $api := get (dict "Deployment" "apps/v1" "ReplicaSet" "apps/v1" "StatefulSet" "apps/v1" "DaemonSet" "apps/v1" "ReplicationController" "v1" "Pod" "v1" "Job" "batch/v1" "CronJob" "batch/v1") . -}}
{{- if not $api -}}{{- fail "Private Core continuity consumer kind is unsupported" -}}{{- end -}}
{{- $api -}}
{{- end -}}
{{/* Match private-activation.ts: reviewed workloads without private authority
     are not epoch-stamped. Read material names from the maintained bundle. */}}
{{- define "kars.sre.consumesPrivateAuthority" -}}
{{- $pod := .pod -}}{{- $activation := .activation -}}{{- $definition := .definition -}}
{{- $secrets := $definition.secrets | default list -}}
{{- if eq (dig "root" "budgetTls" "namespace" "name" "" $activation) .namespace -}}
{{- $secrets = append $secrets $activation.root.budgetTls.secret.name -}}
{{- end -}}
{{- $private := false -}}{{- $token := false -}}
{{- range $volume := $pod.volumes | default list -}}
{{- if or (hasKey $volume "hostPath") (has (dig "secret" "secretName" "" $volume) $secrets)
  (has (dig "csi" "nodePublishSecretRef" "name" "" $volume) $secrets) -}}{{- $private = true -}}{{- end -}}
{{- range $source := dig "projected" "sources" list $volume -}}
{{- if and (hasKey $source "serviceAccountToken") (ne (get $source "serviceAccountToken" | toJson) "null") -}}{{- $token = true -}}{{- end -}}
{{- if or (has (dig "secret" "name" "" $source) $secrets)
  (has (dig "serviceAccountToken" "audience" "" $source) ($definition.tokenAudiences | default list)) -}}{{- $private = true -}}{{- end -}}
{{- end -}}
{{- range $kind := list "azureFile" "cephfs" "cinder" "flexVolume" "iscsi" "rbd" "scaleIO" "storageos" -}}
{{- if or (has (dig $kind "secretName" "" $volume) $secrets) (has (dig $kind "secretRef" "name" "" $volume) $secrets) -}}{{- $private = true -}}{{- end -}}
{{- end -}}
{{- end -}}
{{- range $pod.imagePullSecrets | default list -}}{{- if has (.name | default "") $secrets -}}{{- $private = true -}}{{- end -}}{{- end -}}
{{- range $container := concat ($pod.containers | default list) ($pod.initContainers | default list) ($pod.ephemeralContainers | default list) -}}
{{- range $container.envFrom | default list -}}{{- if has (dig "secretRef" "name" "" .) $secrets -}}{{- $private = true -}}{{- end -}}{{- end -}}
{{- range $container.env | default list -}}{{- if has (dig "valueFrom" "secretKeyRef" "name" "" .) $secrets -}}{{- $private = true -}}{{- end -}}{{- end -}}
{{- if eq (dig "securityContext" "privileged" false $container | toJson) "true" -}}{{- $private = true -}}{{- end -}}
{{- range dig "securityContext" "capabilities" "add" list $container -}}
{{- if has . (list "ALL" "SYS_ADMIN" "SYS_PTRACE" "SYS_MODULE" "SYS_RAWIO" "BPF" "PERFMON" "CHECKPOINT_RESTORE" "DAC_READ_SEARCH") -}}{{- $private = true -}}{{- end -}}
{{- end -}}
{{- end -}}
{{- $account := $pod.serviceAccountName | default "" -}}
{{- $privilegedIdentity := or (and (eq .namespace $activation.root.namespace.name) (eq $account $activation.root.account.name))
  (and (eq .namespace "kars-sre") (eq $account "sre-api-router"))
  (and (eq .namespace "kube-system") (has $account ($definition.controllers | default list))) -}}
{{- if and $privilegedIdentity (or (ne (get $pod "automountServiceAccountToken" | toJson) "false") $token) -}}{{- $private = true -}}{{- end -}}
{{- range $field := list "hostNetwork" "hostPID" "hostIPC" -}}
{{- if eq (get $pod $field | toJson) "true" -}}{{- $private = true -}}{{- end -}}
{{- end -}}
{{- $private -}}
{{- end -}}
{{/* A no-op policy publication can unblock an interrupted initial pause. It
     neither claims retirement nor changes trust; the full sealed-values check
     still runs, and the CLI additionally verifies the SSA controller projection. */}}
{{- define "kars.sre.privatePausingNoop" -}}
{{- $root := .root -}}{{- $controller := .controller -}}{{- $workspace := .workspace -}}
{{- $fields := dig "metadata" "annotations" dict $workspace -}}
{{- $prefix := "kars.azure.com/private-" -}}
{{- $raw := get $fields (print $prefix "root-retirement") -}}
{{- $state := $raw | mustFromJson -}}
{{- $fence := $root.Values.controller.upgradePrecondition | default dict -}}
{{- if or (ne (include "kars.sre.privateCanonical" $state) $raw) (ne (toJson $state.version) "1")
  (gt (len (omit $state "version" "attempt" "binding" "replicaIntent" "originalVersion" "pauseRoot" "phase" "captured" "exposedKeys")) 0)
  (ne $state.phase "pausing") (ne (toJson $state.pauseRoot) "true")
  (not (regexMatch "^[a-f0-9]{64}$" ($state.attempt | default "")))
  (not (regexMatch "^[a-f0-9]{64}$" ($state.binding | default "")))
  (not (kindIs "string" $state.originalVersion)) (empty $state.originalVersion)
  (not (regexMatch "^[1-9][0-9]*$" (toJson $state.replicaIntent))) (gt (int64 $state.replicaIntent) 2147483647)
  (not (kindIs "map" $state.captured)) (not (hasKey $state.captured $workspace.metadata.uid))
  (not (kindIs "slice" $state.exposedKeys)) (empty $state.exposedKeys)
  (ne (get $fields (print $prefix "enabled")) "true") (ne (get $fields (print $prefix "state")) "Pending")
  (get $fields (print $prefix "core-publication"))
  $workspace.metadata.deletionTimestamp $controller.metadata.deletionTimestamp
  (ne $workspace.metadata.name $root.Release.Namespace)
  (ne $controller.metadata.name "kars-controller") (ne $controller.metadata.namespace $root.Release.Namespace)
  (ne (dig "spec" "template" "spec" "serviceAccountName" "" $controller) "kars-controller")
  (ne (dig "spec" "replicas" -1 $controller | toJson) "0") (ne (toJson $root.Values.controller.replicas) "0")
  (ne ($fence.uid | default "") $controller.metadata.uid)
  (ne ($fence.resourceVersion | default "") $controller.metadata.resourceVersion) -}}
{{- fail "Paused provider maintenance requires the original Pending root, retirement evidence and exact operation fence" -}}
{{- end -}}
{{- range $uid, $pods := $state.captured -}}
{{- if or (empty $uid) (not (kindIs "slice" $pods)) -}}{{- fail "Paused provider maintenance captured identities are malformed" -}}{{- end -}}
{{- range $pods -}}{{- if or (not (kindIs "string" .)) (empty .) -}}{{- fail "Paused provider maintenance Pod identity is malformed" -}}{{- end -}}{{- end -}}
{{- end -}}
{{- range $state.exposedKeys -}}{{- if not (regexMatch "^[a-f0-9]{64}$" .) -}}{{- fail "Paused provider maintenance exposed key is malformed" -}}{{- end -}}{{- end -}}
{{- range $key, $value := dict "namespace-uid" $workspace.metadata.uid "root-namespace" $workspace.metadata.name "root-namespace-uid" $workspace.metadata.uid "root-deployment" $controller.metadata.name "root-deployment-uid" $controller.metadata.uid -}}
{{- if ne (get $fields (print $prefix $key)) $value -}}{{- fail "Paused provider maintenance root identity changed" -}}{{- end -}}
{{- end -}}
{{- if ne (include "kars.sre.privateCanonical" $controller.spec.template | sha256sum) (get $fields (print $prefix "root-template-digest")) -}}
{{- fail "Paused provider maintenance root template changed" -}}
{{- end -}}
{{- $_ := set .receipt "replicaIntent" $state.replicaIntent -}}
{{- end -}}
{{- define "kars.sre.privateContinuity" -}}
{{- $root := .root -}}
{{- $controller := .controller | default dict -}}
{{- $workspace := .workspace | default dict -}}
{{- $fields := dig "metadata" "annotations" dict $workspace -}}
{{- $prefix := "kars.azure.com/private-" -}}
{{- $raw := get $fields (print $prefix "root-retirement") -}}
{{- $proof := $raw | mustFromJson -}}
{{- $state := $proof -}}
{{- $qualified := eq (get $fields (print $prefix "state")) "Qualified" -}}
{{- if $qualified -}}
{{- if or (ne (toJson $proof.version) "2") (ne (len $proof) 4) (not (kindIs "string" $proof.retirement)) -}}
{{- fail "Private Core continuity requires the retained qualified retirement receipt" -}}
{{- end -}}
{{- $state = $proof.retirement | mustFromJson -}}
{{- if or (ne (include "kars.sre.privateCanonical" $state) $proof.retirement) (ne ($proof.retirement | sha256sum) $proof.retirementDigest) -}}
{{- fail "Private Core continuity retirement receipt digest changed" -}}
{{- end -}}
{{- else -}}
{{- $publication := get $fields (print $prefix "core-publication") | mustFromJson -}}
{{- if or (ne (get $fields (print $prefix "state")) "Pending")
  (ne (toJson $publication.version) "1") (ne (len $publication) 5) (ne $publication.phase "preparing")
  (not (regexMatch "^[a-f0-9]{64}$" ($publication.attempt | default "")))
  (not (regexMatch "^[a-f0-9]{64}$" ($publication.planDigest | default "")))
  (not (kindIs "string" $publication.context)) (empty $publication.context) -}}
{{- fail "Private Core continuity requires owned preparing publication; unknown or launched outcomes cannot render" -}}
{{- end -}}
{{- end -}}
{{- if or (ne (include "kars.sre.privateCanonical" $proof) $raw) (ne (toJson $state.version) "1")
  (gt (len (omit $state "version" "attempt" "binding" "replicaIntent" "originalVersion" "pauseRoot" "phase" "captured" "exposedKeys" "baseline")) 0)
  (not (regexMatch "^[a-f0-9]{64}$" ($state.attempt | default "")))
  (not (regexMatch "^[a-f0-9]{64}$" ($state.binding | default "")))
  (not (kindIs "string" $state.originalVersion)) (empty $state.originalVersion)
  (ne (toJson $state.pauseRoot) "true")
  (not (regexMatch "^[1-9][0-9]*$" (toJson $state.replicaIntent))) (gt (int64 $state.replicaIntent) 2147483647)
  (ne $state.phase (ternary "restoring" "retired" $qualified))
  (ne (get $fields (print $prefix "enabled")) "true")
  $workspace.metadata.deletionTimestamp $controller.metadata.deletionTimestamp
  (ne $workspace.metadata.name $root.Release.Namespace)
  (ne $controller.metadata.name "kars-controller") (ne $controller.metadata.namespace $root.Release.Namespace)
  (ne (dig "spec" "template" "spec" "serviceAccountName" "" $controller) "kars-controller")
  (ne (dig "spec" "replicas" -1 $controller | toJson) (ternary (toJson $state.replicaIntent) "0" $qualified))
  (ne (toJson $root.Values.controller.replicas) (ternary (toJson $state.replicaIntent) "0" $qualified)) -}}
{{- fail "Private Core continuity requires the original live root and exact retired/restored replica intent" -}}
{{- end -}}
{{- if or (gt (len $state.originalVersion) 253) (not (kindIs "map" $state.captured))
  (not (kindIs "slice" $state.exposedKeys)) (ne (len ($state.baseline | default dict)) 3)
  (not (kindIs "string" $state.baseline.resourceVersion)) (empty $state.baseline.resourceVersion)
  (not (regexMatch "^[a-f0-9]{64}$" ($state.baseline.keyDigest | default "")))
  (not (has $state.baseline.keyDigest $state.exposedKeys)) -}}
{{- fail "Private Core continuity requires the retained TLS baseline and captured retirement evidence" -}}
{{- end -}}
{{- range $state.exposedKeys -}}
{{- if not (regexMatch "^[a-f0-9]{64}$" .) -}}{{- fail "Private Core continuity exposed-key evidence is invalid" -}}{{- end -}}
{{- end -}}
{{- range $uid, $pods := $state.captured -}}
{{- if or (empty $uid) (not (kindIs "slice" $pods)) -}}{{- fail "Private Core continuity captured scope changed" -}}{{- end -}}
{{- range $pods -}}
{{- if or (not (kindIs "string" .)) (empty .) (gt (len .) 253) -}}{{- fail "Private Core continuity captured pod identity is invalid" -}}{{- end -}}
{{- end -}}
{{- end -}}
{{- $namespace := dict "name" $root.Release.Namespace "uid" $workspace.metadata.uid -}}
{{- $deployment := dict "name" "kars-controller" "uid" $controller.metadata.uid -}}
{{- $account := dict "name" "kars-controller" "uid" (get $fields (print $prefix "root-uid")) -}}
{{- range $key, $value := dict "namespace-uid" $namespace.uid "root-namespace" $namespace.name "root-namespace-uid" $namespace.uid "root-deployment" $deployment.name "root-deployment-uid" $deployment.uid "root-account" $account.name "root-user" (printf "system:serviceaccount:%s:kars-controller" $namespace.name) "budget-namespace" $namespace.name "budget-namespace-uid" $namespace.uid "budget-tls-name" $root.Values.inferenceBudget.tlsSecretName -}}
{{- if or (empty $value) (ne (get $fields (print $prefix $key)) $value) -}}
{{- fail "Private Core continuity identity annotations changed" -}}
{{- end -}}
{{- end -}}
{{- if empty $account.uid -}}{{- fail "Private Core continuity lacks its original account UID" -}}{{- end -}}
{{- $template := deepCopy $controller.spec.template -}}
{{- if dig "metadata" "annotations" dict $template -}}
{{- $_ := unset $template.metadata.annotations (print $prefix "epoch") -}}
{{- if not $template.metadata.annotations -}}{{- $_ := unset $template.metadata "annotations" -}}{{- end -}}
{{- end -}}
{{- $templateDigest := include "kars.sre.privateCanonical" $template | sha256sum -}}
{{- if ne $templateDigest (get $fields (print $prefix "root-template-digest")) -}}
{{- fail "Private Core continuity root template differs from its receipt" -}}
{{- end -}}
{{- $profile := get $fields (print $prefix "profile") -}}
{{- $controllerUids := dict -}}
{{- if eq $profile "service-accounts" -}}
{{- if not .privateControllers -}}{{- fail "Private Core continuity requires the maintained controller profile" -}}{{- end -}}
{{- range .privateControllers -}}
{{- $uid := get $fields (printf "%s%s-uid" $prefix .) -}}
{{- if empty $uid -}}{{- fail "Private Core continuity controller profile is incomplete" -}}{{- end -}}
{{- $_ := set $controllerUids . $uid -}}
{{- end -}}
{{- else if ne $profile "kcm-certificate" -}}{{- fail "Private Core continuity controller profile changed" -}}{{- end -}}
{{- $secret := dict "name" $root.Values.inferenceBudget.tlsSecretName "uid" (get $fields (print $prefix "budget-tls-uid")) -}}
{{- $consumers := list (dict "kind" "Deployment" "object" $deployment "templateDigest" $templateDigest) -}}
{{- $namespaceBindings := list (dict "namespace" $namespace "consumers" $consumers) -}}
{{- if not $qualified -}}
{{/* Pending keeps the predecessor's reviewed parent UIDs in every enrolled
     namespace. Reconstruct paused Deployments; the retirement binding below
     authenticates the complete namespace, identity and template inventory. */}}
{{- $scopes := deepCopy (.privateNamespaces | default dict) -}}
{{- $_ := set $scopes $namespace.name $workspace -}}
{{- $namespaceUids := dict -}}{{- $uids := dict $deployment.uid true -}}
{{- $liveConsumers := .privateConsumers | default dict -}}
{{- $namespaceBindings = list -}}
{{- range $scopeName := keys $scopes | sortAlpha -}}
{{- $scope := get $scopes $scopeName -}}
{{- $scopeUid := dig "metadata" "uid" "" $scope -}}
{{- $scopeFields := dig "metadata" "annotations" dict $scope -}}
{{- if or (empty $scopeUid) (hasKey $namespaceUids $scopeUid) (ne (dig "metadata" "name" "" $scope) $scopeName)
  (dig "metadata" "deletionTimestamp" "" $scope) (ne (get $scopeFields (print $prefix "namespace-uid")) $scopeUid) -}}
{{- fail "Private Core continuity Pending namespace identity changed" -}}
{{- end -}}
{{- $_ := set $namespaceUids $scopeUid true -}}
{{- range $key := list "enabled" "state" "profile" "bundle-revision" "root-namespace" "root-namespace-uid" "root-account" "root-user" "root-uid" "root-deployment" "root-deployment-uid" "root-template-digest" "budget-namespace" "budget-namespace-uid" "budget-tls-name" "budget-tls-uid" "budget-tls-version" "budget-key" -}}
{{- if ne (get $scopeFields (print $prefix $key)) (get $fields (print $prefix $key)) -}}{{- fail "Private Core continuity Pending namespace authority changed" -}}{{- end -}}
{{- end -}}
{{- range $name, $uid := $controllerUids -}}
{{- if ne (get $scopeFields (printf "%s%s-uid" $prefix $name)) $uid -}}{{- fail "Private Core continuity Pending namespace controller profile changed" -}}{{- end -}}
{{- end -}}
{{- $ordered := dict -}}
{{- if eq $scopeName $namespace.name -}}
{{- $_ := set $ordered (include "kars.sre.privateCanonical" (first $consumers)) (first $consumers) -}}
{{- end -}}
{{- range $id, $live := $liveConsumers -}}
{{- $uid := dig "metadata" "uid" "" $live -}}
{{- if and (ne $uid $deployment.uid) (hasKey $scopeFields (print $prefix "parent-" $uid)) -}}
{{- $lookupId := printf "Deployment/%s" $live.metadata.name -}}
{{- if ne $scopeName $namespace.name -}}{{- $lookupId = printf "%s/%s" $scopeName $lookupId -}}{{- end -}}
{{- if or (empty $uid) (hasKey $uids $uid) (ne ($live.kind | default "") "Deployment") (ne ($live.apiVersion | default "") "apps/v1")
  (ne (dig "metadata" "namespace" "" $live) $scopeName) (dig "metadata" "deletionTimestamp" "" $live)
  (ne $id $lookupId) (ne (dig "spec" "replicas" -1 $live | toJson) "0") -}}
{{- fail "Private Core continuity requires original paused Deployment companions" -}}
{{- end -}}
{{- $_ := set $uids $uid true -}}
{{- $currentTemplate := dig "spec" "template" dict $live | deepCopy -}}
{{- if dig "metadata" "annotations" dict $currentTemplate -}}
{{- $_ := unset $currentTemplate.metadata.annotations (print $prefix "epoch") -}}
{{- if not $currentTemplate.metadata.annotations -}}{{- $_ := unset $currentTemplate.metadata "annotations" -}}{{- end -}}
{{- end -}}
{{- $bound := dict "kind" "Deployment" "object" (pick $live.metadata "name" "uid") "templateDigest" (include "kars.sre.privateCanonical" $currentTemplate | sha256sum) -}}
{{- $_ := set $ordered (include "kars.sre.privateCanonical" $bound) $bound -}}
{{- end -}}
{{- end -}}
{{- if empty $ordered -}}{{- fail "Private Core continuity Pending namespace has no reviewed consumers" -}}{{- end -}}
{{- $scopeConsumers := list -}}
{{- range $key := keys $ordered | sortAlpha -}}{{- $scopeConsumers = append $scopeConsumers (get $ordered $key) -}}{{- end -}}
{{- $namespaceBindings = append $namespaceBindings (dict "namespace" (dict "name" $scopeName "uid" $scopeUid) "consumers" $scopeConsumers) -}}
{{- end -}}
{{- range $uid, $_ := $state.captured -}}
{{- if not (hasKey $namespaceUids $uid) -}}{{- fail "Private Core continuity captured scope changed" -}}{{- end -}}
{{- end -}}
{{- end -}}
{{- $rootScope := dict -}}
{{- if $qualified -}}
{{- if or (not (kindIs "slice" $proof.activation.namespaces)) (empty $proof.activation.namespaces) -}}{{- fail "Private Core continuity requires reviewed namespaces" -}}{{- end -}}
{{- $definition := required "Private Core continuity requires the maintained private-material definition" .privateDefinition -}}
{{- $liveConsumers := .privateConsumers | default dict -}}
{{- $liveNamespaces := .privateNamespaces | default dict -}}
{{- $names := dict -}}{{- $namespaceUids := dict -}}{{- $bindings := dict -}}{{- $uids := dict -}}{{- $hasRoot := false -}}
{{- range $scope := $proof.activation.namespaces -}}
{{- $scopeName := $scope.namespace.name | default "" -}}{{- $scopeUid := $scope.namespace.uid | default "" -}}
{{- if or (not (kindIs "string" $scopeName)) (not (regexMatch "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$" $scopeName)) (gt (len $scopeName) 63)
  (empty $scopeUid) (hasKey $names $scopeName) (hasKey $namespaceUids $scopeUid)
  (not (kindIs "slice" $scope.consumers)) (empty $scope.consumers) (empty ($scope.epoch | default "")) -}}
{{- fail "Private Core continuity qualified consumer scope changed" -}}
{{- end -}}
{{- $_ := set $names $scopeName true -}}{{- $_ := set $namespaceUids $scopeUid true -}}
{{- $liveNamespace := get $liveNamespaces $scopeName | default dict -}}
{{- if eq $scopeName $namespace.name -}}{{- $liveNamespace = $workspace -}}{{- $rootScope = $scope -}}{{- end -}}
{{- $scopeFields := dig "metadata" "annotations" dict $liveNamespace -}}
{{- if or (ne (dig "metadata" "name" "" $liveNamespace) $scopeName) (ne (dig "metadata" "uid" "" $liveNamespace) $scopeUid)
  (dig "metadata" "deletionTimestamp" "" $liveNamespace) (ne (get $scopeFields (print $prefix "namespace-uid")) $scopeUid)
  (ne (get $scopeFields (print $prefix "epoch")) $scope.epoch) -}}
{{- fail "Private Core continuity live namespace identity or epoch changed" -}}
{{- end -}}
{{- range $key := concat (list "enabled" "state" "profile" "bundle-revision" "root-namespace" "root-namespace-uid" "root-account" "root-user" "root-uid" "root-deployment" "root-deployment-uid" "root-template-digest" "budget-namespace" "budget-namespace-uid" "budget-tls-name" "budget-tls-uid" "budget-tls-version" "budget-key") -}}
{{- if ne (get $scopeFields (print $prefix $key)) (get $fields (print $prefix $key)) -}}{{- fail "Private Core continuity namespace authority changed" -}}{{- end -}}
{{- end -}}
{{- range $name, $uid := $controllerUids -}}
{{- if ne (get $scopeFields (printf "%s%s-uid" $prefix $name)) $uid -}}{{- fail "Private Core continuity namespace controller profile changed" -}}{{- end -}}
{{- end -}}
{{- $identities := dict -}}{{- $ordered := dict -}}
{{- range $consumer := $scope.consumers -}}
{{- $api := include "kars.sre.privateConsumerApi" $consumer.kind -}}
{{- $id := printf "%s/%s" $consumer.kind $consumer.object.name -}}
{{- if or (hasKey $identities $id) (empty $consumer.object.uid) (hasKey $uids $consumer.object.uid) -}}
{{- fail "Private Core continuity consumer identities must be unique" -}}
{{- end -}}
{{- $_ := set $identities $id true -}}{{- $_ := set $uids $consumer.object.uid true -}}
{{- $lookupId := $id -}}
{{- if ne $scopeName $namespace.name -}}{{- $lookupId = printf "%s/%s" $scopeName $id -}}{{- end -}}
{{- $live := get $liveConsumers $lookupId | default dict -}}
{{- if and (eq $scopeName $namespace.name) (eq $consumer.kind "Deployment") (eq $consumer.object.name $deployment.name) -}}
{{- $live = $controller -}}{{- $hasRoot = true -}}
{{- else if or (ne ($live.kind | default "") $consumer.kind) (ne ($live.apiVersion | default "") $api) -}}
{{- fail "Private Core continuity requires every reviewed live consumer" -}}
{{- end -}}
{{- if or (ne (dig "metadata" "uid" "" $live) $consumer.object.uid)
  (ne (dig "metadata" "name" "" $live) $consumer.object.name)
  (ne (dig "metadata" "namespace" "" $live) $scopeName) (dig "metadata" "deletionTimestamp" "" $live) -}}
{{- fail "Private Core continuity live consumer identity changed" -}}
{{- end -}}
{{- $currentTemplate := dig "spec" "template" dict $live | deepCopy -}}
{{- if eq $consumer.kind "CronJob" -}}
{{- $currentTemplate = dig "spec" "jobTemplate" "spec" "template" dict $live | deepCopy -}}
{{- else if eq $consumer.kind "Pod" -}}
{{- $currentTemplate = dict "metadata" (dict "labels" (dig "metadata" "labels" dict $live) "annotations" (dig "metadata" "annotations" dict $live)) "spec" $live.spec | deepCopy -}}
{{- end -}}
{{- $epoch := dig "metadata" "annotations" (print $prefix "epoch") "" $currentTemplate -}}
{{- $requiresEpoch := eq (include "kars.sre.consumesPrivateAuthority" (dict "pod" $currentTemplate.spec "namespace" $scopeName "activation" $proof.activation "definition" $definition)) "true" -}}
{{- if and (or $requiresEpoch (hasKey (dig "metadata" "annotations" dict $currentTemplate) (print $prefix "epoch"))) (ne $epoch $scope.epoch) -}}
{{- fail "Private Core continuity live consumer epoch changed" -}}
{{- end -}}
{{- if dig "metadata" "annotations" dict $currentTemplate -}}
{{- $_ := unset $currentTemplate.metadata.annotations (print $prefix "epoch") -}}
{{- if not $currentTemplate.metadata.annotations -}}{{- $_ := unset $currentTemplate.metadata "annotations" -}}{{- end -}}
{{- end -}}
{{- $digest := include "kars.sre.privateCanonical" $currentTemplate | sha256sum -}}
{{- if ne $digest $consumer.templateDigest -}}{{- fail "Private Core continuity live consumer template changed" -}}{{- end -}}
{{- $bound := dict "kind" $consumer.kind "object" (pick $consumer.object "name" "uid") "templateDigest" $digest -}}
{{- $_ := set $ordered (include "kars.sre.privateCanonical" $bound) $bound -}}
{{- end -}}
{{- $scopeConsumers := list -}}
{{- range $key := keys $ordered | sortAlpha -}}{{- $scopeConsumers = append $scopeConsumers (get $ordered $key) -}}{{- end -}}
{{- $_ := set $bindings $scopeName (dict "namespace" (pick $scope.namespace "name" "uid") "consumers" $scopeConsumers) -}}
{{- end -}}
{{- if or (not $hasRoot) (empty $rootScope) -}}{{- fail "Private Core continuity requires its reviewed root consumer" -}}{{- end -}}
{{- range $uid, $_ := $state.captured -}}
{{- if not (hasKey $namespaceUids $uid) -}}{{- fail "Private Core continuity captured scope changed" -}}{{- end -}}
{{- end -}}
{{- $namespaceBindings = list -}}
{{- range $name := keys $bindings | sortAlpha -}}{{- $namespaceBindings = append $namespaceBindings (get $bindings $name) -}}{{- end -}}
{{- end -}}
{{- $binding := dict "contract" "kars.azure.com/private-consumption/v1" "bundleRevision" (get $fields (print $prefix "bundle-revision")) "profile" $profile "controllerUids" $controllerUids
  "root" (dict "namespace" $namespace "account" $account "deployment" $deployment "templateDigest" $templateDigest "replicaIntent" $state.replicaIntent "budget" (dict "namespace" $namespace "secret" $secret))
  "namespaces" $namespaceBindings -}}
{{- if or (empty $secret.uid) (not (regexMatch "^[a-f0-9]{64}$" $binding.bundleRevision)) (ne ($binding | toRawJson | sha256sum) $state.binding) -}}
{{- fail "Private Core continuity retirement binding changed" -}}
{{- end -}}
{{- if $qualified -}}
{{- $activation := $proof.activation -}}
{{- if or (ne $activation.phase "qualified") (ne $activation.contract $binding.contract)
  (ne $activation.bundleRevision $binding.bundleRevision) (ne $activation.profile $profile)
  (ne ($activation.controllerUids | toRawJson) ($controllerUids | toRawJson))
  (ne $activation.root.templateDigest $templateDigest) (ne (toJson $activation.root.replicaIntent) (toJson $state.replicaIntent)) -}}
{{- fail "Private Core continuity qualified activation changed" -}}
{{- end -}}
{{- range $pair := list (list $activation.root.namespace $namespace) (list $activation.root.deployment $deployment) (list $activation.root.account $account) (list $activation.root.budgetTls.namespace $namespace) (list $activation.root.budgetTls.secret $secret) (list $rootScope.namespace $namespace) -}}
{{- if ne (pick (first $pair) "name" "uid" | toRawJson) (last $pair | toRawJson) -}}{{- fail "Private Core continuity qualified identities changed" -}}{{- end -}}
{{- end -}}
{{- $epoch := $rootScope.epoch | default "" -}}
{{- if or (empty $epoch) (ne $epoch (get $fields (print $prefix "epoch")))
  (ne $epoch (dig "spec" "template" "metadata" "annotations" (print $prefix "epoch") "" $controller)) -}}
{{- fail "Private Core continuity qualified activation epoch changed" -}}
{{- end -}}
{{- end -}}
{{- $catalog := .catalog | default dict -}}
{{- $tls := .budgetTls | default dict -}}
{{- range $object := list $catalog $tls -}}
{{- if or (empty $object.metadata.uid) $object.metadata.deletionTimestamp (ne $object.metadata.namespace $root.Release.Namespace) -}}
{{- fail "Private Core continuity requires live catalog and TLS identities in the release namespace" -}}
{{- end -}}
{{- end -}}
{{- $owned := list $catalog -}}
{{- $identity := $root.Values.inferenceBudget.tls | default dict -}}
{{- if $identity.certificate -}}{{- $owned = append $owned $tls -}}{{- end -}}
{{- range $object := $owned -}}
{{- if or (ne (dig "metadata" "annotations" "meta.helm.sh/release-name" "" $object) $root.Release.Name)
  (ne (dig "metadata" "annotations" "meta.helm.sh/release-namespace" "" $object) $root.Release.Namespace)
  (ne (dig "metadata" "labels" "app.kubernetes.io/managed-by" "" $object) "Helm") -}}
{{- fail "Private Core continuity requires live release-owned catalog and TLS identities" -}}
{{- end -}}
{{- end -}}
{{- if or (ne $catalog.metadata.name "kars-inference-budget-contracts")
  (ne (dig "metadata" "annotations" "kars.azure.com/inference-budget-contracts" "" $catalog) "v1")
  (ne $tls.type "kubernetes.io/tls") (ne $tls.metadata.name $secret.name) (ne $tls.metadata.uid $secret.uid)
  (ne $root.Values.inferenceBudget.tlsSecretName $secret.name)
  (ne (dig "baseline" "secretUid" "" $state) $secret.uid)
  (eq (dig "baseline" "resourceVersion" "" $state) $tls.metadata.resourceVersion)
  (ne (index $catalog.data "ca.crt") $root.Values.inferenceBudget.caBundle)
  (not (contains "-----BEGIN CERTIFICATE-----" $root.Values.inferenceBudget.caBundle)) (contains "PRIVATE KEY" $root.Values.inferenceBudget.caBundle) -}}
{{- fail "Private Core continuity must preserve the already-rotated live public CA and original TLS identity" -}}
{{- end -}}
{{- if and $qualified (or (ne $proof.activation.root.budgetTls.secret.resourceVersion $tls.metadata.resourceVersion)
  (ne (get $fields (print $prefix "budget-tls-version")) $tls.metadata.resourceVersion)
  (ne (get $fields (print $prefix "budget-key")) $proof.activation.root.budgetTls.keyDigest)
  (has $proof.activation.root.budgetTls.keyDigest $state.exposedKeys)) -}}
{{- fail "Private Core continuity qualified TLS lineage changed" -}}
{{- end -}}
{{- $_ := set .receipt "replicaIntent" $state.replicaIntent -}}
{{- end -}}
