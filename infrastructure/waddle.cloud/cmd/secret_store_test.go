package cmd

import (
	"context"
	"testing"

	"github.com/rawkode-academy/rawkode-cloud3/internal/config"
	"github.com/rawkode-academy/rawkode-cloud3/internal/secrets"
)

type fakeSecretStore struct {
	id int
}

func (fakeSecretStore) EnsurePath(context.Context, string) error                  { return nil }
func (fakeSecretStore) GetSecret(context.Context, string, string) (string, error) { return "", nil }
func (fakeSecretStore) GetSecrets(context.Context, string) (map[string]string, error) {
	return map[string]string{}, nil
}
func (fakeSecretStore) SetSecret(context.Context, string, string, string) error { return nil }

func TestGetOrCreateSecretStoreReusesCachedStore(t *testing.T) {
	oldNewStoreFn := secretsNewStoreFn
	oldCacheKeyFn := secretsCacheKeyFn
	t.Cleanup(func() {
		secretsNewStoreFn = oldNewStoreFn
		secretsCacheKeyFn = oldCacheKeyFn
		secretStoreCache.store = nil
		secretStoreCache.key = ""
	})

	secretStoreCache.store = nil
	secretStoreCache.key = ""

	createCalls := 0
	stubStore := &fakeSecretStore{id: 1}
	secretsCacheKeyFn = func(secrets.StoreConfig) (string, error) {
		return "cache-key", nil
	}
	secretsNewStoreFn = func(context.Context, secrets.StoreConfig) (secrets.Store, error) {
		createCalls++
		return stubStore, nil
	}

	cfg := &config.Config{
		Secrets: config.SecretsConfig{Provider: secrets.ProviderInfisical},
		Infisical: config.InfisicalConfig{
			SiteURL:      "https://app.infisical.com",
			ProjectID:    "project",
			Environment:  "production",
			ClientID:     "id",
			ClientSecret: "secret",
		},
	}

	first, err := getOrCreateSecretStore(context.Background(), cfg)
	if err != nil {
		t.Fatalf("first getOrCreateSecretStore returned error: %v", err)
	}
	second, err := getOrCreateSecretStore(context.Background(), cfg)
	if err != nil {
		t.Fatalf("second getOrCreateSecretStore returned error: %v", err)
	}

	if createCalls != 1 {
		t.Fatalf("createCalls = %d, want 1", createCalls)
	}
	if first != second {
		t.Fatal("expected cached store instance to be reused")
	}
}

func TestGetOrCreateSecretStoreCacheIsolationByKey(t *testing.T) {
	oldNewStoreFn := secretsNewStoreFn
	oldCacheKeyFn := secretsCacheKeyFn
	t.Cleanup(func() {
		secretsNewStoreFn = oldNewStoreFn
		secretsCacheKeyFn = oldCacheKeyFn
		secretStoreCache.store = nil
		secretStoreCache.key = ""
	})

	secretStoreCache.store = nil
	secretStoreCache.key = ""

	createCalls := 0
	secretsCacheKeyFn = func(cfg secrets.StoreConfig) (string, error) {
		return cfg.Provider + "-key", nil
	}
	secretsNewStoreFn = func(context.Context, secrets.StoreConfig) (secrets.Store, error) {
		createCalls++
		return &fakeSecretStore{id: createCalls}, nil
	}

	infisicalCfg := &config.Config{
		Secrets: config.SecretsConfig{Provider: secrets.ProviderInfisical},
		Infisical: config.InfisicalConfig{
			SiteURL:      "https://app.infisical.com",
			ProjectID:    "project",
			Environment:  "production",
			ClientID:     "id",
			ClientSecret: "secret",
		},
	}
	onePasswordCfg := &config.Config{
		Secrets: config.SecretsConfig{Provider: secrets.Provider1Password},
		OnePassword: config.OnePasswordConfig{
			Vault: "Employee",
		},
	}

	first, err := getOrCreateSecretStore(context.Background(), infisicalCfg)
	if err != nil {
		t.Fatalf("getOrCreateSecretStore(infisical) returned error: %v", err)
	}
	second, err := getOrCreateSecretStore(context.Background(), onePasswordCfg)
	if err != nil {
		t.Fatalf("getOrCreateSecretStore(1password) returned error: %v", err)
	}

	if createCalls != 2 {
		t.Fatalf("createCalls = %d, want 2", createCalls)
	}
	if first == second {
		t.Fatal("expected different store instances for different cache keys")
	}
}
