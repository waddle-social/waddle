package config

import (
	"context"
	"fmt"
	"net"
	"os"
	"strings"

	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/secrets"
	"gopkg.in/yaml.v3"
)

// Config is the top-level cluster configuration loaded from YAML.
type Config struct {
	Environment string            `yaml:"environment"`
	Cluster     ClusterConfig     `yaml:"cluster"`
	Scaleway    ScalewayConfig    `yaml:"scaleway"`
	NodePools   []NodePoolConfig  `yaml:"nodePools"`
	Storage     StorageConfig     `yaml:"storage"`
	Secrets     SecretsConfig     `yaml:"secrets"`
	Infisical   InfisicalConfig   `yaml:"infisical"`
	OnePassword OnePasswordConfig `yaml:"onepassword"`
	Ingress     IngressConfig     `yaml:"ingress"`
	Flux        FluxConfig        `yaml:"flux"`

	// Runtime credentials loaded from secret providers, never serialized.
	scwAccessKey string
	scwSecretKey string
}

// ClusterConfig holds Kubernetes/Talos version info.
type ClusterConfig struct {
	TalosVersion      string `yaml:"talosVersion"`
	KubernetesVersion string `yaml:"kubernetesVersion"`
	TalosSchematic    string `yaml:"talosSchematic"`
	CiliumVersion     string `yaml:"ciliumVersion"`
	FluxVersion       string `yaml:"fluxVersion"`
	// ControlPlaneTaints controls whether control-plane NoSchedule taints are kept.
	// true keeps taints (isolated control-plane), false removes them (schedulable).
	ControlPlaneTaints *bool `yaml:"controlPlaneTaints"`
}

// ScalewayConfig holds Scaleway infrastructure settings (no credentials).
type ScalewayConfig struct {
	ProjectID              string `yaml:"projectId"`
	OrganizationID         string `yaml:"organizationId"`
	PrivateNetworkIPv4CIDR string `yaml:"privateNetworkIPv4CIDR"`
}

// NodePoolConfig describes a group of nodes sharing the same hardware/disk layout.
type NodePoolConfig struct {
	Name               string     `yaml:"name"`
	Type               string     `yaml:"type"`
	Zone               string     `yaml:"zone"`
	Size               int        `yaml:"size"`
	Offer              string     `yaml:"offer"`
	BillingCycle       string     `yaml:"billingCycle"`
	Disks              DiskConfig `yaml:"disks"`
	ReservedPrivateIPs []string   `yaml:"reservedPrivateIPs"`
}

// StorageConfig holds cluster storage provisioning settings.
type StorageConfig struct {
	Provider            string `yaml:"provider"`
	DiskPoolDiskByID    string `yaml:"diskPoolDiskByID"`
	StorageClassName    string `yaml:"storageClassName"`
	DefaultStorageClass bool   `yaml:"defaultStorageClass"`
	ReplicaCount        int    `yaml:"replicaCount"`
}

const (
	NodeTypeControlPlane = "control-plane"
	NodeTypeWorker       = "worker"
)

const StorageProviderOpenEBSMayastorLab = "openebs-mayastor-lab"

const (
	defaultCiliumVersion = "v1.19.1"
	defaultFluxVersion   = "latest"
)

// DiskConfig holds disk device paths.
type DiskConfig struct {
	OS   string `yaml:"os"`
	Data string `yaml:"data"`
}

// InfisicalConfig holds Infisical backend settings.
type InfisicalConfig struct {
	SiteURL      string `yaml:"siteUrl"`
	ProjectID    string `yaml:"projectId"`
	Environment  string `yaml:"environment"`
	ClientID     string `yaml:"clientId"`
	ClientSecret string `yaml:"clientSecret"`
}

// OnePasswordConfig holds 1Password backend settings.
type OnePasswordConfig struct {
	Vault   string `yaml:"vault"`
	Account string `yaml:"account"`
}

// SecretsConfig holds provider-agnostic secret settings.
type SecretsConfig struct {
	Provider          string `yaml:"provider"`
	SecretPath        string `yaml:"secretPath"`
	NetbirdSecretPath string `yaml:"netbirdSecretPath"`
	NetbirdSecretKey  string `yaml:"netbirdSecretKey"`
}

// IngressConfig holds cluster ingress settings.
type IngressConfig struct {
	PublicIPv4 string `yaml:"publicIPv4"`
}

// FluxConfig holds FluxCD configuration.
type FluxConfig struct {
	OCIRepo string `yaml:"ociRepo"`
}

const (
	scwAccessKeySecretKey = "SCW_ACCESS_KEY"
	scwSecretKeySecretKey = "SCW_SECRET_KEY"
)

// Load reads and parses a cluster configuration YAML file.
// Environment variables override YAML values for sensitive fields.
func Load(path string) (*Config, error) {
	if path == "" {
		return nil, fmt.Errorf("config path is required")
	}

	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read config %s: %w", path, err)
	}

	var cfg Config
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("parse config %s: %w", path, err)
	}

	// Environment variable overrides for sensitive values
	if v := os.Getenv("INFISICAL_CLIENT_ID"); v != "" && cfg.Infisical.ClientID == "" {
		cfg.Infisical.ClientID = v
	}
	if v := os.Getenv("INFISICAL_CLIENT_SECRET"); v != "" && cfg.Infisical.ClientSecret == "" {
		cfg.Infisical.ClientSecret = v
	}
	if err := cfg.validateScalewayConfiguration(); err != nil {
		return nil, err
	}
	if err := cfg.validateStorageConfiguration(); err != nil {
		return nil, err
	}
	if err := cfg.validateSecretsConfiguration(); err != nil {
		return nil, err
	}
	if err := cfg.validateIngressConfiguration(); err != nil {
		return nil, err
	}

	return &cfg, nil
}

func (c *Config) validateIngressConfiguration() error {
	if c == nil {
		return fmt.Errorf("config is required")
	}

	override := strings.TrimSpace(c.Ingress.PublicIPv4)
	if override == "" {
		return nil
	}

	parsed := net.ParseIP(override)
	if parsed == nil || parsed.To4() == nil {
		return fmt.Errorf("ingress.publicIPv4 must be a valid IPv4 address")
	}

	return nil
}

func (c *Config) validateScalewayConfiguration() error {
	if c == nil {
		return fmt.Errorf("config is required")
	}

	cidrValue := strings.TrimSpace(c.Scaleway.PrivateNetworkIPv4CIDR)
	if cidrValue == "" {
		return fmt.Errorf("scaleway.privateNetworkIPv4CIDR is required")
	}

	parsedIP, network, err := net.ParseCIDR(cidrValue)
	if err != nil || parsedIP == nil || parsedIP.To4() == nil {
		return fmt.Errorf("scaleway.privateNetworkIPv4CIDR must be a valid IPv4 CIDR")
	}

	normalizedCIDR := network.String()
	c.Scaleway.PrivateNetworkIPv4CIDR = normalizedCIDR

	for _, pool := range c.NodePools {
		for _, reservedIP := range pool.ReservedPrivateIPs {
			trimmedIP := strings.TrimSpace(reservedIP)
			if trimmedIP == "" {
				continue
			}

			parsedReservedIP := net.ParseIP(trimmedIP)
			if parsedReservedIP == nil || parsedReservedIP.To4() == nil {
				return fmt.Errorf("nodePools[%s].reservedPrivateIPs contains invalid IPv4 address %q", strings.TrimSpace(pool.Name), trimmedIP)
			}

			if !network.Contains(parsedReservedIP.To4()) {
				return fmt.Errorf(
					"nodePools[%s].reservedPrivateIPs contains %s, which is outside scaleway.privateNetworkIPv4CIDR %s",
					strings.TrimSpace(pool.Name),
					trimmedIP,
					normalizedCIDR,
				)
			}
		}
	}

	return nil
}

func (c *Config) validateSecretsConfiguration() error {
	if c == nil {
		return fmt.Errorf("config is required")
	}
	if strings.TrimSpace(c.Secrets.SecretPath) == "" {
		return fmt.Errorf("secrets.secretPath is required")
	}

	cfg, err := c.secretStoreConfig()
	if err != nil {
		return err
	}

	return secrets.ValidateStoreConfig(cfg)
}

func (c *Config) validateStorageConfiguration() error {
	if c == nil {
		return fmt.Errorf("config is required")
	}

	provider := strings.TrimSpace(c.Storage.Provider)
	if provider == "" {
		return nil
	}

	if provider != StorageProviderOpenEBSMayastorLab {
		return fmt.Errorf("unsupported storage.provider %q", provider)
	}

	if len(c.NodePools) != 1 {
		return fmt.Errorf("storage.provider %q requires exactly one node pool", provider)
	}

	pool := c.NodePools[0]
	if pool.EffectiveType() != NodeTypeControlPlane {
		return fmt.Errorf("storage.provider %q requires the single node pool to be control-plane", provider)
	}
	if pool.DesiredSize() != 1 {
		return fmt.Errorf("storage.provider %q requires single-node topology", provider)
	}
	if c.Cluster.EffectiveControlPlaneTaints() {
		return fmt.Errorf("storage.provider %q requires cluster.controlPlaneTaints=false", provider)
	}

	dataDisk := strings.TrimSpace(pool.Disks.Data)
	if dataDisk == "" {
		return fmt.Errorf("storage.provider %q requires nodePools[0].disks.data", provider)
	}
	if dataDisk == strings.TrimSpace(pool.Disks.OS) {
		return fmt.Errorf("storage.provider %q requires nodePools[0].disks.data to differ from disks.os", provider)
	}
	if strings.TrimSpace(c.Storage.DiskPoolDiskByID) == "" {
		return fmt.Errorf("storage.diskPoolDiskByID is required when storage.provider=%q", provider)
	}
	if strings.TrimSpace(c.Storage.StorageClassName) == "" {
		return fmt.Errorf("storage.storageClassName is required when storage.provider=%q", provider)
	}
	if !c.Storage.DefaultStorageClass {
		return fmt.Errorf("storage.defaultStorageClass must be true when storage.provider=%q", provider)
	}
	if c.Storage.ReplicaCount != 1 {
		return fmt.Errorf("storage.replicaCount must be 1 when storage.provider=%q", provider)
	}

	return nil
}

func (c *Config) secretStoreConfig() (secrets.StoreConfig, error) {
	if c == nil {
		return secrets.StoreConfig{}, fmt.Errorf("config is required")
	}

	switch secrets.NormalizeProvider(c.Secrets.Provider) {
	case secrets.ProviderInfisical:
		return secrets.StoreConfig{
			Provider: secrets.ProviderInfisical,
			Infisical: secrets.InfisicalConfig{
				SiteURL:      strings.TrimSpace(c.Infisical.SiteURL),
				ProjectID:    strings.TrimSpace(c.Infisical.ProjectID),
				Environment:  strings.TrimSpace(c.Infisical.Environment),
				ClientID:     strings.TrimSpace(c.Infisical.ClientID),
				ClientSecret: strings.TrimSpace(c.Infisical.ClientSecret),
			},
		}, nil
	case secrets.Provider1Password:
		return secrets.StoreConfig{
			Provider: secrets.Provider1Password,
			OnePassword: secrets.OnePasswordConfig{
				Vault:   strings.TrimSpace(c.OnePassword.Vault),
				Account: strings.TrimSpace(c.OnePassword.Account),
			},
		}, nil
	default:
		return secrets.StoreConfig{}, fmt.Errorf(
			"unsupported secrets.provider %q (expected %q or %q)",
			strings.TrimSpace(c.Secrets.Provider),
			secrets.ProviderInfisical,
			secrets.Provider1Password,
		)
	}
}

// SecretStoreConfig returns the provider-specific store configuration.
func (c *Config) SecretStoreConfig() (secrets.StoreConfig, error) {
	return c.secretStoreConfig()
}

// LoadRuntimeSecrets fetches operational credentials from the configured secret backend.
func (c *Config) LoadRuntimeSecrets(ctx context.Context) error {
	if c == nil {
		return fmt.Errorf("config is required")
	}

	storeCfg, err := c.secretStoreConfig()
	if err != nil {
		return err
	}

	store, err := secrets.NewStore(ctx, storeCfg)
	if err != nil {
		return fmt.Errorf("create secret store: %w", err)
	}

	return c.LoadRuntimeSecretsWithStore(ctx, store)
}

// LoadRuntimeSecretsWithStore fetches operational credentials using a caller-managed secret store.
func (c *Config) LoadRuntimeSecretsWithStore(ctx context.Context, store secrets.Store) error {
	if c == nil {
		return fmt.Errorf("config is required")
	}
	if store == nil {
		return fmt.Errorf("secret store is required")
	}
	if strings.TrimSpace(c.Secrets.SecretPath) == "" {
		return fmt.Errorf("secrets.secretPath is required")
	}
	var err error
	c.scwAccessKey, err = requiredSecret(
		ctx, store, c.Secrets.SecretPath, scwAccessKeySecretKey,
	)
	if err != nil {
		return err
	}

	c.scwSecretKey, err = requiredSecret(
		ctx, store, c.Secrets.SecretPath, scwSecretKeySecretKey,
	)
	if err != nil {
		return err
	}

	return nil
}

func requiredSecret(
	ctx context.Context,
	store secrets.Store,
	secretPath, key string,
) (string, error) {
	value, err := store.GetSecret(ctx, secretPath, key)
	if err != nil {
		return "", fmt.Errorf("load %s from secret path %s: %w", key, secretPath, err)
	}
	trimmed := strings.TrimSpace(value)
	if trimmed == "" {
		return "", fmt.Errorf("%s is empty in secret path %s", key, secretPath)
	}

	return trimmed, nil
}

// EffectiveIngressPublicIPv4 returns the configured ingress public IPv4, or the discovered value when unset.
func (c *Config) EffectiveIngressPublicIPv4(discovered string) string {
	if c == nil {
		return strings.TrimSpace(discovered)
	}
	if override := strings.TrimSpace(c.Ingress.PublicIPv4); override != "" {
		return override
	}
	return strings.TrimSpace(discovered)
}

// Save writes the configuration back to a YAML file.
func Save(path string, cfg *Config) error {
	data, err := yaml.Marshal(cfg)
	if err != nil {
		return fmt.Errorf("marshal config: %w", err)
	}

	if err := os.WriteFile(path, data, 0o644); err != nil {
		return fmt.Errorf("write config %s: %w", path, err)
	}

	return nil
}

// ScalewayCredentials returns the Scaleway credentials loaded from the secret backend.
func (c *Config) ScalewayCredentials() (accessKey, secretKey string) {
	return c.scwAccessKey, c.scwSecretKey
}

// FindNodePool returns the NodePoolConfig with the given name, or an error.
func (c *Config) FindNodePool(name string) (*NodePoolConfig, error) {
	for i := range c.NodePools {
		if c.NodePools[i].Name == name {
			return &c.NodePools[i], nil
		}
	}
	return nil, fmt.Errorf("node pool %q not found in config", name)
}

// DefaultNodePool returns the first node pool, or an error if none exist.
func (c *Config) DefaultNodePool() (*NodePoolConfig, error) {
	if len(c.NodePools) == 0 {
		return nil, fmt.Errorf("no node pools defined in config")
	}
	return &c.NodePools[0], nil
}

// FirstNodePoolByType returns the first pool matching a normalized type.
func (c *Config) FirstNodePoolByType(poolType string) (*NodePoolConfig, error) {
	normalized := NormalizeNodePoolType(poolType)
	if normalized == "" {
		return nil, fmt.Errorf("invalid node pool type %q", poolType)
	}

	for i := range c.NodePools {
		if c.NodePools[i].EffectiveType() == normalized {
			return &c.NodePools[i], nil
		}
	}

	return nil, fmt.Errorf("no node pool with type %q found", normalized)
}

// EffectiveType returns the normalized pool type, defaulting to control-plane.
func (p NodePoolConfig) EffectiveType() string {
	if normalized := NormalizeNodePoolType(p.Type); normalized != "" {
		return normalized
	}
	return NodeTypeControlPlane
}

// DesiredSize returns the configured pool size with a sane default.
func (p NodePoolConfig) DesiredSize() int {
	if p.Size <= 0 {
		return 1
	}

	return p.Size
}

// EffectiveZone returns the node pool zone value with surrounding whitespace removed.
func (p NodePoolConfig) EffectiveZone() string {
	return strings.TrimSpace(p.Zone)
}

// EffectiveCiliumVersion returns the Cilium version with a default fallback.
func (c ClusterConfig) EffectiveCiliumVersion() string {
	if version := strings.TrimSpace(c.CiliumVersion); version != "" {
		return version
	}

	return defaultCiliumVersion
}

// EffectiveFluxVersion returns the Flux version with a default fallback.
func (c ClusterConfig) EffectiveFluxVersion() string {
	if version := strings.TrimSpace(c.FluxVersion); version != "" {
		return version
	}

	return defaultFluxVersion
}

// EffectiveControlPlaneTaints returns whether control-plane NoSchedule taints should be kept.
// Defaults to true to preserve control-plane isolation when unset.
func (c ClusterConfig) EffectiveControlPlaneTaints() bool {
	if c.ControlPlaneTaints == nil {
		return true
	}

	return *c.ControlPlaneTaints
}

// ScalewayVPCName derives the shared VPC name from the cluster/environment name.
func (c *Config) ScalewayVPCName() (string, error) {
	if c == nil {
		return "", fmt.Errorf("config is required")
	}

	name := strings.TrimSpace(c.Environment)
	if name == "" {
		return "", fmt.Errorf("environment is required to derive scaleway vpc name")
	}

	return name, nil
}

// ScalewayPrivateNetworkName derives the shared private network name from the cluster name.
func (c *Config) ScalewayPrivateNetworkName() (string, error) {
	vpcName, err := c.ScalewayVPCName()
	if err != nil {
		return "", err
	}

	return vpcName + "-private", nil
}

// StorageEnabled returns true when a storage provider has been configured.
func (c *Config) StorageEnabled() bool {
	if c == nil {
		return false
	}

	return strings.TrimSpace(c.Storage.Provider) != ""
}

// NormalizeNodePoolType normalizes user-facing type variants.
func NormalizeNodePoolType(value string) string {
	normalized := compactLower(value)
	switch normalized {
	case "", strings.ReplaceAll(NodeTypeControlPlane, "-", ""), "cp":
		return NodeTypeControlPlane
	case NodeTypeWorker:
		return NodeTypeWorker
	default:
		return ""
	}
}

func compactLower(value string) string {
	trimmed := strings.ToLower(strings.TrimSpace(value))
	return strings.NewReplacer("-", "", "_", "").Replace(trimmed)
}
