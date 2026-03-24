package talos

import (
	"fmt"

	"gopkg.in/yaml.v3"
)

// WithMayastorLabConfig applies the Talos machine prerequisites required for
// the single-node lab Mayastor profile.
func WithMayastorLabConfig(machineConfig []byte) ([]byte, error) {
	if len(machineConfig) == 0 {
		return nil, fmt.Errorf("machine config is required")
	}

	var cfg map[string]any
	if err := yaml.Unmarshal(machineConfig, &cfg); err != nil {
		return nil, fmt.Errorf("parse machine config YAML: %w", err)
	}
	if cfg == nil {
		cfg = map[string]any{}
	}

	machine := ensureMapField(cfg, "machine")
	sysctls := ensureMapField(machine, "sysctls")
	sysctls["vm.nr_hugepages"] = "1024"

	nodeLabels := ensureMapField(machine, "nodeLabels")
	nodeLabels["openebs.io/engine"] = "mayastor"

	kubelet := ensureMapField(machine, "kubelet")
	kubelet["extraMounts"] = ensureMayastorExtraMounts(kubelet["extraMounts"])

	cluster := ensureMapField(cfg, "cluster")
	apiServer := ensureMapField(cluster, "apiServer")
	apiServer["admissionControl"] = ensureOpenEBSPodSecurityExemption(apiServer["admissionControl"])

	out, err := yaml.Marshal(cfg)
	if err != nil {
		return nil, fmt.Errorf("encode machine config YAML: %w", err)
	}

	return out, nil
}

func ensureMayastorExtraMounts(existing any) []any {
	mounts := make([]any, 0)
	found := false

	switch typed := existing.(type) {
	case []any:
		for _, item := range typed {
			mount, ok := item.(map[string]any)
			if !ok {
				mounts = append(mounts, item)
				continue
			}
			if mount["destination"] == "/var/local" {
				mount["source"] = "/var/local"
				mount["type"] = "bind"
				mount["options"] = []string{"bind", "rshared", "rw"}
				found = true
			}
			mounts = append(mounts, mount)
		}
	}

	if !found {
		mounts = append(mounts, map[string]any{
			"destination": "/var/local",
			"type":        "bind",
			"source":      "/var/local",
			"options":     []string{"bind", "rshared", "rw"},
		})
	}

	return mounts
}

func ensureOpenEBSPodSecurityExemption(existing any) []any {
	entries := make([]any, 0)
	found := false

	switch typed := existing.(type) {
	case []any:
		for _, item := range typed {
			entry, ok := item.(map[string]any)
			if !ok {
				entries = append(entries, item)
				continue
			}
			if entry["name"] == "PodSecurity" {
				configuration := ensureMapField(entry, "configuration")
				exemptions := ensureMapField(configuration, "exemptions")
				existingNamespaces := stringSlice(exemptions["namespaces"])
				exemptions["namespaces"] = normalizeStringSet(append(existingNamespaces, "openebs")...)
				found = true
			}
			entries = append(entries, entry)
		}
	}

	if !found {
		entries = append(entries, map[string]any{
			"name": "PodSecurity",
			"configuration": map[string]any{
				"apiVersion": "pod-security.admission.config.k8s.io/v1",
				"kind":       "PodSecurityConfiguration",
				"defaults": map[string]any{
					"enforce":         "baseline",
					"enforce-version": "latest",
					"audit":           "restricted",
					"audit-version":   "latest",
					"warn":            "restricted",
					"warn-version":    "latest",
				},
				"exemptions": map[string]any{
					"namespaces": []string{"kube-system", "openebs"},
				},
			},
		})
	}

	return entries
}
