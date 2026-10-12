{{/* Copyright (c) Microsoft Corporation.
Licensed under the MIT License. */}}
{{/*
The credential authority, not bootstrap values, selects console-managed settings.
Render its current UID-bound configuration for both Helm 3 merge and Helm 4 SSA.
Credentials remain Secret references, never copied values in release storage.
*/}}
{{- define "kars.controller.operatorSettings" -}}
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
{{- toYaml .deployment -}}
{{- end -}}
