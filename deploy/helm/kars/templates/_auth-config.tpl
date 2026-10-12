{{/* Copyright (c) Microsoft Corporation.
Licensed under the MIT License. */}}

{{- define "kars.authConfig.spec" -}}
{{- $spec := deepCopy (required "entraSidecar.authConfig is required when the sidecar is enabled" .Values.entraSidecar.authConfig) -}}
{{- if ne .Release.Namespace "kars-system" -}}
  {{- fail "entraSidecar requires namespace kars-system (the controller and router use this fixed namespace)" -}}
{{- end -}}
{{- $mode := required "entraSidecar.authConfig.controller.credentialMode is required" $spec.controller.credentialMode -}}
{{- if not (has $mode (list "ManagedIdentityImds" "WorkloadIdentity")) -}}
  {{- fail "entraSidecar credentialMode must be ManagedIdentityImds or WorkloadIdentity" -}}
{{- end -}}
{{- if and (eq $mode "ManagedIdentityImds") (empty $spec.controller.managedIdentityClientId) -}}
  {{- fail "ManagedIdentityImds requires managedIdentityClientId" -}}
{{- end -}}
{{- range $key, $canonical := dict "tenantId" $spec.tenant.tenantId "blueprintClientId" $spec.agentId.blueprintClientId -}}
  {{- $legacy := get $.Values.entraSidecar $key -}}
  {{- if and $legacy (ne (lower $legacy) (lower $canonical)) -}}
    {{- fail (printf "entraSidecar.%s conflicts with authConfig; remove the deprecated duplicate value" $key) -}}
  {{- end -}}
{{- end -}}
{{- if not (hasKey $spec.tenant "authorityHost") -}}
  {{- $_ := set $spec.tenant "authorityHost" "https://login.microsoftonline.com/" -}}
{{- end -}}
{{- if not (hasKey $spec "meshAuthBackend") -}}{{- $_ := set $spec "meshAuthBackend" "Anonymous" -}}{{- end -}}
{{- if not (hasKey $spec "downstreamApis") -}}{{- $_ := set $spec "downstreamApis" dict -}}{{- end -}}
{{- $seen := dict -}}
{{- range $name, $api := $spec.downstreamApis -}}
  {{- $key := lower $name -}}
  {{- if hasKey $seen $key -}}{{- fail "downstreamApis names must be unique ignoring case" -}}{{- end -}}
  {{- $_ := set $seen $key true -}}
  {{- if not (hasKey $api "requestAppToken") -}}{{- $_ := set $api "requestAppToken" true -}}{{- end -}}
{{- end -}}
{{- mustToJson $spec -}}
{{- end -}}

{{- define "kars.authConfig.env" -}}
{{- $s := include "kars.authConfig.spec" . | fromJson -}}
{{- $env := dict "AzureAd__TenantId" $s.tenant.tenantId "AzureAd__Instance" $s.tenant.authorityHost "AzureAd__ClientId" $s.agentId.blueprintClientId -}}
{{- $prefix := "AzureAd__ClientCredentials__0__" -}}
{{- if eq $s.controller.credentialMode "ManagedIdentityImds" -}}
  {{- $_ := set $env (printf "%sSourceType" $prefix) "SignedAssertionFromManagedIdentity" -}}
  {{- $_ := set $env (printf "%sManagedIdentityClientId" $prefix) $s.controller.managedIdentityClientId -}}
{{- else -}}
  {{- $_ := set $env (printf "%sSourceType" $prefix) "SignedAssertionFilePath" -}}
  {{- $_ := set $env (printf "%sSignedAssertionFileDiskPath" $prefix) "/var/run/secrets/azure/tokens/azure-identity-token" -}}
{{- end -}}
{{- $explicitMesh := false -}}
{{- range $name, $api := $s.downstreamApis -}}
  {{- if eq (lower $name) "agentmesh" -}}{{- $explicitMesh = true -}}{{- end -}}
  {{- $_ := set $env (printf "DownstreamApis__%s__BaseUrl" $name) $api.baseUrl -}}
  {{- $_ := set $env (printf "DownstreamApis__%s__RequestAppToken" $name) (toString $api.requestAppToken) -}}
  {{- range $i, $scope := $api.scopes -}}
    {{- $_ := set $env (printf "DownstreamApis__%s__Scopes__%d" $name $i) $scope -}}
  {{- end -}}
{{- end -}}
{{- if and (eq $s.meshAuthBackend "EntraAgentIdentity") (not $explicitMesh) -}}
  {{- $_ := set $env "DownstreamApis__AgentMesh__BaseUrl" "https://agentmesh.invalid/" -}}
  {{- $_ := set $env "DownstreamApis__AgentMesh__RequestAppToken" "true" -}}
  {{- $_ := set $env "DownstreamApis__AgentMesh__Scopes__0" (default "api://agentmesh/.default" (trim (default "" $s.meshAuthAudience))) -}}
{{- end -}}
{{- mustToJson $env -}}
{{- end -}}

{{- define "kars.authConfig.configMapName" -}}
{{- printf "kars-auth-env-%s" (include "kars.authConfig.env" . | sha256sum | trunc 48) -}}
{{- end -}}

{{- define "kars.authConfig.crd" -}}
{{- $crd := .Files.Get "files/auth-config-schema-baseline.yaml" | fromYaml -}}
{{- if empty $crd -}}{{- fail "AuthConfig schema baseline is required" -}}{{- end -}}
{{- range $version := $crd.spec.versions -}}
  {{- if eq $version.name "v1alpha1" -}}
    {{- $controller := $version.schema.openAPIV3Schema.properties.spec.properties.controller -}}
    {{- $_ := unset $controller "required" -}}
    {{- $_ := set $controller.properties "credentialMode" (dict "type" "string" "default" "ManagedIdentityImds" "enum" (list "ManagedIdentityImds" "WorkloadIdentity")) -}}
    {{- range $name := list "managedIdentityClientId" "managedIdentityResourceId" "managedIdentityPrincipalId" -}}
      {{- $_ := set (get $controller.properties $name) "nullable" true -}}
    {{- end -}}
  {{- end -}}
{{- end -}}
{{- mustToJson $crd -}}
{{- end -}}

{{- define "kars.authConfig.bootstrap" -}}
{{- $hook := .Values.schemaHook | default dict -}}
{{- if $hook.enabled -}}
  {{- fail "entraSidecar cannot run with the cold schemaHook: install Core with the sidecar disabled, then explicitly upgrade the same release with schemaHook disabled and reviewed authConfig values" -}}
{{- end -}}
{{- if not (.Capabilities.APIVersions.Has "kars.azure.com/v1alpha1/KarsAuthConfig") -}}
  {{- fail "entraSidecar requires an established supported KarsAuthConfig CRD: bootstrap Core with the sidecar disabled before explicitly upgrading the same release" -}}
{{- end -}}
{{- with . -}}
  {{- $crd := lookup "apiextensions.k8s.io/v1" "CustomResourceDefinition" "" "karsauthconfigs.kars.azure.com" -}}
  {{- if not $crd -}}{{- fail "KarsAuthConfig CRD lookup is empty; API discovery alone cannot establish the supported schema (offline enabled renders need an explicit lookup fixture)" -}}{{- end -}}
  {{- if $crd -}}
    {{- $compatible := false -}}
    {{- $expected := include "kars.authConfig.crd" . | fromJson -}}
    {{- $schema := (first $expected.spec.versions).schema | mustToJson -}}
    {{- if kindIs "map" $crd.spec -}}
      {{- if and (eq (mustToJson $crd.spec.group) (mustToJson $expected.spec.group)) (eq (mustToJson $crd.spec.scope) (mustToJson $expected.spec.scope)) (kindIs "map" $crd.spec.names) (kindIs "slice" $crd.spec.versions) -}}
        {{- if and (eq (mustToJson $crd.spec.names.kind) (mustToJson $expected.spec.names.kind)) (eq (mustToJson $crd.spec.names.plural) (mustToJson $expected.spec.names.plural)) -}}
          {{- range $v := $crd.spec.versions -}}
            {{- if kindIs "map" $v -}}
              {{- if and (eq (mustToJson $v.name) "\"v1alpha1\"") (eq (mustToJson $v.served) "true") (eq (mustToJson $v.storage) "true") (eq (mustToJson $v.schema) $schema) -}}
                {{- $compatible = true -}}
              {{- end -}}
            {{- end -}}
          {{- end -}}
        {{- end -}}
      {{- end -}}
    {{- end -}}
    {{- if not $compatible -}}{{- fail "live KarsAuthConfig CRD does not match the supported credential-mode schema; a reviewed schema transition is required before enabling the sidecar" -}}{{- end -}}
    {{- $established := false -}}
    {{- $terminating := false -}}
    {{- $status := $crd.status | default dict -}}
    {{- range $condition := $status.conditions | default list -}}
      {{- if eq $condition.status "True" -}}
        {{- if eq $condition.type "Established" -}}{{- $established = true -}}{{- end -}}
        {{- if eq $condition.type "Terminating" -}}{{- $terminating = true -}}{{- end -}}
      {{- end -}}
    {{- end -}}
    {{- if or (not $established) $terminating $crd.metadata.deletionTimestamp -}}
      {{- fail "live KarsAuthConfig CRD must be Established=True and not terminating; complete the reviewed schema lifecycle before enabling the sidecar" -}}
    {{- end -}}
  {{- end -}}
{{- end -}}
{{- $live := lookup "kars.azure.com/v1alpha1" "KarsAuthConfig" "" "default" -}}
{{- if $live -}}
  {{- if $live.metadata.deletionTimestamp -}}
    {{- fail "KarsAuthConfig/default is terminating; complete explicit identity retirement/recovery before upgrading" -}}
  {{- end -}}
  {{- $annotations := $live.metadata.annotations | default dict -}}
  {{- $labels := $live.metadata.labels | default dict -}}
  {{- if or (ne (get $annotations "meta.helm.sh/release-name") .Release.Name) (ne (get $annotations "meta.helm.sh/release-namespace") .Release.Namespace) (ne (get $labels "app.kubernetes.io/managed-by") "Helm") -}}
    {{- fail "KarsAuthConfig/default is not owned by this Helm release; explicit ownership/configuration migration is required" -}}
  {{- end -}}
  {{- $hash := get $annotations "kars.azure.com/helm-auth-spec-sha256" -}}
  {{- if empty $hash -}}{{- fail "KarsAuthConfig/default lacks a reviewed Helm spec hash; explicit configuration migration is required" -}}{{- end -}}
  {{- if ne $hash (mustToJson $live.spec | sha256sum) -}}
    {{- fail "KarsAuthConfig/default changed outside Helm; reconcile the reviewed values before upgrading" -}}
  {{- end -}}
{{- end -}}
{{- end -}}
