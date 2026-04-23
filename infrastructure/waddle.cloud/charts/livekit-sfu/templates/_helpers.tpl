{{/*
Expand the name of the chart.
*/}}
{{- define "livekit-sfu.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "livekit-sfu.fullname" -}}
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

{{/*
Common labels.
*/}}
{{- define "livekit-sfu.labels" -}}
helm.sh/chart: {{ include "livekit-sfu.chart" . }}
{{ include "livekit-sfu.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{/*
Selector labels.
*/}}
{{- define "livekit-sfu.selectorLabels" -}}
app.kubernetes.io/name: {{ include "livekit-sfu.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/*
Chart label.
*/}}
{{- define "livekit-sfu.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Service account name.
*/}}
{{- define "livekit-sfu.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "livekit-sfu.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{/*
API keys secret name.
*/}}
{{- define "livekit-sfu.apiKeysSecretName" -}}
{{- if .Values.apiKeys.existingSecret -}}
{{- .Values.apiKeys.existingSecret -}}
{{- else -}}
{{- printf "%s-api-keys" (include "livekit-sfu.fullname" .) -}}
{{- end -}}
{{- end -}}
