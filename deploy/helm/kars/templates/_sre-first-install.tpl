{{/* Copyright (c) Microsoft Corporation. Licensed under the MIT License. */}}
{{- define "kars.sre.firstInstallTransition" -}}
{{- $root := .root -}}
{{- $prior := .intentMap -}}
{{- $registration := .registration -}}
{{- $source := .source -}}
{{- $custom := .resources -}}
{{- $binding := dig "spec" "firstInstallFunding" dict $registration -}}
{{- $intent := index $prior.data "intent.json" | mustFromJson -}}
{{- $adoption := .adoptionMap | default dict -}}
{{/* Top-ups and unbounded allowances are separately authorized ConfigMap writes,
     not changes to the original SRE funding intent. Restore only these fields for
     comparison; never alter the emitted ConfigMap or the immutable receipts. */}}
{{- $comparisonBudget := deepCopy $root.Values.inferenceBudget -}}
{{/* Calendar policy is independently fund-authorized in the catalog. Old sealed
     values omitted the field and therefore retain strict enforcement by default. */}}
{{- $calendarPolicy := "enforce" -}}
{{- if hasKey $comparisonBudget "calendarPolicy" -}}
{{- $calendarPolicy = $comparisonBudget.calendarPolicy -}}
{{- if or (not (kindIs "string" $calendarPolicy)) (not (has $calendarPolicy (list "enforce" "record-only"))) -}}
{{- fail "inferenceBudget.calendarPolicy must be enforce or record-only" -}}
{{- end -}}
{{- end -}}
{{- if and (not $adoption) (ne $calendarPolicy ($intent.budget.calendarPolicy | default "enforce")) -}}
{{- fail "Calendar policy changes require completed SRE adoption" -}}
{{- end -}}
{{- if hasKey $intent.budget "calendarPolicy" -}}
{{- $_ := set $comparisonBudget "calendarPolicy" $intent.budget.calendarPolicy -}}
{{- else -}}
{{- $_ := unset $comparisonBudget "calendarPolicy" -}}
{{- end -}}
{{- range $field, $label := dict "tokenTopUps" "Token top-ups" "unboundedTokenAllowances" "Unbounded token allowances" -}}
{{- if and (hasKey $comparisonBudget $field) (not (kindIs "slice" (index $comparisonBudget $field))) -}}
{{- fail (printf "inferenceBudget.%s must be a list" $field) -}}
{{- end -}}
{{- if and (not $adoption) (ne (index $comparisonBudget $field | default list | toJson) (index $intent.budget $field | default list | toJson)) -}}
{{- fail (printf "%s require completed SRE adoption; preserve initial funding configuration during ownership transition" $label) -}}
{{- end -}}
{{- if hasKey $intent.budget $field -}}
{{- $_ := set $comparisonBudget $field (index $intent.budget $field) -}}
{{- else -}}
{{- $_ := unset $comparisonBudget $field -}}
{{- end -}}
{{- end -}}
{{/* Operator-owned non-inference classifications are independent of model bounds
     and funding. The router still requires each sandbox's signed egress allowlist. */}}
{{- if ne ($comparisonBudget.nonInferenceEgressHosts | default list | toJson) ($intent.budget.nonInferenceEgressHosts | default list | toJson) -}}
{{- $hosts := $comparisonBudget.nonInferenceEgressHosts -}}
{{- if or (not $adoption) (not (kindIs "slice" $hosts)) -}}
{{- fail "Non-inference egress changes require completed SRE adoption and an explicit host list" -}}
{{- end -}}
{{- if gt (len $hosts) 64 -}}{{- fail "Non-inference egress permits at most 64 exact hosts" -}}{{- end -}}
{{- range $host := $hosts -}}
{{- if not (kindIs "string" $host) -}}{{- fail "Non-inference egress hosts must be strings" -}}{{- end -}}
{{- if or (gt (len $host) 253) (not (regexMatch "^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?([.][a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$" $host)) -}}
{{- fail "Non-inference egress requires exact lowercase hostnames, not URLs, ports or wildcards" -}}
{{- end -}}
{{- range $contract := $comparisonBudget.contracts | default list -}}
{{- $modelHost := regexReplaceAll ":[0-9]+$" ((urlParse $contract.endpoint).host | lower | trimSuffix ".") "" -}}
{{- if eq $host $modelHost -}}{{- fail "A model endpoint cannot be classified as non-inference egress" -}}{{- end -}}
{{- end -}}
{{- end -}}
{{- if hasKey $intent.budget "nonInferenceEgressHosts" -}}
{{- $_ := set $comparisonBudget "nonInferenceEgressHosts" $intent.budget.nonInferenceEgressHosts -}}
{{- else -}}
{{- $_ := unset $comparisonBudget "nonInferenceEgressHosts" -}}
{{- end -}}
{{- end -}}
{{/* Existing model bounds stay sealed apart from validity/version renewal. After
     adoption, additional model routes do not change SRE's pinned inference policy. */}}
{{- if or (ne ($comparisonBudget.contracts | default list | toJson) ($intent.budget.contracts | default list | toJson)) (ne ($comparisonBudget.catalogVersion | default "") ($intent.budget.catalogVersion | default "")) -}}
{{- $contracts := $comparisonBudget.contracts | default list -}}
{{- $originals := $intent.budget.contracts | default list -}}
{{- if or (not $adoption) (not (kindIs "slice" $contracts)) (not $contracts) (lt (len $contracts) (len $originals)) (not (kindIs "string" $comparisonBudget.catalogVersion)) (empty $comparisonBudget.catalogVersion) (eq $comparisonBudget.catalogVersion $intent.budget.catalogVersion) -}}
{{- fail "Model contract changes require completed SRE adoption, all original contracts and a new catalog version" -}}
{{- end -}}
{{- $renewed := false -}}
{{- range $index, $original := $originals -}}
{{- $contract := index $contracts $index -}}
{{- if ne ($contract | toJson) ($original | toJson) -}}
{{- if or (not (kindIs "map" $contract)) (ne (omit $contract "validUntil" "version" | toJson) (omit $original "validUntil" "version" | toJson)) -}}
{{- fail "Model contract renewal cannot change destinations, models, operations, bounds, prices or contract identity" -}}
{{- end -}}
{{- $expiry := $contract.validUntil | default "" -}}
{{- $parsed := toDate "2006-01-02T15:04:05Z07:00" $expiry -}}
{{- if or (not (regexMatch "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$" $expiry))
  (ne (dateInZone "2006-01-02T15:04:05Z" $parsed "UTC") $expiry)
  (le (int64 (unixEpoch $parsed)) (int64 (unixEpoch now)))
  (le (int64 (unixEpoch $parsed)) (int64 (unixEpoch (toDate "2006-01-02T15:04:05Z07:00" $original.validUntil))))
  (not (kindIs "string" $contract.version)) (empty $contract.version) (eq $contract.version $original.version) -}}
{{- fail "Model contract renewal requires a new version and a strictly later, unexpired UTC validity deadline" -}}
{{- end -}}
{{- $renewed = true -}}
{{- end -}}
{{- end -}}
{{- $ids := dict -}}
{{- $routes := dict -}}
{{- range $index, $contract := $contracts -}}
{{- if not (kindIs "map" $contract) -}}{{- fail "Model contracts must be objects" -}}{{- end -}}
{{- $id := $contract.id | default "" -}}
{{- if or (not (kindIs "string" $id)) (empty $id) -}}{{- fail "Model contracts require nonempty string identities" -}}{{- end -}}
{{- $route := list $contract.providerId $contract.model $contract.operation | toJson -}}
{{- if or (hasKey $ids $id) (hasKey $routes $route) -}}{{- fail "Additional model contracts cannot duplicate identities or provider/model/operation routes" -}}{{- end -}}
{{- $_ := set $ids $id true -}}{{- $_ := set $routes $route true -}}
{{- if ge $index (len $originals) -}}
{{- range $original := $originals -}}
{{- if and (eq $contract.providerId $original.providerId) (eq $contract.model $original.model) -}}
{{- fail "Additional contracts cannot replace or extend an original model route" -}}
{{- end -}}
{{- end -}}
{{- range $key := list "providerId" "model" "endpoint" "version" -}}
{{- $value := get $contract $key -}}
{{- if or (not (kindIs "string" $value)) (empty $value) -}}{{- fail "Additional model contracts require explicit provider, model, endpoint and version" -}}{{- end -}}
{{- end -}}
{{- range $key := list "maximumInputTokens" "maximumOutputTokens" "maximumWireBytes" -}}
{{- $value := get $contract $key -}}
{{- if or (not (regexMatch "^[1-9][0-9]{0,15}$" (toJson $value))) (gt (float64 $value) 9007199254740991.0) -}}
{{- fail "Additional model contracts require finite positive safe-integer bounds" -}}
{{- end -}}
{{- end -}}
{{- $expiry := $contract.validUntil | default "" -}}
{{- $parsed := toDate "2006-01-02T15:04:05Z07:00" $expiry -}}
{{- if or (not (regexMatch "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$" $expiry))
  (ne (dateInZone "2006-01-02T15:04:05Z" $parsed "UTC") $expiry)
  (le (int64 (unixEpoch $parsed)) (int64 (unixEpoch now))) -}}
{{- fail "Additional model contracts require an unexpired UTC validity deadline" -}}
{{- end -}}
{{- $renewed = true -}}
{{- end -}}
{{- end -}}
{{- if not $renewed -}}{{- fail "Catalog version changes require an actual model contract renewal or addition" -}}{{- end -}}
{{- $_ := set $comparisonBudget "contracts" $intent.budget.contracts -}}
{{- $_ := set $comparisonBudget "catalogVersion" $intent.budget.catalogVersion -}}
{{- end -}}
{{- $upgradeValues := deepCopy $root.Values -}}
{{- $_ := set $upgradeValues "inferenceBudget" $comparisonBudget -}}
{{/* A fresh CAS fence is operation metadata, not a change to sealed SRE configuration. */}}
{{- if hasKey $root.Values.controller "upgradePrecondition" -}}
{{- $fence := $root.Values.controller.upgradePrecondition -}}
{{- if or (not $adoption) (not (kindIs "map" $fence)) -}}
{{- fail "SRE controller.upgradePrecondition requires completed adoption and an exact live UID/resourceVersion fence" -}}
{{- end -}}
{{- if or (ne (len $fence) 2) (not (kindIs "string" $fence.uid)) (not (kindIs "string" $fence.resourceVersion)) -}}
{{- fail "SRE controller.upgradePrecondition requires exactly two string UID/resourceVersion fields" -}}
{{- end -}}
{{- if or (empty $fence.uid) (empty $fence.resourceVersion)
  (ne $fence.uid (dig "metadata" "uid" "" .controller))
  (ne $fence.resourceVersion (dig "metadata" "resourceVersion" "" .controller)) -}}
{{- fail "SRE controller.upgradePrecondition does not match the live controller UID/resourceVersion" -}}
{{- end -}}
{{- $_ := unset $upgradeValues.controller "upgradePrecondition" -}}
{{- end -}}
{{/* A restored, independently qualified private root may precede initial SRE
     adoption. Its rotated CA otherwise creates a cycle with SRE readiness.
     Pending/paused publication still requires completed adoption. */}}
{{- $privateInitialAdoption := false -}}
{{- $privatePendingPublication := false -}}
{{- if or (eq (toJson $root.Values.controller.replicas) "0") (ne ($comparisonBudget.caBundle | default "") ($intent.budget.caBundle | default "")) -}}
{{- if and (not $adoption) (or (eq (toJson $root.Values.controller.replicas) "0") (ne (dig "metadata" "annotations" "kars.azure.com/private-state" "" .workspace) "Qualified")) -}}
{{- fail "Private Core continuity requires completed SRE adoption unless the original root is restored and qualified" -}}
{{- end -}}
{{- $continuity := dict -}}
{{- $retirement := dig "metadata" "annotations" "kars.azure.com/private-root-retirement" "{}" .workspace | mustFromJson -}}
{{- if and (eq ($retirement.phase | default "") "pausing") (eq ($comparisonBudget.caBundle | default "") ($intent.budget.caBundle | default "")) -}}
{{- include "kars.sre.privatePausingNoop" (dict "root" $root "controller" .controller "workspace" .workspace "receipt" $continuity) -}}
{{- else -}}
{{- include "kars.sre.privateContinuity" (dict "root" $root "controller" .controller "workspace" .workspace "catalog" .catalog "budgetTls" .budgetTls "privateControllers" .privateControllers "privateDefinition" .privateDefinition "privateConsumers" .privateConsumers "privateNamespaces" .privateNamespaces "receipt" $continuity) -}}
{{- end -}}
{{- $privateInitialAdoption = not $adoption -}}
{{- $privatePendingPublication = and (not (empty $adoption)) (eq ($retirement.phase | default "") "retired")
  (eq (toJson $root.Values.controller.replicas) "0")
  (eq (dig "metadata" "annotations" "kars.azure.com/private-state" "" .workspace) "Pending") -}}
{{- if eq (toJson $root.Values.controller.replicas) "0" -}}
{{- $_ := set $upgradeValues.controller "replicas" $continuity.replicaIntent -}}
{{- end -}}
{{- $_ := set $comparisonBudget "caBundle" $intent.budget.caBundle -}}
{{- end -}}
{{- $comparisonResources := deepCopy $custom -}}
{{- if hasKey $root.Values.sre "modelRouteBaseline" -}}
{{- $baseline := $root.Values.sre.modelRouteBaseline -}}
{{- if or (not $adoption) (not (kindIs "map" $baseline)) -}}
{{- fail "SRE model selection requires completed adoption and the original provider/model values baseline" -}}
{{- end -}}
{{- if or (ne (len $baseline) 2) (not (kindIs "string" $baseline.provider)) (not (kindIs "string" $baseline.model)) -}}
{{- fail "SRE modelRouteBaseline requires exactly the original provider and model strings, including empty inherited defaults" -}}
{{- end -}}
{{- $selectedRoute := include "kars.sre.modelRoute" $root | fromJson -}}
{{- $_ := set $upgradeValues.sre "provider" $baseline.provider -}}
{{- $_ := set $upgradeValues.sre "model" $baseline.model -}}
{{- $_ := unset $upgradeValues.sre "modelRouteBaseline" -}}
{{- $originalRoute := include "kars.sre.modelRoute" (dict "Values" $upgradeValues) | fromJson -}}
{{- $foundOriginal := false -}}
{{- range $intent.resources -}}
{{- if and (eq .kind "InferencePolicy") (eq .metadata.name "sre-inference") -}}
{{- if ne (toJson .spec.modelPreference.primary) (toJson $originalRoute) -}}
{{- fail "SRE model route baseline must reproduce the original sealed policy" -}}
{{- end -}}
{{- $foundOriginal = true -}}
{{- end -}}
{{- end -}}
{{- if not $foundOriginal -}}{{- fail "SRE model selection requires the original sealed inference policy" -}}{{- end -}}
{{- range $comparisonResources -}}
{{- if and (eq .kind "InferencePolicy") (eq .metadata.name "sre-inference") -}}
{{- if ne (toJson .spec.modelPreference.primary) (toJson $selectedRoute) -}}
{{- fail "SRE inference policy must match the selected provider and model" -}}
{{- end -}}
{{/* Normalize only for the sealed comparison. Emitted resources keep the new
     primary; fallback, safeguards, identity and funding remain unchanged. */}}
{{- $_ := set .spec.modelPreference "primary" (deepCopy $originalRoute) -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- range $intent.resources -}}
{{- if and (eq .kind "InferencePolicy") (eq .metadata.name "sre-inference") -}}
{{- if eq (include "kars.sre.unboundedKind" $root) "true" -}}
{{- $originalBudget := get .spec "tokenBudget" -}}
{{- if and $originalBudget (not $adoption) -}}
{{- fail "Removing sealed SRE token ceilings requires completed adoption and unbounded Kind funding" -}}
{{- end -}}
{{- range $comparisonResources -}}
{{- if and (eq .kind "InferencePolicy") (eq .metadata.name "sre-inference") -}}
{{- if hasKey .spec "tokenBudget" -}}
{{- fail "Unbounded Kind SRE policy must omit all token ceilings" -}}
{{- end -}}
{{/* Only restore the sealed budget for comparison. Funding, registration and
     adoption are still validated below; emitted resources remain capless. */}}
{{- if $originalBudget -}}{{- $_ := set .spec "tokenBudget" (deepCopy $originalBudget) -}}{{- end -}}
{{- end -}}
{{- end -}}
{{- else -}}
{{- $originalDaily := dig "tokenBudget" "dailyTokens" 0 .spec -}}
{{- $selectedDaily := $root.Values.sre.dailyTokens -}}
{{- if ne (toJson $selectedDaily) (toJson $originalDaily) -}}
{{- if or (not $adoption) (not (regexMatch "^[1-9][0-9]{0,15}$" (toJson $selectedDaily)))
  (not (regexMatch "^[1-9][0-9]{0,15}$" (toJson $originalDaily)))
  (le (int64 $selectedDaily) (int64 $originalDaily)) (gt (int64 $selectedDaily) 9007199254740991) -}}
{{- fail "SRE daily calendar limit changes require completed adoption and a finite integer increase; lifetime funding remains separate" -}}
{{- end -}}
{{- $_ := set $upgradeValues.sre "dailyTokens" $originalDaily -}}
{{- range $comparisonResources -}}
{{- if and (eq .kind "InferencePolicy") (eq .metadata.name "sre-inference") -}}
{{- if ne (toJson .spec.tokenBudget.dailyTokens) (toJson $selectedDaily) -}}
{{- fail "SRE daily calendar policy must match the selected Helm limit" -}}
{{- end -}}
{{- $_ := set .spec.tokenBudget "dailyTokens" $originalDaily -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- $_ := unset $upgradeValues.controller "image" -}}
{{- if hasKey ($root.Values.foundry | default dict) "projectEndpointBaseline" -}}
{{- $baseline := $root.Values.foundry.projectEndpointBaseline -}}
{{- $selected := $root.Values.foundry.projectEndpoint -}}
{{- if or (not $adoption) (not (kindIs "string" $baseline)) (not (kindIs "string" $selected)) -}}
{{- fail "Foundry project configuration requires completed SRE adoption and the original string endpoint baseline" -}}
{{- end -}}
{{- if not (regexMatch "^https://[a-z0-9][a-z0-9-]*[.]services[.]ai[.]azure[.]com/api/projects/[A-Za-z0-9][A-Za-z0-9_-]*/?$" $selected) -}}
{{- fail "Foundry project configuration requires a canonical HTTPS Azure Foundry project endpoint without credentials, query or fragment" -}}
{{- end -}}
{{/* The supplied original must still reproduce the sealed adoption digest. Only
     the project URL changes: inference, credentials, grants and funding stay sealed. */}}
{{- $_ := set $upgradeValues.foundry "projectEndpoint" $baseline -}}
{{- $_ := unset $upgradeValues.foundry "projectEndpointBaseline" -}}
{{- end -}}
{{/* Cold installs may already include image and monitoring baselines. They are
     not transitions when the entire original intent still matches exactly. */}}
{{- $initialValues := deepCopy $root.Values -}}
{{- $_ := set $initialValues "inferenceBudget" $comparisonBudget -}}
{{- $_ := set $initialValues.schemaHook "enabled" true -}}
{{- $sealedInitial := eq ($initialValues | toJson | sha256sum) $intent.valuesDigest -}}
{{- if hasKey ($root.Values.evals | default dict) "runnerImageBaseline" -}}
{{- $baseline := $root.Values.evals.runnerImageBaseline -}}
{{- $selected := $root.Values.evals.runnerImage -}}
{{- if or (and (not $adoption) (not $sealedInitial)) (not (kindIs "string" $baseline)) (not (kindIs "string" $selected)) -}}
{{- fail "Eval runner refresh requires completed SRE adoption and string image references" -}}
{{- end -}}
{{- if or (not (regexMatch "^[a-zA-Z0-9][a-zA-Z0-9._:/-]*:latest(@sha256:[a-f0-9]{64})?$" $baseline))
  (not (regexMatch "^[a-zA-Z0-9][a-zA-Z0-9._:/-]*:latest@sha256:[a-f0-9]{64}$" $selected)) -}}
{{- fail "Eval runner refresh requires the original latest image reference and an immutable selected latest@sha256 image" -}}
{{- end -}}
{{/* The original runner may be the public mutable default. Its exact value must
     still reproduce the sealed adoption digest; only the selected runner changes. */}}
{{- $_ := set $upgradeValues.evals "runnerImage" $baseline -}}
{{- $_ := unset $upgradeValues.evals "runnerImageBaseline" -}}
{{- end -}}
{{- if hasKey ($root.Values.headlamp | default dict) "imageBaseline" -}}
{{- $baseline := $root.Values.headlamp.imageBaseline -}}
{{- $image := $root.Values.headlamp.image -}}
{{- if or (and (not $adoption) (not $sealedInitial)) (not (kindIs "string" $baseline)) (not (kindIs "map" $image)) -}}
{{- fail "Headlamp image refresh requires completed SRE adoption and the original string image baseline" -}}
{{- end -}}
{{- if or (not (kindIs "string" $image.repository)) (not (kindIs "string" $image.tag)) -}}
{{- fail "Headlamp image repository and tag must be strings" -}}
{{- end -}}
{{- $selected := printf "%s:%s" $image.repository $image.tag -}}
{{- if or (not (regexMatch "^[a-zA-Z0-9][a-zA-Z0-9._:/-]*:latest(@sha256:[a-f0-9]{64})?$" $baseline))
  (not (regexMatch "^latest@sha256:[a-f0-9]{64}$" $image.tag))
  (not (regexMatch "^[a-zA-Z0-9][a-zA-Z0-9._:/-]*:latest@sha256:[a-f0-9]{64}$" $selected)) -}}
{{- fail "Headlamp image refresh requires the original latest image reference and an immutable selected latest@sha256 image" -}}
{{- end -}}
{{/* Only the image tuple is projected back; OIDC, credentials, ServiceAccount
     and all other Headlamp configuration remain bound to the original receipt. */}}
{{- $originalTag := regexFind ":latest(@sha256:[a-f0-9]{64})?$" $baseline -}}
{{- $_ := set $upgradeValues.headlamp.image "repository" (trimSuffix $originalTag $baseline) -}}
{{- $_ := set $upgradeValues.headlamp.image "tag" (trimPrefix ":" $originalTag) -}}
{{- $_ := unset $upgradeValues.headlamp "imageBaseline" -}}
{{- end -}}
{{- if hasKey ($root.Values.monitoring | default dict) "kubeStateMetricsResourcesBaseline" -}}
{{- $monitoring := $root.Values.monitoring -}}
{{- $baseline := $monitoring.kubeStateMetricsResourcesBaseline -}}
{{- $selected := dig "kube-state-metrics" "resources" dict $monitoring -}}
{{- if or (not $adoption) (ne (get ($root.Values.global | default dict) "installationProfile") "kind-beta")
  (ne (get $monitoring "enabled" | toJson) "true") (not (kindIs "map" $baseline)) (not (kindIs "map" $selected)) -}}
{{- fail "Kind kube-state-metrics capacity refresh requires completed adoption, enabled monitoring and a sealed resources baseline" -}}
{{- end -}}
{{- range $resources := list $baseline $selected -}}
{{- if ne (keys $resources | sortAlpha | toJson) (list "limits" "requests" | toJson) -}}
{{- fail "Kind kube-state-metrics resources require exactly requests and limits" -}}
{{- end -}}
{{- range $group := list "requests" "limits" -}}
{{- $quantities := get $resources $group -}}
{{- if not (kindIs "map" $quantities) -}}{{- fail "Kind kube-state-metrics quantities must be objects" -}}{{- end -}}
{{- if ne (keys $quantities | sortAlpha | toJson) (list "cpu" "memory" | toJson) -}}
{{- fail "Kind kube-state-metrics quantities require exactly CPU and memory" -}}
{{- end -}}
{{- range $resource, $suffix := dict "cpu" "m" "memory" "Mi" -}}
{{- $quantity := get $quantities $resource -}}
{{- if not (kindIs "string" $quantity) -}}{{- fail "Kind kube-state-metrics quantities must be strings" -}}{{- end -}}
{{- if not (regexMatch (printf "^[1-9][0-9]{0,8}%s$" $suffix) $quantity) -}}
{{- fail "Kind kube-state-metrics quantities require bounded positive integer millicores and MiB" -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- range $resource, $suffix := dict "cpu" "m" "memory" "Mi" -}}
{{- if gt (int64 (trimSuffix $suffix (get $resources.requests $resource))) (int64 (trimSuffix $suffix (get $resources.limits $resource))) -}}
{{- fail "Kind kube-state-metrics requests must not exceed limits" -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- range $group := list "requests" "limits" -}}
{{- range $resource, $suffix := dict "cpu" "m" "memory" "Mi" -}}
{{- if lt (int64 (trimSuffix $suffix (get (get $selected $group) $resource))) (int64 (trimSuffix $suffix (get (get $baseline $group) $resource))) -}}
{{- fail "Kind kube-state-metrics readiness repair must not reduce the sealed capacity" -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{/* Only resource quantities change; the original adoption hash still seals
     images, probes, selectors, credentials and every other monitoring setting. */}}
{{- $_ := set (get $upgradeValues.monitoring "kube-state-metrics") "resources" (deepCopy $baseline) -}}
{{- $_ := unset $upgradeValues.monitoring "kubeStateMetricsResourcesBaseline" -}}
{{- end -}}
{{- if hasKey ($root.Values.monitoring | default dict) "grafanaBaseline" -}}
{{- $baseline := $root.Values.monitoring.grafanaBaseline -}}
{{- $grafana := $root.Values.monitoring.grafana -}}
{{- if or (and (not $adoption) (not $sealedInitial)) (not (kindIs "map" $baseline)) (not (kindIs "map" $grafana)) -}}
{{- fail "Grafana lifecycle changes require completed SRE adoption and the original sealed Grafana baseline" -}}
{{- end -}}
{{- if or (not (kindIs "bool" $baseline.enabled)) (not (kindIs "bool" $grafana.enabled)) -}}
{{- fail "Grafana baseline and selected enabled values must be booleans" -}}
{{- end -}}
{{- range $pair := list (dict "original" $baseline.image "selected" $grafana.image) (dict "original" (dig "sidecar" "image" dict $baseline) "selected" (dig "sidecar" "image" dict $grafana)) -}}
{{- if or (not (kindIs "map" $pair.original)) (not (kindIs "map" $pair.selected)) -}}
{{- fail "Grafana and sidecar image baselines require registry, repository and tag objects" -}}
{{- end -}}
{{- range $image := list $pair.original $pair.selected -}}
{{- range $field := list "registry" "repository" "tag" -}}
{{- if or (not (hasKey $image $field)) (not (kindIs "string" (get $image $field))) -}}{{- fail "Grafana image registry, repository and tag must be explicit strings" -}}{{- end -}}
{{- end -}}
{{- $reference := printf "%s:%s" (trimPrefix "/" (printf "%s/%s" $image.registry $image.repository)) $image.tag -}}
{{- if not (regexMatch "^[a-zA-Z0-9][a-zA-Z0-9._:/-]*:latest(@sha256:[a-f0-9]{64})?$" $reference) -}}
{{- fail "Grafana image baselines and selected images require valid latest image references" -}}
{{- end -}}
{{- end -}}
{{- if not (regexMatch "^latest@sha256:[a-f0-9]{64}$" $pair.selected.tag) -}}
{{- fail "Selected Grafana and sidecar images must be immutable latest@sha256 references" -}}
{{- end -}}
{{- end -}}
{{/* Helm adds dependency defaults only while Grafana is enabled. Compare both
     configurations with the locked defaults before restoring the exact sealed
     baseline; this must not discard non-default auth, RBAC or resource changes. */}}
{{- $defaults := required "Grafana lifecycle comparison requires the locked dependency defaults" .grafanaDefaults -}}
{{- $expected := mergeOverwrite (deepCopy $defaults) (deepCopy $baseline) -}}
{{- $selected := mergeOverwrite (deepCopy $defaults) (deepCopy $grafana) -}}
{{- $_ := set $expected "enabled" $grafana.enabled -}}
{{- if and $grafana.enabled (ne $selected.rbac.namespaced true) -}}
{{- fail "Grafana lifecycle requires namespace-scoped RBAC, not cluster-wide Secret access" -}}
{{- end -}}
{{- if and (eq $expected.rbac.namespaced false) (eq $selected.rbac.namespaced true) -}}
{{- $_ := set $expected.rbac "namespaced" true -}}
{{- end -}}
{{- range $field := list "registry" "repository" "tag" -}}
{{- $_ := set $expected.image $field (get $grafana.image $field) -}}
{{- $_ := set $expected.sidecar.image $field (get $grafana.sidecar.image $field) -}}
{{- end -}}
{{- range $config := list $expected $selected -}}
{{- $_ := set $config "global" (mergeOverwrite (deepCopy ($config.global | default dict)) (deepCopy ($root.Values.monitoring.global | default dict))) -}}
{{- end -}}
{{- if ne ($expected | toJson) ($selected | toJson) -}}
{{- fail "Grafana lifecycle permits only enabled, image tuples and namespace-scope narrowing; preserve authentication, other RBAC, networking, resources and other configuration" -}}
{{- end -}}
{{- $_ := set $upgradeValues.monitoring "grafana" (deepCopy $baseline) -}}
{{- $_ := unset $upgradeValues.monitoring "grafanaBaseline" -}}
{{- end -}}
{{- if hasKey ($root.Values.monitoring | default dict) "querySubjectsBaseline" -}}
{{- if and (not $adoption) (not $sealedInitial) -}}{{- fail "Monitoring query subject changes require completed SRE adoption" -}}{{- end -}}
{{- range $field := list "querySubjectsBaseline" "querySubjects" -}}
{{- $subjects := get $root.Values.monitoring $field -}}
{{- if not (kindIs "slice" $subjects) -}}{{- fail (printf "monitoring.%s must be an explicit list" $field) -}}{{- end -}}
{{- $seen := dict -}}
{{- range $subject := $subjects -}}
{{- if not (kindIs "map" $subject) -}}{{- fail (printf "monitoring.%s subjects must be objects" $field) -}}{{- end -}}
{{- if or (ne (len $subject) 2) (not (kindIs "string" $subject.kind)) (not (kindIs "string" $subject.name)) -}}
{{- fail (printf "monitoring.%s subjects require exactly string kind and name fields" $field) -}}
{{- end -}}
{{- if or (not (has $subject.kind (list "User" "Group"))) (not (regexMatch "^\\S(?:[^\\r\\n]*\\S)?$" $subject.name)) -}}
{{- fail (printf "monitoring.%s permits only named User or Group subjects without surrounding whitespace or newlines" $field) -}}
{{- end -}}
{{- $key := toJson $subject -}}
{{- if hasKey $seen $key -}}{{- fail (printf "monitoring.%s subjects must be unique" $field) -}}{{- end -}}
{{- $_ := set $seen $key true -}}
{{- end -}}
{{- end -}}
{{/* Empty lists are explicit revocation, not a missing baseline. The exact
     original list must still reproduce the sealed digest; proxy scope is fixed. */}}
{{- $_ := set $upgradeValues.monitoring "querySubjects" $root.Values.monitoring.querySubjectsBaseline -}}
{{- $_ := unset $upgradeValues.monitoring "querySubjectsBaseline" -}}
{{- end -}}
{{- if hasKey $root.Values.sre "runtimeImageBaseline" -}}
{{- $baseline := $root.Values.sre.runtimeImageBaseline -}}
{{- if or (not $adoption) (not (kindIs "map" $baseline)) -}}
{{- fail "Runtime image refresh requires completed SRE adoption and the original sealed image baseline" -}}
{{- end -}}
{{- if or (ne (len $baseline) 2) (not (hasKey $baseline "openclaw")) (not (hasKey $baseline "hermes")) -}}
{{- fail "Runtime image baseline requires exactly the original openclaw and hermes image references" -}}
{{- end -}}
{{- $openclaw := printf "%s:%s" (dig "image" "repository" "" ($root.Values.sandbox | default dict)) (dig "image" "tag" "" ($root.Values.sandbox | default dict)) -}}
{{- $hermes := dig "hermes" "image" "" ($root.Values.runtimes | default dict) -}}
{{- range list $baseline.openclaw $baseline.hermes $openclaw $hermes -}}
{{- if not (kindIs "string" .) -}}{{- fail "Runtime images must be strings" -}}{{- end -}}
{{- if not (regexMatch "^[a-zA-Z0-9][a-zA-Z0-9._:/-]*:latest@sha256:[a-f0-9]{64}$" .) -}}
{{- fail "Runtime image refresh requires immutable latest@sha256 references for both original and selected images" -}}
{{- end -}}
{{- end -}}
{{- if or (ne (first (splitList "@" $baseline.openclaw)) (first (splitList "@" $openclaw)))
  (ne (first (splitList "@" $baseline.hermes)) (first (splitList "@" $hermes))) -}}
{{/* As with router continuity, retain only the exact installed runtime tuple
     after a reviewed migration; a new repository cannot be selected here. */}}
{{- $installed := dict -}}
{{- $controllerCount := 0 -}}
{{- range dig "spec" "template" "spec" "containers" list .controller -}}
{{- if eq .name "controller" -}}
{{- $controllerCount = add1 $controllerCount -}}
{{- range .env | default list -}}
{{- if has .name (list "SANDBOX_IMAGE" "HERMES_RUNTIME_IMAGE") -}}
{{- if or (hasKey $installed .name) (ne (len .) 2) (not (kindIs "string" .value)) -}}
{{- fail "Installed runtime continuity requires unique literal image declarations" -}}
{{- end -}}
{{- $_ := set $installed .name .value -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- if or (ne (int $controllerCount) 1) .controller.metadata.deletionTimestamp
  (ne (dig "metadata" "name" "" .controller) "kars-controller")
  (ne (dig "metadata" "namespace" "" .controller) $root.Release.Namespace)
  (ne ($installed.SANDBOX_IMAGE | default "") $openclaw)
  (ne ($installed.HERMES_RUNTIME_IMAGE | default "") $hermes) -}}
{{- fail "Runtime repository continuity requires the exact already-installed immutable images on the original controller" -}}
{{- end -}}
{{- end -}}
{{/* Restore only the exact original image references for the sealed comparison;
     identities, runtime configuration and funding must still match below. */}}
{{- $_ := set $upgradeValues.sandbox.image "repository" (trimSuffix (printf ":latest@%s" (last (splitList "@" $baseline.openclaw))) $baseline.openclaw) -}}
{{- $_ := set $upgradeValues.sandbox.image "tag" (printf "latest@%s" (last (splitList "@" $baseline.openclaw))) -}}
{{- if hasKey $root.Values.sandbox.image "digest" -}}
{{- if ne (toJson $root.Values.sandbox.image.digest) (toJson (last (splitList "@" $openclaw))) -}}
{{- fail "Runtime image digest must match the selected immutable OpenClaw reference" -}}
{{- end -}}
{{- $_ := set $upgradeValues.sandbox.image "digest" (last (splitList "@" $baseline.openclaw)) -}}
{{- end -}}
{{- $_ := set $upgradeValues.runtimes.hermes "image" $baseline.hermes -}}
{{- $_ := unset $upgradeValues.sre "runtimeImageBaseline" -}}
{{- end -}}
{{- if hasKey $root.Values.sre "routerImageBaseline" -}}
{{- $baseline := $root.Values.sre.routerImageBaseline -}}
{{- if or (not $adoption) (not (kindIs "string" $baseline)) -}}
{{- fail "Router image refresh requires completed SRE adoption and the original sealed router image baseline" -}}
{{- end -}}
{{- $selected := printf "%s:%s" $root.Values.inferenceRouter.image.repository $root.Values.inferenceRouter.image.tag -}}
{{- range list $baseline $selected -}}
{{- if not (regexMatch "^[a-zA-Z0-9][a-zA-Z0-9._:/-]*:latest@sha256:[a-f0-9]{64}$" .) -}}
{{- fail "Router image refresh requires immutable latest@sha256 references for both original and selected images" -}}
{{- end -}}
{{- end -}}
{{- $originalDigest := last (splitList "@" $baseline) -}}
{{- $sameRepository := eq (first (splitList "@" $baseline)) (first (splitList "@" $selected)) -}}
{{- if not $sameRepository -}}
{{/* A reviewed private-root migration can precede Core publication. Carry forward
     only the exact installed router tuple on the original controller; this is not
     permission to select a new repository. The sealed original is still hashed. */}}
{{- $installed := dict -}}
{{- $controllerCount := 0 -}}
{{- range dig "spec" "template" "spec" "containers" list .controller -}}
{{- if eq .name "controller" -}}
{{- $controllerCount = add1 $controllerCount -}}
{{- range .env | default list -}}
{{- if has .name (list "INFERENCE_ROUTER_IMAGE" "KARS_INFERENCE_BUDGET_ROUTER_DIGEST" "KARS_INFERENCE_BUDGET_FIRST_INSTALL_ROUTER_DIGEST") -}}
{{- if or (hasKey $installed .name) (ne (len .) 2) (not (kindIs "string" .value)) -}}
{{- fail "Installed router continuity requires unique literal image and verifier declarations" -}}
{{- end -}}
{{- $_ := set $installed .name .value -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- if or (ne (int $controllerCount) 1) .controller.metadata.deletionTimestamp
  (ne (dig "metadata" "name" "" .controller) "kars-controller")
  (ne (dig "metadata" "namespace" "" .controller) $root.Release.Namespace)
  (ne ($installed.INFERENCE_ROUTER_IMAGE | default "") $selected)
  (ne ($installed.KARS_INFERENCE_BUDGET_ROUTER_DIGEST | default "") $root.Values.inferenceBudget.routerImageDigest)
  (ne ($installed.KARS_INFERENCE_BUDGET_FIRST_INSTALL_ROUTER_DIGEST | default "") $originalDigest) -}}
{{- fail "Router repository continuity requires the exact already-installed immutable image and verifier tuple on the original controller" -}}
{{- end -}}
{{- end -}}
{{- if or (ne $originalDigest $intent.budget.routerImageDigest)
  (ne (last (splitList "@" $selected)) $root.Values.inferenceBudget.routerImageDigest) -}}
{{- fail "Router image refresh must prove the sealed original digest and match the selected budget router digest" -}}
{{- end -}}
{{/* Restore the exact original repository and tag for the sealed comparison;
     all other image/runtime fields, identities, credentials and grants stay bound. */}}
{{- $_ := set $upgradeValues.inferenceRouter.image "repository" (trimSuffix (printf ":latest@%s" $originalDigest) $baseline) -}}
{{- $_ := set $upgradeValues.inferenceRouter.image "tag" (printf "latest@%s" $originalDigest) -}}
{{- if hasKey $root.Values.inferenceRouter.image "digest" -}}
{{- if ne (toJson $root.Values.inferenceRouter.image.digest) (toJson (last (splitList "@" $selected))) -}}
{{- fail "Router image digest must match the selected immutable router reference" -}}
{{- end -}}
{{- $_ := set $upgradeValues.inferenceRouter.image "digest" $originalDigest -}}
{{- end -}}
{{- $_ := set $comparisonBudget "routerImageDigest" $originalDigest -}}
{{- $_ := set $upgradeValues.inferenceBudget "routerImageDigest" $originalDigest -}}
{{- $_ := unset $upgradeValues.sre "routerImageBaseline" -}}
{{- end -}}
{{- if hasKey ($root.Values.metricsServer | default dict) "hostAliasesBaseline" -}}
{{- $metrics := $root.Values.metricsServer -}}
{{- $baseline := $metrics.hostAliasesBaseline -}}
{{- $selected := $metrics.hostAliases -}}
{{- if or (not $adoption) (ne (($root.Values.global | default dict).installationProfile | default "") "kind-beta") (ne (toJson $metrics.enabled) "true") -}}
{{- fail "Metrics node address refresh requires adopted Kind beta metrics-server and its sealed host aliases" -}}
{{- end -}}
{{- if or (not (kindIs "slice" $baseline)) (not (kindIs "slice" $selected)) (empty $baseline) (ne (len $baseline) (len $selected)) -}}
{{- fail "Metrics node address refresh must preserve the original nonempty host-alias list" -}}
{{- end -}}
{{- $seen := dict -}}
{{- range $index, $original := $baseline -}}
{{- $alias := index $selected $index -}}
{{- range list $original $alias -}}
{{- if or (not (kindIs "map" .)) (ne (len .) 2) (not (kindIs "string" .ip)) (empty .ip)
  (not (kindIs "slice" .hostnames)) (ne (len .hostnames) 1) -}}
{{- fail "Metrics host aliases require exactly an IP and one node hostname" -}}
{{- end -}}
{{- end -}}
{{- $name := first $original.hostnames -}}
{{- if or (not (kindIs "string" $name)) (empty $name) (ne (toJson $original.hostnames) (toJson $alias.hostnames)) (hasKey $seen $name) -}}
{{- fail "Metrics node address refresh must preserve unique original node hostnames" -}}
{{- end -}}
{{- $_ := set $seen $name true -}}
{{- $node := get ($.metricsNodes | default dict) $name | default dict -}}
{{- if or (ne (get $node "apiVersion") "v1") (ne (get $node "kind") "Node")
  (ne (dig "metadata" "name" "" $node) $name) (empty (dig "metadata" "uid" "" $node)) (dig "metadata" "deletionTimestamp" "" $node) -}}
{{- fail "Metrics node address refresh requires the live original named Node" -}}
{{- end -}}
{{- $internal := false -}}
{{- range (dig "status" "addresses" list $node) -}}
{{- if and (eq .type "InternalIP") (eq .address $alias.ip) -}}{{- $internal = true -}}{{- end -}}
{{- end -}}
{{- if not $internal -}}{{- fail "Metrics host alias must select the live node InternalIP" -}}{{- end -}}
{{- end -}}
{{- $_ := set $upgradeValues.metricsServer "hostAliases" (deepCopy $baseline) -}}
{{- $_ := unset $upgradeValues.metricsServer "hostAliasesBaseline" -}}
{{- end -}}
{{- if hasKey ($root.Values.missionDispatcher | default dict) "imageBaseline" -}}
{{- $mission := $root.Values.missionDispatcher -}}
{{- $baseline := $mission.imageBaseline -}}
{{- $selected := $mission.image | default dict -}}
{{- if or (not $adoption) (ne (toJson $mission.enabled) "true") (not (kindIs "map" $baseline)) (not (kindIs "map" $selected)) -}}
{{- fail "Dispatcher image refresh requires completed SRE adoption, an enabled dispatcher and the original sealed image baseline" -}}
{{- end -}}
{{- range list $baseline $selected -}}
{{- if or (not (hasKey . "repository")) (not (hasKey . "digest"))
  (ne (len .) (ternary 3 2 (hasKey . "tag")))
  (not (kindIs "string" .repository)) (not (kindIs "string" .digest)) -}}
{{- fail "Dispatcher images require exactly repository, digest and an optional matching immutable latest tag" -}}
{{- end -}}
{{- if or (not (regexMatch "^[a-zA-Z0-9][a-zA-Z0-9._:/-]*$" .repository))
  (not (regexMatch "^sha256:[a-f0-9]{64}$" .digest)) -}}
{{- fail "Dispatcher image refresh requires immutable image references" -}}
{{- end -}}
{{- if and (hasKey . "tag") (ne (toJson .tag) (toJson (printf "latest@%s" .digest))) -}}
{{- fail "Dispatcher image tag must match its immutable digest" -}}
{{- end -}}
{{- end -}}
{{- if and (ne $baseline.repository $selected.repository)
  (or (ne (first (splitList "/" $baseline.repository)) (first (splitList "/" $selected.repository)))
    (not (regexMatch "^(localhost|[^/]+[.:][^/]*)/[^/]+" $baseline.repository))) -}}
{{- fail "Dispatcher image refresh must preserve the original explicit registry" -}}
{{- end -}}
{{- if ne (hasKey $baseline "tag") (hasKey $selected "tag") -}}
{{- fail "Dispatcher image refresh must preserve the original image fields" -}}
{{- end -}}
{{/* The original image must reproduce the sealed receipt, not merely the last deployment. */}}
{{- $_ := set $upgradeValues.missionDispatcher "image" (deepCopy $baseline) -}}
{{- $_ := unset $upgradeValues.missionDispatcher "imageBaseline" -}}
{{- end -}}
{{- if hasKey $root.Values.sre "datapathWitnessBaseline" -}}
{{- $baseline := $root.Values.sre.datapathWitnessBaseline -}}
{{- $selected := $root.Values.datapathWitness | default dict -}}
{{- if or (not $adoption) (not (kindIs "map" $baseline)) (not (kindIs "map" $selected)) -}}
{{- fail "Datapath witness changes require completed SRE adoption and the original sealed witness baseline" -}}
{{- end -}}
{{- $imageDomain := "[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?([.][a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?)*(:[0-9]+)?" -}}
{{- $imagePath := "[a-z0-9]+(([._]|__|-+)[a-z0-9]+)*" -}}
{{- $imagePattern := printf "^(%s/)?%s(/%s)*(:[a-zA-Z0-9_][a-zA-Z0-9_.-]{0,127})?@sha256:[a-f0-9]{64}$" $imageDomain $imagePath $imagePath -}}
{{- range $config := list $baseline $selected -}}
{{- if or (ne (toJson $config.enabled) "true") (ne (toJson $config.managed) "true") -}}
{{- fail "Datapath witness transition requires an already enabled, Core-managed witness" -}}
{{- end -}}
{{/* Absence in an older sealed receipt means selected, never the new all default. */}}
{{- $scope := "selected" -}}
{{- if hasKey $config "scope" -}}{{- $scope = $config.scope -}}{{- end -}}
{{- if or (not (kindIs "string" $scope)) (not (has $scope (list "all" "selected"))) (not (kindIs "slice" $config.sandboxes)) -}}
{{- fail "Datapath witness requires scope all or selected and an explicit sandbox list" -}}
{{- end -}}
{{- if or (and (eq $scope "all") (ne (len $config.sandboxes) 0)) (and (eq $scope "selected") (eq (len $config.sandboxes) 0)) (gt (len $config.sandboxes) 50) -}}
{{- fail "Datapath witness all scope requires an empty list; selected requires 1-50 unique sandbox names" -}}
{{- end -}}
{{- $seen := dict -}}
{{- range $name := $config.sandboxes -}}
{{- if not (kindIs "string" $name) -}}{{- fail "Datapath witness sandbox names must be strings" -}}{{- end -}}
{{- if or (gt (len $name) 58) (not (regexMatch "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$" $name)) (hasKey $seen $name) -}}
{{- fail "Datapath witness requires unique valid sandbox names" -}}
{{- end -}}
{{- $_ := set $seen $name true -}}
{{- end -}}
{{- range $field := list "aggregator" "gadget" -}}
{{- $image := dig $field "image" "" $config -}}
{{- if not (kindIs "string" $image) -}}{{- fail "Datapath witness images must be strings" -}}{{- end -}}
{{- if not (regexMatch $imagePattern $image) -}}
{{- fail "Datapath witness refresh requires valid immutable original and selected image references" -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- $expected := deepCopy $baseline -}}
{{- if hasKey $selected "scope" -}}
{{- $_ := set $expected "scope" $selected.scope -}}
{{- end -}}
{{- $_ := set $expected "sandboxes" $selected.sandboxes -}}
{{- range $field := list "aggregator" "gadget" -}}
{{- $_ := set (get $expected $field) "image" (dig $field "image" "" $selected) -}}
{{- end -}}
{{- if ne ($expected | toJson) ($selected | toJson) -}}
{{- fail "Datapath witness permits only scope, sandbox list and digest-pinned runtime image reference changes; preserve nodes, hook mode, DNS/TCP artifacts, resources, authentication and all other configuration" -}}
{{- end -}}
{{/* Restore the exact original shape, including an absent legacy scope, only for
     hashing. A forged or last-upgrade baseline cannot replace the sealed receipt. */}}
{{- $_ := set $upgradeValues "datapathWitness" (deepCopy $baseline) -}}
{{- $_ := unset $upgradeValues.sre "datapathWitnessBaseline" -}}
{{- end -}}
{{- $upgradeDigest := $upgradeValues | toJson | sha256sum -}}
{{- if $root.Values.schemaHook.enabled -}}
{{- fail "An ordinary SRE release cannot re-enable the first-install schema hook" -}}
{{- end -}}
{{- if $adoption -}}
{{/* Pull coverage is operator metadata read by Bridge, not runtime or funding configuration.
     Permit adding this one literal declaration while retaining the sealed receipt. */}}
{{- if ne (dig "data" "valuesDigest" "" $adoption) $upgradeDigest -}}
{{/* A separately UID-bound Bridge grant does not alter SRE funding. Try the
     original values and a projection without that actor; every other value must
     still reproduce the immutable adoption digest. Controller enrollment validates
     the grant against the live source, namespace and account before any inference. */}}
{{- $candidates := list $upgradeValues -}}
{{- $funding := $upgradeValues.inferenceBudget.builtInFunding | default list -}}
{{- if not (kindIs "slice" $funding) -}}{{- fail "inferenceBudget.builtInFunding must be a list" -}}{{- end -}}
{{- $remainingFunding := list -}}
{{- $orchestratorCount := 0 -}}
{{- range $funding -}}
{{- if and (eq (dig "root" "kind" "" .) "KarsSandbox")
  (eq (dig "root" "resource" "name" "" .) "bridge-orchestrator")
  (eq (dig "root" "resource" "namespace" "" .) $root.Release.Namespace) -}}
{{- $orchestratorCount = add1 $orchestratorCount -}}
{{- else -}}{{- $remainingFunding = append $remainingFunding . -}}{{- end -}}
{{- end -}}
{{- if gt (int $orchestratorCount) 1 -}}{{- fail "Only one Bridge orchestrator funding declaration is permitted" -}}{{- end -}}
{{- if eq (int $orchestratorCount) 1 -}}
{{- $projected := deepCopy $upgradeValues -}}
{{- $_ := set $projected.inferenceBudget "builtInFunding" $remainingFunding -}}
{{- $candidates = append $candidates $projected -}}
{{- if not $remainingFunding -}}
{{- $absent := deepCopy $projected -}}
{{- $_ := unset $absent.inferenceBudget "builtInFunding" -}}
{{- $candidates = append $candidates $absent -}}
{{- end -}}
{{- end -}}
{{- range $candidate := $candidates -}}
{{- if eq ($candidate | toJson | sha256sum) (dig "data" "valuesDigest" "" $adoption) -}}
{{- $upgradeDigest = dig "data" "valuesDigest" "" $adoption -}}
{{- end -}}
{{- $comparison := deepCopy $candidate -}}
{{- $remaining := list -}}
{{- $coverageCount := 0 -}}
{{- range $comparison.controller.extraEnv | default list -}}
{{- if eq (.name | default "") "IMAGE_PULL_REGISTRIES" -}}
{{- $coverageCount = add1 $coverageCount -}}
{{- if or (ne (len .) 2) (not (kindIs "string" .value)) (empty (.value | default "")) -}}
{{- fail "Runtime pull coverage requires one nonempty literal IMAGE_PULL_REGISTRIES declaration" -}}
{{- end -}}
{{- else -}}
{{- $remaining = append $remaining . -}}
{{- end -}}
{{- end -}}
{{- $_ := set $comparison.controller "extraEnv" $remaining -}}
{{- if and (eq (int $coverageCount) 1) (eq ($comparison | toJson | sha256sum) (dig "data" "valuesDigest" "" $adoption)) -}}
{{- $upgradeDigest = dig "data" "valuesDigest" "" $adoption -}}
{{- end -}}
{{/* A separate Kind dispatcher may be activated from the sealed disabled defaults.
     Compare the entire remaining configuration, including any original pull declaration. */}}
{{- $mission := $root.Values.missionDispatcher | default dict -}}
{{- if and (eq (($root.Values.global | default dict).installationProfile | default "") "kind-beta")
  (eq (len $mission) 5) (eq (toJson $mission.enabled) "true") (eq $mission.authMode "development")
  (eq $mission.relayUrl "") (eq $mission.registryUrl "")
  (eq (len ($mission.image | default dict)) 2)
  (regexMatch "^[a-zA-Z0-9._:/-]+$" ($mission.image.repository | default ""))
  (regexMatch "^sha256:[a-f0-9]{64}$" ($mission.image.digest | default "")) -}}
{{- $disabled := dict "enabled" false "authMode" "" "image" (dict "repository" "karsacr.azurecr.io/kars-mission-dispatcher" "digest" "") "relayUrl" "" "registryUrl" "" -}}
{{- $missionComparison := deepCopy $candidate -}}
{{- $_ := set $missionComparison "missionDispatcher" $disabled -}}
{{- $_ := set $comparison "missionDispatcher" $disabled -}}
{{- if or (eq ($missionComparison | toJson | sha256sum) (dig "data" "valuesDigest" "" $adoption))
  (and (eq (int $coverageCount) 1) (eq ($comparison | toJson | sha256sum) (dig "data" "valuesDigest" "" $adoption))) -}}
{{- $upgradeDigest = dig "data" "valuesDigest" "" $adoption -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- if or (not $adoption.immutable) $adoption.metadata.deletionTimestamp
  (ne (dig "metadata" "annotations" "meta.helm.sh/release-name" "" $adoption) $root.Release.Name)
  (ne (dig "metadata" "annotations" "meta.helm.sh/release-namespace" "" $adoption) $root.Release.Namespace)
  (ne (dig "data" "intentUID" "" $adoption) $prior.metadata.uid)
  (ne (dig "data" "valuesDigest" "" $adoption) $upgradeDigest) -}}
{{- fail "SRE ordinary upgrade requires the original adoption receipt and unchanged values except supported controller, sealed-baseline runtime/router/eval-runner/Headlamp image, sealed-baseline monitoring query subjects and Grafana lifecycle, sealed-baseline Kind kube-state-metrics capacity and metrics node addresses, pull-coverage, Kind dispatcher activation, sealed-baseline dispatcher image, sealed-baseline datapath witness scope/list/image, sealed-baseline Foundry project and SRE primary model selection, model-contract renewal, finite daily-limit increase and separately authorized token-top-up/unbounded-allowance transitions" -}}
{{- end -}}
{{- else -}}
{{- $originalValues := deepCopy $root.Values -}}
{{- $_ := set $originalValues "inferenceBudget" $comparisonBudget -}}
{{- $_ := set $originalValues.schemaHook "enabled" true -}}
{{- if ne ($originalValues | toJson | sha256sum) $intent.valuesDigest -}}
{{- fail "First-install SRE ordinary ownership transition permits only disabling schemaHook; preserve the full operator intent" -}}
{{- end -}}
{{- end -}}
{{- if or (not $root.Values.sre.enabled) (not $binding) (ne $binding.intent.uid $prior.metadata.uid) (ne $binding.intent.digest (index $prior.data "intent.json" | sha256sum)) (ne (dig "spec" "sandbox" "uid" "" $registration) (dig "metadata" "uid" "missing" $source)) (ne (dig "spec" "controller" "release" "" $registration) $root.Release.Name) (ne (dig "metadata" "annotations" "meta.helm.sh/release-name" "" $source) $root.Release.Name) (ne (dig "metadata" "annotations" "meta.helm.sh/release-namespace" "" $source) $root.Release.Namespace) (ne ($intent.resources | toJson) ($comparisonResources | toJson)) (ne ($intent.funding | toJson) ($root.Values.sre.firstInstallFunding | toJson)) (ne ($intent.budget | toJson) (omit $comparisonBudget "tls" "builtInFunding" | toJson)) -}}
{{- fail "First-install SRE ownership or intent changed; preserve authority and use explicit reviewed migration" -}}
{{- end -}}
{{/* An adopted intent or independently qualified initial CA transition permits
     repairing Blocked SRE authority. Rendering never qualifies SRE itself; the
     original identities, funding, full initial values and live checks remain. */}}
{{- $phase := dig "status" "phase" "" $registration -}}
{{- $privacyRevision := dig "status" "privacyRevision" "" $registration -}}
{{- $blockedRepair := and (or $adoption $privateInitialAdoption) (eq $phase "Blocked") -}}
{{/* Retirement leaves SRE provisioning until the captured consumers can resume.
     Only the fully validated, owned Pending publication may cross this cycle. */}}
{{- $provisioningPublication := and $privatePendingPublication (eq $phase "Provisioning") -}}
{{- if or (not $prior.immutable) $prior.metadata.deletionTimestamp
  (ne (dig "metadata" "annotations" "meta.helm.sh/release-name" "" $prior) $root.Release.Name)
  (ne (dig "metadata" "annotations" "meta.helm.sh/release-namespace" "" $prior) $root.Release.Namespace)
  (not (or (eq $phase "Ready") $blockedRepair $provisioningPublication))
  (not (or (eq $privacyRevision "kars.azure.com/sre-privacy/v2") (and (or $blockedRepair $provisioningPublication) (eq $privacyRevision ""))))
  (ne (dig "status" "observedGeneration" 0 $registration) (dig "metadata" "generation" -1 $registration))
  (not (dig "spec" "enabled" false $registration))
  (ne (dig "spec" "runtimeNamespace" "uid" "" $registration) (dig "metadata" "uid" "missing" .runtime))
  (ne (dig "spec" "controller" "deployment" "uid" "" $registration) (dig "metadata" "uid" "missing" .controller))
  (ne (dig "spec" "controller" "namespace" "uid" "" $registration) (dig "metadata" "uid" "missing" .workspace))
  (ne (dig "spec" "inferenceBudgetRef" "root" dict $source | toJson) ($binding.grant.root | toJson))
  (ne ($binding.grant.limits.tokens | toJson) ($intent.funding.tokens | toJson))
  (ne ($binding.grant.expiresAt | default "") ($intent.funding.expiresAt | default ""))
  (ne ($binding.grant.unboundedTokens | default false) ($intent.funding.unboundedTokens | default false))
  (and (not $intent.funding.unboundedTokens) (not $adoption) (le (int64 (unixEpoch (toDate "2006-01-02T15:04:05Z07:00" $intent.funding.expiresAt))) (int64 (unixEpoch now)))) -}}
{{- fail "First-install SRE transition requires live original identities and unchanged funding; finite initial adoption also requires unexpired authority" -}}
{{- end -}}
{{- range $custom -}}
{{- if eq .kind "KarsSandbox" -}}
{{- if ne (omit $source.spec "inferenceBudgetRef" | toJson) (.spec | toJson) -}}
{{- fail "The controller-owned funded SRE source must retain its original runtime configuration" -}}
{{- end -}}
{{- range $key, $value := .metadata.labels -}}
{{- if ne (index (default dict $source.metadata.labels) $key | default "") $value -}}
{{- fail "The controller-owned SRE source must retain its intended labels" -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{/* Record release ownership separately; funded source metadata remains controller-only. */}}
{{- $_ := set .receipt "intentUID" $prior.metadata.uid -}}
{{- $_ := set .receipt "valuesDigest" $upgradeDigest -}}
{{- end -}}
