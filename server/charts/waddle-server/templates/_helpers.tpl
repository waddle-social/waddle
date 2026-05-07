{{/*
Expand the name of the chart.
*/}}
{{- define "waddle-server.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "waddle-server.fullname" -}}
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
Create chart name and version as used by the chart label.
*/}}
{{- define "waddle-server.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Common labels.
*/}}
{{- define "waddle-server.labels" -}}
helm.sh/chart: {{ include "waddle-server.chart" . }}
{{ include "waddle-server.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{/*
Container image reference.
*/}}
{{- define "waddle-server.image" -}}
{{- $repository := required "image.repository is required" .Values.image.repository -}}
{{- $digest := default "" .Values.image.digest -}}
{{- if $digest -}}
{{- printf "%s@%s" $repository $digest -}}
{{- else -}}
{{- printf "%s:%s" $repository (default .Chart.AppVersion .Values.image.tag) -}}
{{- end -}}
{{- end -}}

{{/*
Selector labels.
*/}}
{{- define "waddle-server.selectorLabels" -}}
app.kubernetes.io/name: {{ include "waddle-server.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/*
Service account name.
*/}}
{{- define "waddle-server.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "waddle-server.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{/*
ConfigMap name.
*/}}
{{- define "waddle-server.configMapName" -}}
{{- printf "%s-config" (include "waddle-server.fullname" .) -}}
{{- end -}}

{{/*
Secret name (operator-supplied values).
*/}}
{{- define "waddle-server.secretName" -}}
{{- if .Values.secret.existingSecret -}}
{{- .Values.secret.existingSecret -}}
{{- else -}}
{{- printf "%s-secrets" (include "waddle-server.fullname" .) -}}
{{- end -}}
{{- end -}}

{{/*
Bootstrap Secret name (chart-managed auto-generated keys).

The bootstrap Secret is created out-of-band by a pre-install hook (see
templates/secret-bootstrap-hook.yaml) so that `helm template` does not
produce drift against the live cluster. See waddle-social/waddle#303.
*/}}
{{- define "waddle-server.bootstrapSecretName" -}}
{{- printf "%s-bootstrap-secrets" (include "waddle-server.fullname" .) -}}
{{- end -}}

{{/*
Checksum external envFrom Secret contents so Helm reconciles roll pods after
ExternalSecret-backed key rotation. The Secret values stay hashed; they are not
rendered into the pod template.

This deliberately uses `lookup` to observe live Secret data. Without cluster
access (`helm template`) the checksum collapses to `missing`; the
deployment annotation will then differ from a live render. That is benign
drift -- the annotation only drives pod template hashing, never secret
generation, and matches the documented purpose of `extraSecretChecksum`
(rotate pods when ExternalSecret keys change). For a cluster-access-free
GitOps flow that needs stable annotations, set `extraSecretChecksum` from
`HelmRelease.spec.valuesFrom` referencing the source-of-truth checksum.
*/}}
{{- define "waddle-server.extraSecretChecksum" -}}
{{- range $name := .Values.extraSecretRefs }}
{{- $secret := lookup "v1" "Secret" $.Release.Namespace $name -}}
{{ $name }}={{ if and $secret $secret.data }}{{ toJson $secret.data }}{{ else }}missing{{ end }}
{{ end -}}
{{- end -}}

{{/*
PVC name.
*/}}
{{- define "waddle-server.pvcName" -}}
{{- if .Values.persistence.existingClaim -}}
{{- .Values.persistence.existingClaim -}}
{{- else -}}
{{- printf "%s-data" (include "waddle-server.fullname" .) -}}
{{- end -}}
{{- end -}}
