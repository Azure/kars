{{/* Copyright (c) Microsoft Corporation.
Licensed under the MIT License. */}}
{{/*
The credential authority, not bootstrap values, selects console-managed settings.
Render its current UID-bound configuration for both Helm 3 merge and Helm 4 SSA.
Credentials remain Secret references, never copied values in release storage.
*/}}
{{- define "kars.controller.operatorSettings" -}}
{{- with .precondition -}}
  {{- if or (not (kindIs "string" .uid)) (not (kindIs "string" .resourceVersion)) -}}
    {{- fail "controller.upgradePrecondition requires string UID and resourceVersion fences" -}}
  {{- end -}}
  {{- $_ := set $.deployment.metadata "uid" (required "controller.upgradePrecondition.uid is required" .uid) -}}
  {{- $_ := set $.deployment.metadata "resourceVersion" (required "controller.upgradePrecondition.resourceVersion is required" .resourceVersion) -}}
{{- end -}}
{{- $annotation := dig "spec" "template" "metadata" "annotations" "kars.azure.com/credential-settings-revision" "" .live -}}
{{- if $annotation -}}
  {{- $verified := false -}}
  {{- $uid := dig "metadata" "uid" "" .live -}}
  {{- $settingsUid := dig "metadata" "uid" "" .settings -}}
  {{- $version := dig "metadata" "resourceVersion" "" .settings -}}
  {{- $source := printf "kars-credential-controller-settings:%s:%s" $settingsUid $version -}}
  {{- range (.grants.items | default list) -}}
    {{- $controller := .spec.controller | default dict -}}
    {{- $status := .status | default dict -}}
    {{- $revisions := splitList "," ($status.integrationRevision | default "") -}}
    {{- if and (eq ($controller.name | default "") "kars-controller")
          (eq ($controller.uid | default "") $uid)
          (eq ($status.phase | default "") "Ready")
          (eq (toString ($status.observedGeneration | default 0)) (toString .metadata.generation))
          (not $status.integrationError) (has $source $revisions) (has $annotation $revisions) -}}
      {{- range (.spec.integrationStores | default list) -}}
        {{- if and (eq .purpose "controller-settings")
              (eq .secret.name "kars-credential-controller-settings")
              (eq .secret.uid $settingsUid) -}}
          {{- $verified = true -}}
        {{- end -}}
      {{- end -}}
    {{- end -}}
  {{- end -}}
  {{- if not $verified -}}
    {{- fail "Controller settings are operator-managed but their current credential authority is not reconciled; repair that condition before upgrading." -}}
  {{- end -}}
  {{- $configuration := dig "data" "configuration" "" .settings | b64dec | mustFromJson -}}
  {{- if not (kindIs "slice" $configuration) -}}
    {{- fail "Reconciled controller settings must contain an array of environment changes." -}}
  {{- end -}}
  {{- $owned := dict -}}
  {{- $operatorEnv := list -}}
  {{- range $configuration -}}
    {{- $name := required "Reconciled controller setting name is missing." .name -}}
    {{- if hasKey $owned $name -}}{{- fail "Reconciled controller settings contain duplicate names." -}}{{- end -}}
    {{- $_ := set $owned $name true -}}
    {{- if not (.remove | default false) -}}
      {{- if hasKey . "secret" -}}
        {{- $reference := dict "name" (required "Controller credential Secret name is missing." .secret.name)
              "key" (required "Controller credential Secret key is missing." .secret.key) -}}
        {{- $operatorEnv = append $operatorEnv (dict "name" $name "valueFrom" (dict "secretKeyRef" $reference)) -}}
      {{- else -}}
        {{- if or (has $name (list "AZURE_OPENAI_API_KEY" "FOUNDRY_API_KEY" "COPILOT_GITHUB_TOKEN"))
              (not (hasKey . "value")) (not (kindIs "string" .value)) -}}
          {{- fail "Reconciled controller credentials require Secret references, not inline values." -}}
        {{- end -}}
        {{- $operatorEnv = append $operatorEnv (dict "name" $name "value" .value) -}}
      {{- end -}}
    {{- end -}}
  {{- end -}}
  {{- range .deployment.spec.template.spec.containers -}}
    {{- if eq .name "controller" -}}
      {{- $env := list -}}
      {{- range (.env | default list) -}}
        {{- if not (hasKey $owned .name) -}}{{- $env = append $env . -}}{{- end -}}
      {{- end -}}
      {{- $_ := set . "env" (concat $env $operatorEnv) -}}
    {{- end -}}
  {{- end -}}
{{- end -}}
{{/* A fenced publication of a protected root must not restart it solely because
     bootstrap and migration appended identical environment entries differently.
     Retain live ordering only for the same declarations, after operator settings. */}}
{{- if and .precondition (dig "spec" "template" "metadata" "annotations" "kars.azure.com/private-epoch" "" .live)
      (eq .precondition.uid (dig "metadata" "uid" "" .live))
      (eq .precondition.resourceVersion (dig "metadata" "resourceVersion" "" .live)) -}}
  {{- range .deployment.spec.template.spec.containers -}}
    {{- $desired := . -}}
    {{- range (dig "spec" "template" "spec" "containers" list $.live) -}}
      {{- if eq .name $desired.name -}}
        {{- $liveEnv := .env | default list -}}
        {{- $desiredEnv := $desired.env | default list -}}
        {{- $maps := list -}}
        {{- range (list $liveEnv $desiredEnv) -}}
          {{- $entries := dict -}}
          {{- range . -}}
            {{- if hasKey $entries .name -}}{{- fail "Fenced controller environments require unique names" -}}{{- end -}}
            {{- $entry := deepCopy . -}}
            {{- if eq (toJson $entry.value) (toJson "") -}}
              {{- $_ := unset $entry "value" -}}
            {{- end -}}
            {{- if eq (dig "valueFrom" "fieldRef" "apiVersion" "" $entry) "v1" -}}
              {{- $_ := unset $entry.valueFrom.fieldRef "apiVersion" -}}
            {{- end -}}
            {{- $_ := set $entries .name $entry -}}
          {{- end -}}
          {{- $maps = append $maps $entries -}}
        {{- end -}}
        {{- if eq (toJson (index $maps 0)) (toJson (index $maps 1)) -}}
          {{- $ordered := list -}}
          {{- range $liveEnv -}}
            {{- $name := .name -}}
            {{- range $desiredEnv -}}
              {{- if eq .name $name -}}{{- $ordered = append $ordered . -}}{{- end -}}
            {{- end -}}
          {{- end -}}
          {{- $_ := set $desired "env" $ordered -}}
        {{- end -}}
      {{- end -}}
    {{- end -}}
  {{- end -}}
{{- end -}}
{{- toYaml .deployment -}}
{{- end -}}
