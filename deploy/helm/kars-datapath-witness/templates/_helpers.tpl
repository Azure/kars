{{/* Copyright (c) Microsoft Corporation.
Licensed under the MIT License. */}}
{{- define "witness.labels" -}}
app.kubernetes.io/name: kars-datapath-witness
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: Helm
kars.azure.com/witness-addon: "true"
{{- end -}}

{{- define "witness.owner" -}}
meta.helm.sh/release-name: {{ .Release.Name | quote }}
meta.helm.sh/release-namespace: {{ .Release.Namespace | quote }}
{{- end -}}

{{- define "witness.config" -}}
{{ dict "sandboxes" .Values.sandboxes "window_seconds" .Values.aggregator.windowSeconds "interval_seconds" .Values.aggregator.intervalSeconds "dns_image" .Values.gadget.dnsImage "tcp_image" .Values.gadget.tcpImage | toJson }}
{{- end -}}

{{- define "witness.assertOwner" -}}
{{- $obj := .object -}}
{{- if $obj -}}
  {{- $annotations := default dict $obj.metadata.annotations -}}
  {{- $labels := default dict $obj.metadata.labels -}}
  {{- if or (ne (get $annotations "meta.helm.sh/release-name") .root.Release.Name) (ne (get $annotations "meta.helm.sh/release-namespace") .root.Release.Namespace) (ne (get $labels "app.kubernetes.io/managed-by") "Helm") (ne (get $labels "kars.azure.com/witness-addon") "true") -}}
    {{- fail (printf "witness ownership conflict: %s/%s; operator review required; never adopt or delete legacy/shared objects" (default "cluster" $obj.metadata.namespace) $obj.metadata.name) -}}
  {{- end -}}
{{- end -}}
{{- end -}}
