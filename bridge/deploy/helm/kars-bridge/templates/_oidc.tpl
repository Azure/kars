{{/* Copyright (c) Microsoft Corporation. Licensed under the MIT License. */}}
{{- define "kars-bridge.externalOIDCCA.validate" -}}
{{- $oidc := dict -}}
{{- if hasKey .Values.web "oidc" -}}
{{- if not (kindIs "map" .Values.web.oidc) -}}{{- fail "web.oidc must be a map" -}}{{- end -}}
{{- $oidc = .Values.web.oidc -}}
{{- end -}}
{{- $ca := "" -}}
{{- if hasKey $oidc "caConfigMap" -}}
{{- if not (kindIs "string" $oidc.caConfigMap) -}}{{- fail "web.oidc.caConfigMap must be a ConfigMap name string" -}}{{- end -}}
{{- $ca = $oidc.caConfigMap -}}
{{- end -}}
{{- if $ca -}}
{{- if or (gt (len $ca) 253) (not (regexMatch "^[a-z0-9]([-a-z0-9]*[a-z0-9])?(\\.[a-z0-9]([-a-z0-9]*[a-z0-9])?)*$" $ca)) -}}
{{- fail "web.oidc.caConfigMap must be a valid namespace-local ConfigMap name" -}}
{{- end -}}
{{- if .Values.idp.enabled -}}{{- fail "web.oidc.caConfigMap is only supported for external OIDC with idp.enabled=false" -}}{{- end -}}
{{- $issuers := list -}}
{{- range .Values.web.extraEnv -}}
{{- if has .name (list "NODE_EXTRA_CA_CERTS" "NODE_TLS_REJECT_UNAUTHORIZED" "NODE_OPTIONS" "BRIDGE_OIDC_BACKCHANNEL_ISSUER" "DEX_UPSTREAM_URL") -}}
{{- fail "web.oidc.caConfigMap conflicts with web.extraEnv trust or OIDC transport overrides" -}}
{{- end -}}
{{- if eq .name "BRIDGE_OIDC_ISSUER" -}}{{- $issuers = append $issuers . -}}{{- end -}}
{{- end -}}
{{- if ne (len $issuers) 1 -}}{{- fail "web.oidc.caConfigMap requires exactly one explicit HTTPS BRIDGE_OIDC_ISSUER" -}}{{- end -}}
{{- $issuer := first $issuers -}}
{{- if or $issuer.valueFrom (not (kindIs "string" $issuer.value)) -}}{{- fail "web.oidc.caConfigMap requires an explicit HTTPS BRIDGE_OIDC_ISSUER value" -}}{{- end -}}
{{- if not (regexMatch "^https://[A-Za-z0-9][A-Za-z0-9.-]*(:[0-9]+)?(/[^[:space:]?#]*)?$" $issuer.value) -}}
{{- fail "web.oidc.caConfigMap requires an HTTPS issuer without credentials, query or fragment" -}}
{{- end -}}
{{- $port := regexFind ":[0-9]+$" (urlParse $issuer.value).host | trimPrefix ":" -}}
{{- if and $port (or (lt (int $port) 1) (gt (int $port) 65535)) -}}{{- fail "External OIDC HTTPS port must be between 1 and 65535" -}}{{- end -}}
{{- end -}}
{{- end -}}
