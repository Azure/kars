{{- define "kars.credentialIdentitySchema" -}}
type: object
required: [name, uid]
properties:
  name: {type: string, minLength: 1, maxLength: 253}
  uid: {type: string, minLength: 1, maxLength: 128}
{{- end -}}
{{- define "kars.credentialLegacySchema" -}}
type: object
required: [sourceName, namespace, namespaceUid, secret, resourceVersion, keys]
properties:
  sourceName: {type: string, minLength: 1}
  namespace: {type: string, minLength: 1}
  namespaceUid: {type: string, minLength: 1}
  secret:
    {{- include "kars.credentialIdentitySchema" . | nindent 4 }}
  resourceVersion: {type: string, minLength: 1}
  keys:
    type: array
    items: {type: string}
  target:
    {{- include "kars.credentialTargetSchema" . | nindent 4 }}
{{- end -}}
{{- define "kars.credentialTargetSchema" -}}
type: object
required: [kind, name, namespace, uid]
properties:
  kind: {type: string, enum: [KarsSandbox, KarsTask, KarsTeam]}
  namespace: {type: string, minLength: 1, maxLength: 63}
  name: {type: string, minLength: 1, maxLength: 253}
  uid: {type: string, minLength: 1, maxLength: 128}
{{- end -}}
{{- define "kars.credentialBindingsSchema" -}}
description: Explicit governed credential sources and key grants; included in task authority.
type: object
required: [grant, sources]
properties:
  grant:
    {{- include "kars.credentialIdentitySchema" . | nindent 4 }}
  sources:
    type: array
    minItems: 1
    maxItems: 3
    items:
      type: object
      required: [keys, scope, source]
      properties:
        scope: {type: string, enum: [workspace, team, target]}
        source:
          {{- include "kars.credentialIdentitySchema" . | nindent 10 }}
        keys:
          type: array
          maxItems: 128
          items: {type: string, pattern: '^[A-Z_][A-Z0-9_]{0,127}$'}
        owner:
          {{- include "kars.credentialTargetSchema" . | nindent 10 }}
{{- end -}}
{{- define "kars.githubBindingSchema" -}}
type: object
required: [connection, grant, repositories]
properties:
  grant:
    {{- include "kars.credentialIdentitySchema" . | nindent 4 }}
  connection:
    {{- include "kars.credentialIdentitySchema" . | nindent 4 }}
  repositories:
    type: array
    minItems: 1
    maxItems: 32
    x-kubernetes-list-type: set
    items: {type: string, maxLength: 140, pattern: '^[a-z0-9._-]{1,39}/[a-z0-9._-]{1,100}$'}
  write: {type: boolean, default: false}
{{- end -}}
