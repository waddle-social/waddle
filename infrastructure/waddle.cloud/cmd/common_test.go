package cmd

import (
	"strings"
	"testing"

	clusterstate "github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/cluster"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
)

func TestSecretPathForCluster(t *testing.T) {
	cfg := &config.Config{
		Environment: "production",
		Secrets: config.SecretsConfig{
			SecretPath: "/projects/waddle-cloud",
		},
	}

	got := secretPathForCluster(cfg)
	want := "/projects/waddle-cloud/production"
	if got != want {
		t.Fatalf("secretPathForCluster() = %q, want %q", got, want)
	}
}

func TestNetbirdSetupKeyLookupTargetsDefaults(t *testing.T) {
	cfg := &config.Config{
		Environment: "production",
		Secrets: config.SecretsConfig{
			SecretPath: "/projects/waddle-cloud",
		},
	}

	path, candidates := netbirdSetupKeyLookupTargets(cfg, "", "")
	if path != "/projects/waddle-cloud" {
		t.Fatalf("netbirdSetupKeyLookupTargets() path = %q, want %q", path, "/projects/waddle-cloud")
	}
	if len(candidates) != 2 {
		t.Fatalf("netbirdSetupKeyLookupTargets() candidates length = %d, want 2", len(candidates))
	}
	if candidates[0] != netbirdSetupKeyPrimary || candidates[1] != netbirdSetupKeyCompatibility {
		t.Fatalf("netbirdSetupKeyLookupTargets() candidates = %v, want [%q %q]", candidates, netbirdSetupKeyPrimary, netbirdSetupKeyCompatibility)
	}
}

func TestNetbirdSetupKeyLookupTargetsOverrides(t *testing.T) {
	cfg := &config.Config{
		Environment: "production",
		Secrets: config.SecretsConfig{
			SecretPath: "/projects/waddle-cloud",
		},
	}

	path, candidates := netbirdSetupKeyLookupTargets(cfg, " /apps/shared/netbird ", " CUSTOM_NETBIRD_KEY ")
	if path != "/apps/shared/netbird" {
		t.Fatalf("netbirdSetupKeyLookupTargets() path = %q, want %q", path, "/apps/shared/netbird")
	}
	if len(candidates) != 1 || candidates[0] != "CUSTOM_NETBIRD_KEY" {
		t.Fatalf("netbirdSetupKeyLookupTargets() candidates = %v, want [%q]", candidates, "CUSTOM_NETBIRD_KEY")
	}
}

func TestNetbirdSetupKeyLookupTargetsConfigDefaults(t *testing.T) {
	cfg := &config.Config{
		Environment: "production",
		Secrets: config.SecretsConfig{
			SecretPath:       "/projects/waddle-cloud",
			NetbirdSecretKey: "NETBIRD_SETUP_KEY",
		},
	}

	path, candidates := netbirdSetupKeyLookupTargets(cfg, "", "")
	if path != "/projects/waddle-cloud" {
		t.Fatalf("netbirdSetupKeyLookupTargets() path = %q, want %q", path, "/projects/waddle-cloud")
	}
	if len(candidates) != 1 || candidates[0] != "NETBIRD_SETUP_KEY" {
		t.Fatalf("netbirdSetupKeyLookupTargets() candidates = %v, want [%q]", candidates, "NETBIRD_SETUP_KEY")
	}
}

func TestControlPlaneReservedIPForSlot(t *testing.T) {
	pool := &config.NodePoolConfig{
		Name: "main",
		ReservedPrivateIPs: []string{
			"172.16.16.16",
			"172.16.16.17",
		},
	}

	got, err := controlPlaneReservedIPForSlot(pool, 2)
	if err != nil {
		t.Fatalf("controlPlaneReservedIPForSlot returned error: %v", err)
	}
	if got != "172.16.16.17" {
		t.Fatalf("controlPlaneReservedIPForSlot() = %q, want %q", got, "172.16.16.17")
	}
}

func TestControlPlaneNodeName(t *testing.T) {
	got := controlPlaneNodeName("production", "control-plane", 1)
	want := "production-control-plane-01"
	if got != want {
		t.Fatalf("controlPlaneNodeName() = %q, want %q", got, want)
	}
}

func TestCanonicalKubernetesAPIEndpoint(t *testing.T) {
	cfg := &config.Config{
		Environment: "production",
		NodePools: []config.NodePoolConfig{
			{Name: "control-plane", Type: config.NodeTypeControlPlane},
		},
	}

	got, err := canonicalKubernetesAPIEndpoint(cfg)
	if err != nil {
		t.Fatalf("canonicalKubernetesAPIEndpoint returned error: %v", err)
	}
	if got != "production-control-plane-01.infra.waddle.social" {
		t.Fatalf("canonicalKubernetesAPIEndpoint() = %q, want %q", got, "production-control-plane-01.infra.waddle.social")
	}
}

func TestCanonicalKubernetesAPIEndpointHonorsSuffixOverride(t *testing.T) {
	t.Setenv("TALOS_NODE_FQDN_SUFFIX", "rka.internal")

	cfg := &config.Config{
		Environment: "production",
		NodePools: []config.NodePoolConfig{
			{Name: "control-plane", Type: config.NodeTypeControlPlane},
		},
	}

	got, err := canonicalKubernetesAPIEndpoint(cfg)
	if err != nil {
		t.Fatalf("canonicalKubernetesAPIEndpoint returned error: %v", err)
	}
	if got != "production-control-plane-01.rka.internal" {
		t.Fatalf("canonicalKubernetesAPIEndpoint() = %q, want %q", got, "production-control-plane-01.rka.internal")
	}
}

func TestPooledNodeName(t *testing.T) {
	got := pooledNodeName("production", "worker", 4)
	want := "production-worker-04"
	if got != want {
		t.Fatalf("pooledNodeName() = %q, want %q", got, want)
	}
}

func TestParseControlPlaneSlot(t *testing.T) {
	slot, ok := parseControlPlaneSlot("production", "control-plane", "production-control-plane-03")
	if !ok {
		t.Fatalf("parseControlPlaneSlot() expected success for new format")
	}
	if slot != 3 {
		t.Fatalf("parseControlPlaneSlot() = %d, want %d", slot, 3)
	}

	legacySlot, legacyOK := parseControlPlaneSlot("production", "control-plane", "control-plane-02")
	if !legacyOK {
		t.Fatalf("parseControlPlaneSlot() expected success for legacy format")
	}
	if legacySlot != 2 {
		t.Fatalf("parseControlPlaneSlot() legacy = %d, want %d", legacySlot, 2)
	}
}

func TestParsePooledNodeSlotWorker(t *testing.T) {
	slot, ok := parsePooledNodeSlot("production", "worker", "production-worker-03")
	if !ok {
		t.Fatalf("parsePooledNodeSlot() expected success for worker format")
	}
	if slot != 3 {
		t.Fatalf("parsePooledNodeSlot() = %d, want %d", slot, 3)
	}

	legacySlot, legacyOK := parsePooledNodeSlot("production", "worker", "worker-02")
	if !legacyOK {
		t.Fatalf("parsePooledNodeSlot() expected success for legacy worker format")
	}
	if legacySlot != 2 {
		t.Fatalf("parsePooledNodeSlot() legacy = %d, want %d", legacySlot, 2)
	}
}

func TestNextControlPlaneSlot(t *testing.T) {
	state := &clusterstate.NodesState{
		Nodes: []clusterstate.NodeState{
			{Name: "production-main-01", Role: config.NodeTypeControlPlane, Pool: "main", Status: clusterstate.NodeStatusReady},
			{Name: "production-main-02", Role: config.NodeTypeControlPlane, Pool: "main", Status: clusterstate.NodeStatusDeleted},
			{Name: "workers-01", Role: config.NodeTypeWorker, Pool: "workers", Status: clusterstate.NodeStatusReady},
		},
	}

	if got := nextControlPlaneSlot(state, "production", "main"); got != 2 {
		t.Fatalf("nextControlPlaneSlot() = %d, want %d", got, 2)
	}
}

func TestNextControlPlaneSlotReservesLegacyUnnamedSlots(t *testing.T) {
	state := &clusterstate.NodesState{
		Nodes: []clusterstate.NodeState{
			{Name: "legacy-cp", Role: config.NodeTypeControlPlane, Pool: "main", Status: clusterstate.NodeStatusReady},
			{Name: "production-main-01", Role: config.NodeTypeControlPlane, Pool: "main", Status: clusterstate.NodeStatusReady},
		},
	}

	if got := nextControlPlaneSlot(state, "production", "main"); got != 3 {
		t.Fatalf("nextControlPlaneSlot() = %d, want %d", got, 3)
	}
}

func TestNextNodePoolSlotWorker(t *testing.T) {
	state := &clusterstate.NodesState{
		Nodes: []clusterstate.NodeState{
			{Name: "production-worker-01", Role: config.NodeTypeWorker, Pool: "worker", Status: clusterstate.NodeStatusReady},
			{Name: "legacy-worker-name", Role: config.NodeTypeWorker, Pool: "worker", Status: clusterstate.NodeStatusReady},
			{Name: "production-worker-03", Role: config.NodeTypeWorker, Pool: "worker", Status: clusterstate.NodeStatusDeleted},
			{Name: "production-control-plane-01", Role: config.NodeTypeControlPlane, Pool: "control-plane", Status: clusterstate.NodeStatusReady},
		},
	}

	if got := nextNodePoolSlot(state, "production", "worker", config.NodeTypeWorker); got != 3 {
		t.Fatalf("nextNodePoolSlot() = %d, want %d", got, 3)
	}
}

func TestControlPlaneEndpointFromStatePrefersPrivateIP(t *testing.T) {
	state := &clusterstate.NodesState{
		Nodes: []clusterstate.NodeState{
			{
				Name:      "production-main-01",
				Role:      config.NodeTypeControlPlane,
				Pool:      "main",
				Status:    clusterstate.NodeStatusReady,
				PublicIP:  "203.0.113.10",
				PrivateIP: "172.16.16.16",
			},
		},
	}

	endpoint, err := controlPlaneEndpointFromState(state)
	if err != nil {
		t.Fatalf("controlPlaneEndpointFromState returned error: %v", err)
	}
	if endpoint != "172.16.16.16" {
		t.Fatalf("controlPlaneEndpointFromState() = %q, want %q", endpoint, "172.16.16.16")
	}
}

func TestAppendTalosConfigDocuments(t *testing.T) {
	base := []byte("version: v1alpha1\nmachine: {}\n")
	doc := []byte("apiVersion: v1alpha1\nkind: ExtensionServiceConfig\nname: netbird\n")

	out, err := appendTalosConfigDocuments(base, doc)
	if err != nil {
		t.Fatalf("appendTalosConfigDocuments returned error: %v", err)
	}

	encoded := string(out)
	if !strings.Contains(encoded, "version: v1alpha1") {
		t.Fatalf("expected base config in output, got:\n%s", encoded)
	}
	if !strings.Contains(encoded, "---\napiVersion: v1alpha1\nkind: ExtensionServiceConfig\nname: netbird") {
		t.Fatalf("expected appended document separator and document, got:\n%s", encoded)
	}
}

func TestTalosAPIAllowedSubnetsDefault(t *testing.T) {
	t.Setenv(envTalosAllowedSubnets, "")

	got := talosAPIAllowedSubnets()
	if len(got) != 1 || got[0] != defaultTalosAPINetbirdSubnet {
		t.Fatalf("talosAPIAllowedSubnets() = %v, want [%q]", got, defaultTalosAPINetbirdSubnet)
	}
}

func TestTalosAPIAllowedSubnetsEnv(t *testing.T) {
	t.Setenv(envTalosAllowedSubnets, "100.64.0.0/10, fd00::/8, 100.64.0.0/10")

	got := talosAPIAllowedSubnets()
	if len(got) != 2 {
		t.Fatalf("talosAPIAllowedSubnets() length = %d, want 2 (got %v)", len(got), got)
	}
	if got[0] != "100.64.0.0/10" || got[1] != "fd00::/8" {
		t.Fatalf("talosAPIAllowedSubnets() = %v, want [100.64.0.0/10 fd00::/8]", got)
	}
}

func TestValidateReinstallFlags(t *testing.T) {
	tests := []struct {
		name             string
		serverID         string
		confirmReinstall bool
		wantErrContains  string
	}{
		{
			name:             "no reinstall flags",
			serverID:         "",
			confirmReinstall: false,
		},
		{
			name:             "server id with confirmation",
			serverID:         "srv-123",
			confirmReinstall: true,
		},
		{
			name:             "server id without confirmation",
			serverID:         "srv-123",
			confirmReinstall: false,
			wantErrContains:  "--server-id requires --confirm-reinstall",
		},
		{
			name:             "confirmation without server id",
			serverID:         "",
			confirmReinstall: true,
			wantErrContains:  "--confirm-reinstall requires --server-id",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := validateReinstallFlags(tt.serverID, tt.confirmReinstall)
			if tt.wantErrContains == "" {
				if err != nil {
					t.Fatalf("validateReinstallFlags() returned unexpected error: %v", err)
				}
				return
			}
			if err == nil {
				t.Fatalf("validateReinstallFlags() expected error containing %q, got nil", tt.wantErrContains)
			}
			if !strings.Contains(err.Error(), tt.wantErrContains) {
				t.Fatalf("validateReinstallFlags() error = %q, want contains %q", err, tt.wantErrContains)
			}
		})
	}
}

func TestServerBelongsToPoolManagedTags(t *testing.T) {
	pool := &config.NodePoolConfig{
		Name: "workers",
		Type: config.NodeTypeWorker,
	}

	managedTags := managedServerTags("production", "workers", config.NodeTypeWorker)
	if !serverBelongsToPool("production", pool, "non-matching-node-name", managedTags) {
		t.Fatal("expected serverBelongsToPool to match managed tags even with non-matching name")
	}
}

func TestServerBelongsToPoolFallsBackToNameMatching(t *testing.T) {
	pool := &config.NodePoolConfig{
		Name: "workers",
		Type: config.NodeTypeWorker,
	}

	if !serverBelongsToPool("production", pool, "production-workers-01", nil) {
		t.Fatal("expected serverBelongsToPool to match node name when managed tags are absent")
	}
}

func TestNodeRoleForServerPrefersManagedRoleTag(t *testing.T) {
	pool := &config.NodePoolConfig{
		Name: "workers",
		Type: config.NodeTypeControlPlane,
	}

	tags := managedServerTags("production", "workers", config.NodeTypeWorker)
	got := nodeRoleForServer(pool, tags)
	if got != config.NodeTypeWorker {
		t.Fatalf("nodeRoleForServer() = %q, want %q", got, config.NodeTypeWorker)
	}
}

func TestSameStringSet(t *testing.T) {
	if !sameStringSet([]string{"a", "b"}, []string{"b", "a"}) {
		t.Fatal("expected sameStringSet to ignore ordering")
	}
	if sameStringSet([]string{"a"}, []string{"a", "b"}) {
		t.Fatal("expected sameStringSet to detect different lengths")
	}
}

func TestManagedServerRoleTagValueRequiresManagedTag(t *testing.T) {
	_, ok := managedServerRoleTagValue([]string{managedServerTagRolePrefix + config.NodeTypeWorker})
	if ok {
		t.Fatal("expected managedServerRoleTagValue to require managed tag")
	}
}

func TestValidateReinstallFlagsTrimsWhitespaceServerID(t *testing.T) {
	err := validateReinstallFlags("   ", true)
	if err == nil {
		t.Fatal("expected validation error for whitespace-only server ID")
	}
	if !strings.Contains(err.Error(), "--confirm-reinstall requires --server-id") {
		t.Fatalf("unexpected validation error: %v", err)
	}
}
