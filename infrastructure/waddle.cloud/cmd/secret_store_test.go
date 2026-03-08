package cmd

import (
	"context"
	"testing"

	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/secrets"
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

type persistentFakeSecretStore struct {
	values   map[string]map[string]string
	setCalls int
}

func (s *persistentFakeSecretStore) EnsurePath(context.Context, string) error { return nil }
func (s *persistentFakeSecretStore) GetSecret(ctx context.Context, path, key string) (string, error) {
	all, err := s.GetSecrets(ctx, path)
	if err != nil {
		return "", err
	}
	return all[key], nil
}
func (s *persistentFakeSecretStore) GetSecrets(_ context.Context, path string) (map[string]string, error) {
	if s.values == nil {
		s.values = make(map[string]map[string]string)
	}
	stored := s.values[path]
	if stored == nil {
		return map[string]string{}, nil
	}
	out := make(map[string]string, len(stored))
	for key, value := range stored {
		out[key] = value
	}
	return out, nil
}
func (s *persistentFakeSecretStore) SetSecret(_ context.Context, path, key, value string) error {
	if s.values == nil {
		s.values = make(map[string]map[string]string)
	}
	if s.values[path] == nil {
		s.values[path] = make(map[string]string)
	}
	s.values[path][key] = value
	s.setCalls++
	return nil
}

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

func TestEnsureTalosSecretsYAMLReusesStoredSecretWithinRun(t *testing.T) {
	store := &persistentFakeSecretStore{}
	cfg := &config.Config{
		Environment: "production",
		Secrets: config.SecretsConfig{
			SecretPath: "/projects/waddle-social",
		},
	}

	first, err := ensureTalosSecretsYAML(context.Background(), cfg, store)
	if err != nil {
		t.Fatalf("first ensureTalosSecretsYAML returned error: %v", err)
	}
	second, err := ensureTalosSecretsYAML(context.Background(), cfg, store)
	if err != nil {
		t.Fatalf("second ensureTalosSecretsYAML returned error: %v", err)
	}

	if store.setCalls != 1 {
		t.Fatalf("setCalls = %d, want 1", store.setCalls)
	}
	stored := store.values["/projects/waddle-social/production"][talosSecretsSecretKey]
	if stored == "" {
		t.Fatal("expected Talos secrets to be stored in the backend")
	}
	if len(first) == 0 {
		t.Fatal("expected first Talos secrets document to be non-empty")
	}
	if len(second) == 0 {
		t.Fatal("expected repeated Talos secret reads to return a non-empty backend value")
	}
}
