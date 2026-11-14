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
Headless Service name for clustering swarm DNS bootstrap.
*/}}
{{- define "waddle-server.headlessServiceName" -}}
{{- printf "%s-headless" (include "waddle-server.fullname" .) -}}
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
Runtime Secret name for required server keys.
*/}}
{{- define "waddle-server.runtimeSecretName" -}}
{{- if .Values.secret.runtimeSecretName -}}
{{- .Values.secret.runtimeSecretName -}}
{{- else if .Values.secret.existingSecret -}}
{{- .Values.secret.existingSecret -}}
{{- else if and .Values.secret.create (or .Values.secret.sessionKey .Values.secret.occupantIdSecret) -}}
{{- include "waddle-server.secretName" . -}}
{{- end -}}
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

{{/*
ConfigMap the owner Job publishes: the handoff as `emit-handoff` wrote it.
*/}}
{{- define "waddle-server.handoffPinsConfigMapName" -}}
{{- printf "%s-runtime-handoff-pins" (include "waddle-server.fullname" .) -}}
{{- end -}}

{{/*
ConfigMap the server reads its handoff from: the published pins with this
release's Git SHA.
*/}}
{{- define "waddle-server.handoffConfigMapName" -}}
{{- printf "%s-runtime-handoff" (include "waddle-server.fullname" .) -}}
{{- end -}}

{{- define "waddle-server.ownerJobName" -}}
{{- printf "%s-owner" (include "waddle-server.fullname" .) -}}
{{- end -}}

{{/*
The handoff for this release. Every field except `release_git_sha` is the
same for every release of one prepared database, so the document the owner
Job emitted is reused and only that field is replaced.

The document comes from values when set there, otherwise from the ConfigMap
the owner Job published. The second source uses `lookup`, which returns
nothing without cluster access (`helm template`), so an offline render needs
the document in values.
*/}}
{{- define "waddle-server.restrictedHandoff" -}}
{{- $source := include "waddle-server.restrictedHandoffSource" . -}}
{{- if not $source -}}
{{- fail (printf "restrictedRuntime.enabled needs the handoff: set restrictedRuntime.handoff.document, or run the owner Job so that ConfigMap %s exists" (include "waddle-server.handoffPinsConfigMapName" .)) -}}
{{- end -}}
{{- $document := fromJson $source -}}
{{- $_ := set $document "release_git_sha" .Values.restrictedRuntime.releaseGitSha -}}
{{- toJson $document -}}
{{- end -}}

{{/*
The handoff document as its source holds it, as JSON, or an empty string
when neither values nor the published ConfigMap holds one.
*/}}
{{- define "waddle-server.restrictedHandoffSource" -}}
{{- if .Values.restrictedRuntime.handoff.document -}}
{{- toJson .Values.restrictedRuntime.handoff.document -}}
{{- else -}}
{{- $published := lookup "v1" "ConfigMap" .Release.Namespace (include "waddle-server.handoffPinsConfigMapName" .) -}}
{{- if and $published $published.data (hasKey $published.data "handoff.json") -}}
{{- toJson (fromJson (index $published.data "handoff.json")) -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/*
"true" when the restricted server has to wait for the owner Job of a fresh
install: `handoff.awaitOwnerJob` is set and no handoff exists yet. The
release then renders the Deployment at zero replicas and no handoff
ConfigMap, so that the same release renders the owner CronJob that
produces the handoff. A Deployment that is serving is never scaled down for
this: without a handoff its render fails, as it does when the setting is
off.
*/}}
{{- define "waddle-server.restrictedHandoffPending" -}}
{{- $restricted := .Values.restrictedRuntime -}}
{{- if and $restricted.enabled $restricted.handoff.awaitOwnerJob (not (include "waddle-server.restrictedHandoffSource" .)) -}}
{{- $live := lookup "apps/v1" "Deployment" .Release.Namespace (include "waddle-server.fullname" .) -}}
{{- if and $live $live.spec (gt (int ($live.spec.replicas | default 0)) 0) -}}
{{- fail (printf "restrictedRuntime.enabled needs the handoff, and Deployment %s is serving: run the owner Job so that ConfigMap %s exists" (include "waddle-server.fullname" .) (include "waddle-server.handoffPinsConfigMapName" .)) -}}
{{- end -}}
true
{{- end -}}
{{- end -}}

{{/*
A PostgreSQL URL for one login over verified TLS. The password is expanded
by the kubelet from the environment variable named in `passwordVariable`,
which must be defined earlier in the same container.
*/}}
{{- define "waddle-server.restrictedDatabaseUrl" -}}
{{- $database := .root.Values.restrictedRuntime.database -}}
{{- printf "postgresql://%s:$(%s)@%s:%d/%s?sslmode=verify-full&sslrootcert=/etc/waddle/postgres-ca/%s" .login .passwordVariable $database.host (int $database.port) $database.name $database.caSecret.key -}}
{{- end -}}

{{/*
The runtime login's URL: the application schema is its search path.
*/}}
{{- define "waddle-server.restrictedRuntimeDatabaseUrl" -}}
{{- include "waddle-server.restrictedDatabaseUrl" (dict "root" . "login" .Values.restrictedRuntime.roles.runtimeLogin "passwordVariable" "WADDLE_RUNTIME_DB_PASSWORD") -}}
{{- printf "&options=-c%%20search_path%%3D%s" .Values.restrictedRuntime.schema -}}
{{- end -}}

{{/*
Role and schema names shared by the owner subcommands and the restricted
server.
*/}}
{{- define "waddle-server.restrictedRoleEnv" -}}
- name: WADDLE_DB_MIGRATOR_LOGIN
  value: {{ .Values.restrictedRuntime.roles.migratorLogin | quote }}
- name: WADDLE_XMPP_MAM_SCHEMA_OWNER
  value: {{ .Values.restrictedRuntime.roles.schemaOwner | quote }}
- name: WADDLE_XMPP_MAM_CLAIM_SCHEMA
  value: {{ .Values.restrictedRuntime.schema | quote }}
{{- end -}}

{{/*
The owner phase reads the server's own configuration (deployment identity,
lineage action, clustering switch, keypair pool, lease lifetime), so it
takes the sources the server takes them from.
*/}}
{{- define "waddle-server.ownerJobEnvFrom" -}}
- configMapRef:
    name: {{ include "waddle-server.configMapName" . }}
{{- if and .Values.secret.create (not .Values.secret.existingSecret) }}
- secretRef:
    name: {{ include "waddle-server.secretName" . }}
    optional: true
{{- else if .Values.secret.existingSecret }}
- secretRef:
    name: {{ include "waddle-server.secretName" . }}
    optional: false
{{- end }}
{{- range .Values.extraSecretRefs }}
- secretRef:
    name: {{ . }}
{{- end }}
{{- end -}}

{{- define "waddle-server.ownerJobRoleEnv" -}}
{{ include "waddle-server.restrictedRoleEnv" . }}
- name: WADDLE_DB_RUNTIME_LOGIN
  value: {{ .Values.restrictedRuntime.roles.runtimeLogin | quote }}
- name: WADDLE_DB_ATTESTOR_DEFINER_ROLE
  value: {{ .Values.restrictedRuntime.roles.attestorDefinerRole | quote }}
- name: WADDLE_DB_ATTESTOR_INSTALLER_LOGIN
  value: {{ .Values.restrictedRuntime.roles.attestorInstallerLogin | quote }}
- name: WADDLE_DB_ATTESTOR_SCHEMA
  value: {{ .Values.restrictedRuntime.roles.attestorSchema | quote }}
{{- end -}}

{{- define "waddle-server.ownerJobVolumeMounts" -}}
- name: work
  mountPath: /work
- name: inputs
  mountPath: /inputs
  readOnly: true
- name: postgres-ca
  mountPath: /etc/waddle/postgres-ca
  readOnly: true
{{- end -}}
