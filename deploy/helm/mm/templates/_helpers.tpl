{{/* vim: set filetype=mustache: */}}

{{/* Expand the name of the chart. */}}
{{- define "mm.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Create a default fully qualified app name. We truncate at 63 chars
because some Kubernetes name fields are limited to this (by the
DNS naming spec). A fullnameOverride wins when set.
*/}}
{{- define "mm.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/* Chart label value */}}
{{- define "mm.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/* Common labels applied to every resource. */}}
{{- define "mm.labels" -}}
helm.sh/chart: {{ include "mm.chart" . }}
{{ include "mm.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{/* Selector labels — must be stable across chart upgrades. */}}
{{- define "mm.selectorLabels" -}}
app.kubernetes.io/name: {{ include "mm.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/* Resolved secret name — either the one we create or the external one. */}}
{{- define "mm.secretName" -}}
{{- if .Values.secret.create -}}
{{- include "mm.fullname" . }}-secrets
{{- else -}}
{{- .Values.secret.existingSecretName -}}
{{- end -}}
{{- end -}}

{{/* Resolved config map name. */}}
{{- define "mm.configMapName" -}}
{{- if .Values.config.create -}}
{{- include "mm.fullname" . }}-config
{{- else -}}
{{- .Values.config.existingConfigMapName -}}
{{- end -}}
{{- end -}}

{{/* Resolved PVC name (existingClaim wins). */}}
{{- define "mm.pvcName" -}}
{{- if .Values.persistence.existingClaim -}}
{{- .Values.persistence.existingClaim -}}
{{- else -}}
{{- include "mm.fullname" . }}-data
{{- end -}}
{{- end -}}

{{/* Service name, overridable for operators that pin hostnames. */}}
{{- define "mm.serviceName" -}}
{{- if .Values.service.nameOverride -}}
{{- .Values.service.nameOverride -}}
{{- else -}}
{{- include "mm.fullname" . -}}
{{- end -}}
{{- end -}}
