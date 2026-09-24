{{/* Check live objects, never an operator's claim that a rollout completed. */}}
{{- define "waddle-server.checkCutover" -}}
{{- $allowed := .allowedRevisions -}}
{{- if not $allowed -}}
{{- fail "RollingUpdate cutover guard requires publisher-derived allowedRevisions" -}}
{{- end -}}
{{- range $allowed -}}
{{- if not (regexMatch "^[a-f0-9]{40}$" .) -}}
{{- fail "RollingUpdate cutover guard requires full git SHAs" -}}
{{- end -}}
{{- end -}}
{{- $deployment := .deployment -}}
{{- if not $deployment -}}
{{- fail "RollingUpdate cutover guard cannot find the live Deployment" -}}
{{- end -}}
{{- $replicas := int $deployment.spec.replicas -}}
{{- $status := $deployment.status | default dict -}}
{{- if or (lt $replicas 1) (ne (int ($status.observedGeneration | default 0)) (int $deployment.metadata.generation)) (ne (int ($status.replicas | default 0)) $replicas) (ne (int ($status.updatedReplicas | default 0)) $replicas) (ne (int ($status.readyReplicas | default 0)) $replicas) (ne (int ($status.availableReplicas | default 0)) $replicas) -}}
{{- fail "RollingUpdate cutover guard requires a fully observed, ready Deployment" -}}
{{- end -}}
{{- $count := 0 -}}
{{- range (.pods.items | default list) -}}
{{- $pod := . -}}
{{- $matches := true -}}
{{- range $key, $value := $deployment.spec.selector.matchLabels -}}
{{- if ne (index ($pod.metadata.labels | default dict) $key) $value -}}
{{- $matches = false -}}
{{- end -}}
{{- end -}}
{{- if $matches -}}
{{- $count = add1 $count -}}
{{- $ready := false -}}
{{- range ($pod.status.conditions | default list) -}}
{{- if and (eq .type "Ready") (eq .status "True") -}}{{- $ready = true -}}{{- end -}}
{{- end -}}
{{- if or $pod.metadata.deletionTimestamp (ne $pod.status.phase "Running") (not $ready) -}}
{{- fail "RollingUpdate cutover guard requires every server pod ready and nonterminating" -}}
{{- end -}}
{{- $revision := "" -}}
{{- $image := "" -}}
{{- range $pod.spec.containers -}}
{{- if eq .name "waddle-server" -}}
{{- $image = .image -}}
{{- range (.env | default list) -}}
{{- if eq .name "WADDLE_GIT_SHA" -}}{{- $revision = .value | default "" -}}{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- if or (not (regexMatch "@sha256:[a-f0-9]{64}$" $image)) (not (has $revision $allowed)) -}}
{{- fail (printf "pod %s has not completed the Recreate cutover: require a pinned image with an allowed WADDLE_GIT_SHA" $pod.metadata.name) -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- if ne (int $count) $replicas -}}
{{- fail "RollingUpdate cutover guard requires exactly the desired number of live server pods" -}}
{{- end -}}
{{- end -}}

{{/* Once checked, the controller-owned marker permits normal repair rollouts. */}}
{{- define "waddle-server.guardCutover" -}}
{{- $allowed := required "RollingUpdate cutover guard requires publisher-derived allowedRevisions" .allowedRevisions -}}
{{- $verified := "" -}}
{{- if .deployment -}}
{{- $verified = index (.deployment.metadata.annotations | default dict) "waddle.social/verified-cutover" | default "" -}}
{{- end -}}
{{- if ne $verified (first $allowed) -}}
{{- include "waddle-server.checkCutover" . -}}
{{- end -}}
{{- end -}}
