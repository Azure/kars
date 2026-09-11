{{/* Common labels + names for the kars Bridge chart. */}}
{{- define "kars-bridge.labels" -}}
app.kubernetes.io/name: kars-bridge
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: kars
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
{{- end -}}

{{- define "kars-bridge.namespace" -}}
{{- .Values.namespace | default "kars-system" -}}
{{- end -}}

{{- define "kars-bridge.coreNamespace" -}}
{{- $core := .Values.core | default dict -}}
{{- $core.namespace | default "kars-system" -}}
{{- end -}}

{{- define "kars-bridge.serviceAccountName" -}}
{{- .Values.rbac.serviceAccountName | default "kars-bridge" -}}
{{- end -}}
