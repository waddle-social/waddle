package config

import (
	"fmt"
	"os"
	"strings"

	"gopkg.in/yaml.v3"
)

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
