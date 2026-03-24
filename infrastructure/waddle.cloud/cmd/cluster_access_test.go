package cmd

import (
	"strings"
	"testing"

	clusterstate "github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/cluster"
)

func TestTalosAccessEndpointsPrefersNodeFQDN(t *testing.T) {
	t.Setenv("TALOS_NODE_FQDN_SUFFIX", "rka.internal")

	node := &clusterstate.NodeState{
		Name:      "production-control-plane-01",
		PublicIP:  "62.210.216.62",
		PrivateIP: "172.16.16.16",
	}

	got := talosAccessEndpoints(node)
	want := []string{
		"production-control-plane-01.rka.internal",
		"production-control-plane-01",
		"62.210.216.62",
		"172.16.16.16",
	}

	if len(got) != len(want) {
		t.Fatalf("talosAccessEndpoints() length = %d, want %d (got %v)", len(got), len(want), got)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("talosAccessEndpoints()[%d] = %q, want %q (got %v)", i, got[i], want[i], got)
		}
	}
}

func TestTalosAccessEndpointsFQDNNodeName(t *testing.T) {
	node := &clusterstate.NodeState{
		Name:      "production-control-plane-01.rka.internal",
		PublicIP:  "62.210.216.62",
		PrivateIP: "172.16.16.16",
	}

	got := talosAccessEndpoints(node)
	want := []string{
		"production-control-plane-01.rka.internal",
		"62.210.216.62",
		"172.16.16.16",
	}

	if len(got) != len(want) {
		t.Fatalf("talosAccessEndpoints() length = %d, want %d (got %v)", len(got), len(want), got)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("talosAccessEndpoints()[%d] = %q, want %q (got %v)", i, got[i], want[i], got)
		}
	}
}

func TestRewriteKubeconfigServerIfNeededUsesCanonicalAPIEndpoint(t *testing.T) {
	input := []byte(`
apiVersion: v1
clusters:
  - name: production
    cluster:
      server: https://172.16.16.16:6443
contexts: []
current-context: ""
kind: Config
users: []
`)

	out, err := rewriteKubeconfigServerIfNeeded(input, "production-control-plane-01.infra.waddle.social")
	if err != nil {
		t.Fatalf("rewriteKubeconfigServerIfNeeded returned error: %v", err)
	}

	encoded := string(out)
	if !strings.Contains(encoded, "server: https://production-control-plane-01.infra.waddle.social:6443") {
		t.Fatalf("expected rewritten kubeconfig server, got:\n%s", encoded)
	}
	if strings.Contains(encoded, "server: https://172.16.16.16:6443") {
		t.Fatalf("expected private kubeconfig server to be replaced, got:\n%s", encoded)
	}
}
