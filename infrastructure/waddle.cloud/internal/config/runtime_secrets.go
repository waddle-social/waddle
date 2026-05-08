package config

import (
	"context"
	"fmt"
	"strings"

	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/secrets"
)

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
