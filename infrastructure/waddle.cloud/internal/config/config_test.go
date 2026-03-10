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
	cfg := &Config{Environment: "waddle-cloud"}

	vpcName, err := cfg.ScalewayVPCName()
	if err != nil {
		t.Fatalf("ScalewayVPCName returned error: %v", err)
	}
	if vpcName != "waddle-cloud" {
		t.Fatalf("ScalewayVPCName() = %q, want %q", vpcName, "waddle-cloud")
	}

	privateName, err := cfg.ScalewayPrivateNetworkName()
	if err != nil {
		t.Fatalf("ScalewayPrivateNetworkName returned error: %v", err)
	}
	if privateName != "waddle-cloud-private" {
		t.Fatalf("ScalewayPrivateNetworkName() = %q, want %q", privateName, "waddle-cloud-private")
	}
}

func TestValidateScalewayConfigurationRequiresPrivateNetworkCIDR(t *testing.T) {
	cfg := &Config{}

	err := cfg.validateScalewayConfiguration()
	if err == nil {
		t.Fatal("expected scaleway.privateNetworkIPv4CIDR validation error")
	}
}

func TestValidateScalewayConfigurationRejectsInvalidCIDR(t *testing.T) {
	cfg := &Config{
		Scaleway: ScalewayConfig{
			PrivateNetworkIPv4CIDR: "not-a-cidr",
		},
	}

	err := cfg.validateScalewayConfiguration()
	if err == nil {
		t.Fatal("expected invalid private network cidr validation error")
	}
}

func TestValidateScalewayConfigurationRejectsReservedPrivateIPOutsideCIDR(t *testing.T) {
	cfg := &Config{
		Scaleway: ScalewayConfig{
			PrivateNetworkIPv4CIDR: "172.16.16.0/24",
		},
		NodePools: []NodePoolConfig{
			{
				Name: "control-plane",
				ReservedPrivateIPs: []string{
					"172.16.17.16",
				},
			},
		},
	}

	err := cfg.validateScalewayConfiguration()
	if err == nil {
		t.Fatal("expected reserved private IP outside CIDR validation error")
	}
}

func TestValidateScalewayConfigurationAcceptsReservedPrivateIPsInsideCIDR(t *testing.T) {
	cfg := &Config{
		Scaleway: ScalewayConfig{
			PrivateNetworkIPv4CIDR: "172.16.16.7/24",
		},
		NodePools: []NodePoolConfig{
			{
				Name: "control-plane",
				ReservedPrivateIPs: []string{
					"172.16.16.16",
					"172.16.16.17",
				},
			},
		},
	}

	err := cfg.validateScalewayConfiguration()
	if err != nil {
		t.Fatalf("validateScalewayConfiguration returned error: %v", err)
	}
	if cfg.Scaleway.PrivateNetworkIPv4CIDR != "172.16.16.0/24" {
		t.Fatalf("normalized private network cidr = %q, want %q", cfg.Scaleway.PrivateNetworkIPv4CIDR, "172.16.16.0/24")
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
			SecretPath: "/projects/waddle-cloud",
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
			SecretPath: "/projects/waddle-cloud",
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
			SecretPath: "/projects/waddle-cloud",
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

func TestValidateStorageConfigurationRequiresSingleNodeControlPlane(t *testing.T) {
	noTaints := false
	cfg := &Config{
		Cluster: ClusterConfig{
			ControlPlaneTaints: &noTaints,
		},
		Storage: StorageConfig{
			Provider:         StorageProviderOpenEBSMayastorLab,
			DiskPoolDiskByID: "/dev/disk/by-id/nvme-eui.1234",
			StorageClassName: "openebs-mayastor",
			ReplicaCount:     1,
		},
		NodePools: []NodePoolConfig{
			{
				Name: "worker",
				Type: NodeTypeWorker,
				Size: 1,
				Disks: DiskConfig{
					OS:   "/dev/nvme0n1",
					Data: "/dev/nvme1n1",
				},
			},
		},
	}

	err := cfg.validateStorageConfiguration()
	if err == nil {
		t.Fatal("expected single control-plane validation error")
	}
}

func TestValidateStorageConfigurationRejectsMissingDataDisk(t *testing.T) {
	noTaints := false
	cfg := &Config{
		Cluster: ClusterConfig{
			ControlPlaneTaints: &noTaints,
		},
		Storage: StorageConfig{
			Provider:         StorageProviderOpenEBSMayastorLab,
			DiskPoolDiskByID: "/dev/disk/by-id/nvme-eui.1234",
			StorageClassName: "openebs-mayastor",
			ReplicaCount:     1,
		},
		NodePools: []NodePoolConfig{
			{
				Name: "control-plane",
				Type: NodeTypeControlPlane,
				Size: 1,
				Disks: DiskConfig{
					OS: "/dev/nvme0n1",
				},
			},
		},
	}

	err := cfg.validateStorageConfiguration()
	if err == nil {
		t.Fatal("expected data disk validation error")
	}
}

func TestValidateStorageConfigurationRejectsMissingDiskPoolDiskByID(t *testing.T) {
	noTaints := false
	cfg := &Config{
		Cluster: ClusterConfig{
			ControlPlaneTaints: &noTaints,
		},
		Storage: StorageConfig{
			Provider:         StorageProviderOpenEBSMayastorLab,
			StorageClassName: "openebs-mayastor",
			ReplicaCount:     1,
		},
		NodePools: []NodePoolConfig{
			{
				Name: "control-plane",
				Type: NodeTypeControlPlane,
				Size: 1,
				Disks: DiskConfig{
					OS:   "/dev/nvme0n1",
					Data: "/dev/nvme1n1",
				},
			},
		},
	}

	if err := cfg.validateStorageConfiguration(); err == nil {
		t.Fatal("expected diskPoolDiskByID validation error")
	}
}

func TestValidateStorageConfigurationRejectsSharedOSAndDataDisk(t *testing.T) {
	noTaints := false
	cfg := &Config{
		Cluster: ClusterConfig{
			ControlPlaneTaints: &noTaints,
		},
		Storage: StorageConfig{
			Provider:         StorageProviderOpenEBSMayastorLab,
			DiskPoolDiskByID: "/dev/disk/by-id/nvme-eui.1234",
			StorageClassName: "openebs-mayastor",
			ReplicaCount:     1,
		},
		NodePools: []NodePoolConfig{
			{
				Name: "control-plane",
				Type: NodeTypeControlPlane,
				Size: 1,
				Disks: DiskConfig{
					OS:   "/dev/nvme0n1",
					Data: "/dev/nvme0n1",
				},
			},
		},
	}

	err := cfg.validateStorageConfiguration()
	if err == nil {
		t.Fatal("expected os/data disk validation error")
	}
}

func TestValidateStorageConfigurationRejectsControlPlaneTaintsForMayastor(t *testing.T) {
	withTaints := true
	cfg := &Config{
		Cluster: ClusterConfig{
			ControlPlaneTaints: &withTaints,
		},
		Storage: StorageConfig{
			Provider:         StorageProviderOpenEBSMayastorLab,
			DiskPoolDiskByID: "/dev/disk/by-id/nvme-eui.1234",
			StorageClassName: "openebs-mayastor",
			ReplicaCount:     1,
		},
		NodePools: []NodePoolConfig{
			{
				Name: "control-plane",
				Type: NodeTypeControlPlane,
				Size: 1,
				Disks: DiskConfig{
					OS:   "/dev/nvme0n1",
					Data: "/dev/nvme1n1",
				},
			},
		},
	}

	if err := cfg.validateStorageConfiguration(); err == nil {
		t.Fatal("expected control-plane taint validation error")
	}
}

func TestValidateStorageConfigurationRejectsReplicaCountMismatch(t *testing.T) {
	noTaints := false
	cfg := &Config{
		Cluster: ClusterConfig{
			ControlPlaneTaints: &noTaints,
		},
		Storage: StorageConfig{
			Provider:         StorageProviderOpenEBSMayastorLab,
			DiskPoolDiskByID: "/dev/disk/by-id/nvme-eui.1234",
			StorageClassName: "openebs-mayastor",
			ReplicaCount:     2,
		},
		NodePools: []NodePoolConfig{
			{
				Name: "control-plane",
				Type: NodeTypeControlPlane,
				Size: 1,
				Disks: DiskConfig{
					OS:   "/dev/nvme0n1",
					Data: "/dev/nvme1n1",
				},
			},
		},
	}

	if err := cfg.validateStorageConfiguration(); err == nil {
		t.Fatal("expected replica count validation error")
	}
}

func TestValidateStorageConfigurationAcceptsSingleNodeMayastorLab(t *testing.T) {
	noTaints := false
	cfg := &Config{
		Cluster: ClusterConfig{
			ControlPlaneTaints: &noTaints,
		},
		Storage: StorageConfig{
			Provider:            StorageProviderOpenEBSMayastorLab,
			DiskPoolDiskByID:    "/dev/disk/by-id/nvme-eui.1234",
			StorageClassName:    "openebs-mayastor",
			DefaultStorageClass: true,
			ReplicaCount:        1,
		},
		NodePools: []NodePoolConfig{
			{
				Name: "control-plane",
				Type: NodeTypeControlPlane,
				Size: 1,
				Disks: DiskConfig{
					OS:   "/dev/nvme0n1",
					Data: "/dev/nvme1n1",
				},
			},
		},
	}

	if err := cfg.validateStorageConfiguration(); err != nil {
		t.Fatalf("validateStorageConfiguration returned error: %v", err)
	}
	if !cfg.StorageEnabled() {
		t.Fatal("expected storage to be enabled")
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
