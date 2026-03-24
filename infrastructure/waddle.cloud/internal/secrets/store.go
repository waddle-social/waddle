package secrets

import (
	"context"
	"errors"
	"fmt"
	"strings"
)

const (
	ProviderInfisical = "infisical"
	Provider1Password = "1password"
)

// ErrSecretNotFound indicates that a requested secret key was not found.
var ErrSecretNotFound = errors.New("secret not found")

// Store abstracts a secret backend.
type Store interface {
	EnsurePath(ctx context.Context, path string) error
	GetSecret(ctx context.Context, path, key string) (string, error)
	GetSecrets(ctx context.Context, path string) (map[string]string, error)
	SetSecret(ctx context.Context, path, key, value string) error
}

// StoreConfig is backend-agnostic secret store configuration.
type StoreConfig struct {
	Provider    string
	Infisical   InfisicalConfig
	OnePassword OnePasswordConfig
}

// InfisicalConfig contains Infisical backend settings.
type InfisicalConfig struct {
	SiteURL      string
	ProjectID    string
	Environment  string
	ClientID     string
	ClientSecret string
}

// OnePasswordConfig contains 1Password backend settings.
type OnePasswordConfig struct {
	Vault   string
	Account string
}

var (
	infisicalStoreFactoryFn = newInfisicalStore
	onePasswordFactoryFn    = NewOnePasswordStore
)

func NormalizeProvider(value string) string {
	return strings.ToLower(strings.TrimSpace(value))
}

func ValidateStoreConfig(cfg StoreConfig) error {
	provider := NormalizeProvider(cfg.Provider)
	if provider == "" {
		return fmt.Errorf("secrets.provider is required")
	}

	switch provider {
	case ProviderInfisical:
		if strings.TrimSpace(cfg.Infisical.SiteURL) == "" {
			return fmt.Errorf("infisical.siteUrl is required when secrets.provider=%q", ProviderInfisical)
		}
		if strings.TrimSpace(cfg.Infisical.ProjectID) == "" {
			return fmt.Errorf("infisical.projectId is required when secrets.provider=%q", ProviderInfisical)
		}
		if strings.TrimSpace(cfg.Infisical.Environment) == "" {
			return fmt.Errorf("infisical.environment is required when secrets.provider=%q", ProviderInfisical)
		}
		if strings.TrimSpace(cfg.Infisical.ClientID) == "" || strings.TrimSpace(cfg.Infisical.ClientSecret) == "" {
			return fmt.Errorf("INFISICAL_CLIENT_ID and INFISICAL_CLIENT_SECRET are required when secrets.provider=%q", ProviderInfisical)
		}
	case Provider1Password:
		if strings.TrimSpace(cfg.OnePassword.Vault) == "" {
			return fmt.Errorf("onepassword.vault is required when secrets.provider=%q", Provider1Password)
		}
	default:
		return fmt.Errorf("unsupported secrets.provider %q (expected %q or %q)", provider, ProviderInfisical, Provider1Password)
	}

	return nil
}

func NewStore(ctx context.Context, cfg StoreConfig) (Store, error) {
	if err := ValidateStoreConfig(cfg); err != nil {
		return nil, err
	}

	switch NormalizeProvider(cfg.Provider) {
	case ProviderInfisical:
		return infisicalStoreFactoryFn(ctx, cfg.Infisical)
	case Provider1Password:
		return onePasswordFactoryFn(ctx, cfg.OnePassword)
	default:
		return nil, fmt.Errorf("unsupported secrets.provider %q", cfg.Provider)
	}
}

func CacheKey(cfg StoreConfig) (string, error) {
	if err := ValidateStoreConfig(cfg); err != nil {
		return "", err
	}

	switch NormalizeProvider(cfg.Provider) {
	case ProviderInfisical:
		return strings.Join([]string{
			ProviderInfisical,
			strings.TrimSpace(cfg.Infisical.SiteURL),
			strings.TrimSpace(cfg.Infisical.ProjectID),
			strings.TrimSpace(cfg.Infisical.Environment),
			strings.TrimSpace(cfg.Infisical.ClientID),
			strings.TrimSpace(cfg.Infisical.ClientSecret),
		}, "|"), nil
	case Provider1Password:
		return strings.Join([]string{
			Provider1Password,
			strings.TrimSpace(cfg.OnePassword.Vault),
			strings.TrimSpace(cfg.OnePassword.Account),
		}, "|"), nil
	default:
		return "", fmt.Errorf("unsupported secrets.provider %q", cfg.Provider)
	}
}
