package talos

import (
	"context"
	"testing"

	"gopkg.in/yaml.v3"
)

func TestResolveInstallDisk(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  string
	}{
		{
			name:  "configured disk",
			input: "/dev/vda",
			want:  "/dev/vda",
		},
		{
			name:  "configured disk with whitespace",
			input: "  /dev/sda  ",
			want:  "/dev/sda",
		},
		{
			name:  "empty falls back to default",
			input: "",
			want:  defaultOSDisk,
		},
		{
			name:  "whitespace falls back to default",
			input: "   ",
			want:  defaultOSDisk,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := resolveInstallDisk(tt.input); got != tt.want {
				t.Fatalf("resolveInstallDisk(%q) = %q, want %q", tt.input, got, tt.want)
			}
		})
	}
}

func TestAllowSchedulingOnControlPlanes(t *testing.T) {
	tests := []struct {
		name               string
		controlPlaneTaints bool
		want               bool
	}{
		{
			name:               "taints enabled keeps control plane isolated",
			controlPlaneTaints: true,
			want:               false,
		},
		{
			name:               "taints disabled allows control plane scheduling",
			controlPlaneTaints: false,
			want:               true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := allowSchedulingOnControlPlanes(tt.controlPlaneTaints); got != tt.want {
				t.Fatalf("allowSchedulingOnControlPlanes(%t) = %t, want %t", tt.controlPlaneTaints, got, tt.want)
			}
		})
	}
}

func TestGenerateConfigAddsKubernetesAPIServerCertSANs(t *testing.T) {
	secretsYAML, err := GenerateSecretsYAML(context.Background())
	if err != nil {
		t.Fatalf("GenerateSecretsYAML returned error: %v", err)
	}

	assets, err := GenerateConfig(context.Background(), GenConfigParams{
		ClusterName:                 "production",
		Endpoint:                    "172.16.16.16",
		TalosVersion:                "v1.12.0",
		KubernetesVersion:           "v1.35.0",
		ControlPlaneTaints:          false,
		KubernetesAPIServerCertSANs: []string{"production-control-plane-01.infra.waddle.social"},
		SecretsYAML:                 secretsYAML,
	})
	if err != nil {
		t.Fatalf("GenerateConfig returned error: %v", err)
	}

	var cfg map[string]any
	if err := yaml.Unmarshal(assets.ControlPlane, &cfg); err != nil {
		t.Fatalf("unmarshal control-plane config: %v", err)
	}

	cluster := mustMap(t, cfg, "cluster")
	controlPlane := mustMap(t, cluster, "controlPlane")
	if got := controlPlane["endpoint"]; got != "https://172.16.16.16:6443" {
		t.Fatalf("cluster.controlPlane.endpoint = %v, want %q", got, "https://172.16.16.16:6443")
	}

	apiServer := mustMap(t, cluster, "apiServer")
	certSANs := mustStringSlice(t, apiServer, "certSANs")
	found := false
	for _, san := range certSANs {
		if san == "production-control-plane-01.infra.waddle.social" {
			found = true
			break
		}
	}
	if !found {
		t.Fatalf("apiServer.certSANs = %v, want production-control-plane-01.infra.waddle.social", certSANs)
	}
}
