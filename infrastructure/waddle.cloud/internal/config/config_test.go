package config

import "testing"

func TestNormalizeNodePoolType(t *testing.T) {
	tests := []struct {
		input string
		want  string
	}{
		{input: "", want: NodeTypeControlPlane},
		{input: "control-plane", want: NodeTypeControlPlane},
		{input: "controlPlane", want: NodeTypeControlPlane},
		{input: "CONTROL_PLANE", want: NodeTypeControlPlane},
		{input: "cp", want: NodeTypeControlPlane},
		{input: "worker", want: NodeTypeWorker},
		{input: "unknown", want: ""},
	}

	for _, tt := range tests {
		if got := NormalizeNodePoolType(tt.input); got != tt.want {
			t.Fatalf("NormalizeNodePoolType(%q) = %q, want %q", tt.input, got, tt.want)
		}
	}
}

func TestFirstNodePoolByType(t *testing.T) {
	cfg := &Config{
		NodePools: []NodePoolConfig{
			{Name: "workers", Type: "worker"},
			{Name: "cp-main", Type: "control-plane"},
		},
	}

	cp, err := cfg.FirstNodePoolByType(NodeTypeControlPlane)
	if err != nil {
		t.Fatalf("FirstNodePoolByType(control-plane) returned error: %v", err)
	}
	if cp.Name != "cp-main" {
		t.Fatalf("FirstNodePoolByType(control-plane) returned %q, want %q", cp.Name, "cp-main")
	}

	worker, err := cfg.FirstNodePoolByType(NodeTypeWorker)
	if err != nil {
		t.Fatalf("FirstNodePoolByType(worker) returned error: %v", err)
	}
	if worker.Name != "workers" {
		t.Fatalf("FirstNodePoolByType(worker) returned %q, want %q", worker.Name, "workers")
	}
}

func TestScalewayNetworkNameDerivation(t *testing.T) {
	cfg := &Config{Environment: "rawkode-cloud"}

	vpcName, err := cfg.ScalewayVPCName()
	if err != nil {
		t.Fatalf("ScalewayVPCName returned error: %v", err)
	}
	if vpcName != "rawkode-cloud" {
		t.Fatalf("ScalewayVPCName() = %q, want %q", vpcName, "rawkode-cloud")
	}

	privateName, err := cfg.ScalewayPrivateNetworkName()
	if err != nil {
		t.Fatalf("ScalewayPrivateNetworkName returned error: %v", err)
	}
	if privateName != "rawkode-cloud-private" {
		t.Fatalf("ScalewayPrivateNetworkName() = %q, want %q", privateName, "rawkode-cloud-private")
	}
}

func TestNodePoolEffectiveZone(t *testing.T) {
	pool := NodePoolConfig{Zone: " fr-par-1 "}
	if got := pool.EffectiveZone(); got != "fr-par-1" {
		t.Fatalf("NodePoolConfig.EffectiveZone() = %q, want %q", got, "fr-par-1")
	}
}

func TestClusterEffectiveCiliumVersion(t *testing.T) {
	if got := (ClusterConfig{}).EffectiveCiliumVersion(); got != defaultCiliumVersion {
		t.Fatalf("ClusterConfig{}.EffectiveCiliumVersion() = %q, want %q", got, defaultCiliumVersion)
	}

	cfg := ClusterConfig{CiliumVersion: "v1.18.6"}
	if got := cfg.EffectiveCiliumVersion(); got != "v1.18.6" {
		t.Fatalf("ClusterConfig{CiliumVersion: v1.18.6}.EffectiveCiliumVersion() = %q, want %q", got, "v1.18.6")
	}
}

func TestClusterEffectiveFluxVersion(t *testing.T) {
	if got := (ClusterConfig{}).EffectiveFluxVersion(); got != defaultFluxVersion {
		t.Fatalf("ClusterConfig{}.EffectiveFluxVersion() = %q, want %q", got, defaultFluxVersion)
	}

	cfg := ClusterConfig{FluxVersion: "v2.8.0"}
	if got := cfg.EffectiveFluxVersion(); got != "v2.8.0" {
		t.Fatalf("ClusterConfig{FluxVersion: v2.8.0}.EffectiveFluxVersion() = %q, want %q", got, "v2.8.0")
	}
}

func TestClusterEffectiveControlPlaneTaints(t *testing.T) {
	if got := (ClusterConfig{}).EffectiveControlPlaneTaints(); !got {
		t.Fatalf("ClusterConfig{}.EffectiveControlPlaneTaints() = %t, want true", got)
	}

	keepTaints := true
	cfgKeep := ClusterConfig{ControlPlaneTaints: &keepTaints}
	if got := cfgKeep.EffectiveControlPlaneTaints(); !got {
		t.Fatalf("ClusterConfig{ControlPlaneTaints:true}.EffectiveControlPlaneTaints() = %t, want true", got)
	}

	removeTaints := false
	cfgRemove := ClusterConfig{ControlPlaneTaints: &removeTaints}
	if got := cfgRemove.EffectiveControlPlaneTaints(); got {
		t.Fatalf("ClusterConfig{ControlPlaneTaints:false}.EffectiveControlPlaneTaints() = %t, want false", got)
	}
}

func TestValidateSecretsConfigurationRequiresProvider(t *testing.T) {
	cfg := &Config{
		Secrets: SecretsConfig{
			SecretPath: "/projects/rawkode-cloud",
		},
	}

	err := cfg.validateSecretsConfiguration()
	if err == nil {
		t.Fatal("expected secrets.provider validation error")
	}
}

func TestValidateSecretsConfigurationInfisical(t *testing.T) {
	cfg := &Config{
		Secrets: SecretsConfig{
			Provider:   "infisical",
			SecretPath: "/projects/rawkode-cloud",
		},
		Infisical: InfisicalConfig{
			SiteURL:     "https://app.infisical.com",
			ProjectID:   "project-id",
			Environment: "production",
		},
	}

	err := cfg.validateSecretsConfiguration()
	if err == nil {
		t.Fatal("expected infisical client credential validation error")
	}

	cfg.Infisical.ClientID = "client-id"
	cfg.Infisical.ClientSecret = "client-secret"
	err = cfg.validateSecretsConfiguration()
	if err != nil {
		t.Fatalf("validateSecretsConfiguration returned error: %v", err)
	}
}

func TestValidateSecretsConfigurationOnePasswordRequiresVault(t *testing.T) {
	cfg := &Config{
		Secrets: SecretsConfig{
			Provider:   "1password",
			SecretPath: "/projects/rawkode-cloud",
		},
	}

	err := cfg.validateSecretsConfiguration()
	if err == nil {
		t.Fatal("expected onepassword.vault validation error")
	}

	cfg.OnePassword.Vault = "Employee"
	err = cfg.validateSecretsConfiguration()
	if err != nil {
		t.Fatalf("validateSecretsConfiguration returned error: %v", err)
	}
}

func TestValidateIngressConfiguration(t *testing.T) {
	cfg := &Config{}
	if err := cfg.validateIngressConfiguration(); err != nil {
		t.Fatalf("validateIngressConfiguration returned error for empty config: %v", err)
	}

	cfg.Ingress.PublicIPv4 = "not-an-ip"
	if err := cfg.validateIngressConfiguration(); err == nil {
		t.Fatal("expected ingress.publicIPv4 validation error")
	}

	cfg.Ingress.PublicIPv4 = "51.159.1.2"
	if err := cfg.validateIngressConfiguration(); err != nil {
		t.Fatalf("validateIngressConfiguration returned error: %v", err)
	}
}

func TestEffectiveIngressPublicIPv4(t *testing.T) {
	cfg := &Config{}
	if got := cfg.EffectiveIngressPublicIPv4("51.159.1.2"); got != "51.159.1.2" {
		t.Fatalf("EffectiveIngressPublicIPv4(discovered) = %q, want %q", got, "51.159.1.2")
	}

	cfg.Ingress.PublicIPv4 = "203.0.113.10"
	if got := cfg.EffectiveIngressPublicIPv4("51.159.1.2"); got != "203.0.113.10" {
		t.Fatalf("EffectiveIngressPublicIPv4(override) = %q, want %q", got, "203.0.113.10")
	}
}
