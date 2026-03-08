package secrets

import (
	"context"
	"testing"
)

type noopStore struct{}

func (noopStore) EnsurePath(context.Context, string) error                  { return nil }
func (noopStore) GetSecret(context.Context, string, string) (string, error) { return "", nil }
func (noopStore) GetSecrets(context.Context, string) (map[string]string, error) {
	return map[string]string{}, nil
}
func (noopStore) SetSecret(context.Context, string, string, string) error { return nil }

func TestValidateStoreConfigRequiresProvider(t *testing.T) {
	err := ValidateStoreConfig(StoreConfig{})
	if err == nil {
		t.Fatal("expected provider validation error")
	}
}

func TestValidateStoreConfigInfisical(t *testing.T) {
	err := ValidateStoreConfig(StoreConfig{
		Provider: ProviderInfisical,
		Infisical: InfisicalConfig{
			SiteURL:      "https://app.infisical.com",
			ProjectID:    "project",
			Environment:  "production",
			ClientID:     "id",
			ClientSecret: "secret",
		},
	})
	if err != nil {
		t.Fatalf("ValidateStoreConfig returned error: %v", err)
	}
}

func TestValidateStoreConfigOnePasswordRequiresVault(t *testing.T) {
	err := ValidateStoreConfig(StoreConfig{Provider: Provider1Password})
	if err == nil {
		t.Fatal("expected onepassword.vault validation error")
	}
}

func TestCacheKeyDiffersByProvider(t *testing.T) {
	infisicalKey, err := CacheKey(StoreConfig{
		Provider: ProviderInfisical,
		Infisical: InfisicalConfig{
			SiteURL:      "https://app.infisical.com",
			ProjectID:    "project",
			Environment:  "production",
			ClientID:     "id",
			ClientSecret: "secret",
		},
	})
	if err != nil {
		t.Fatalf("CacheKey infisical returned error: %v", err)
	}

	onePasswordKey, err := CacheKey(StoreConfig{
		Provider:    Provider1Password,
		OnePassword: OnePasswordConfig{Vault: "Employee"},
	})
	if err != nil {
		t.Fatalf("CacheKey onepassword returned error: %v", err)
	}

	if infisicalKey == onePasswordKey {
		t.Fatalf("expected different cache keys, got %q", infisicalKey)
	}
}

func TestCacheKeyOnePasswordIgnoresServiceAccountToken(t *testing.T) {
	t.Setenv("OP_SERVICE_ACCOUNT_TOKEN", "token-a")
	first, err := CacheKey(StoreConfig{
		Provider:    Provider1Password,
		OnePassword: OnePasswordConfig{Vault: "Employee", Account: "waddle-social.1password.eu"},
	})
	if err != nil {
		t.Fatalf("CacheKey first returned error: %v", err)
	}

	t.Setenv("OP_SERVICE_ACCOUNT_TOKEN", "token-b")
	second, err := CacheKey(StoreConfig{
		Provider:    Provider1Password,
		OnePassword: OnePasswordConfig{Vault: "Employee", Account: "waddle-social.1password.eu"},
	})
	if err != nil {
		t.Fatalf("CacheKey second returned error: %v", err)
	}

	if first != second {
		t.Fatalf("expected identical cache keys, got %q and %q", first, second)
	}
}

func TestNewStoreUsesInfisicalFactory(t *testing.T) {
	oldInfisicalFactory := infisicalStoreFactoryFn
	oldOnePasswordFactory := onePasswordFactoryFn
	t.Cleanup(func() {
		infisicalStoreFactoryFn = oldInfisicalFactory
		onePasswordFactoryFn = oldOnePasswordFactory
	})

	called := false
	infisicalStoreFactoryFn = func(context.Context, InfisicalConfig) (Store, error) {
		called = true
		return noopStore{}, nil
	}
	onePasswordFactoryFn = func(context.Context, OnePasswordConfig) (Store, error) {
		t.Fatal("onepassword factory should not be called")
		return nil, nil
	}

	_, err := NewStore(context.Background(), StoreConfig{
		Provider: ProviderInfisical,
		Infisical: InfisicalConfig{
			SiteURL:      "https://app.infisical.com",
			ProjectID:    "project",
			Environment:  "production",
			ClientID:     "id",
			ClientSecret: "secret",
		},
	})
	if err != nil {
		t.Fatalf("NewStore returned error: %v", err)
	}
	if !called {
		t.Fatal("expected infisical factory to be called")
	}
}

func TestNewStoreUsesOnePasswordFactory(t *testing.T) {
	oldInfisicalFactory := infisicalStoreFactoryFn
	oldOnePasswordFactory := onePasswordFactoryFn
	t.Cleanup(func() {
		infisicalStoreFactoryFn = oldInfisicalFactory
		onePasswordFactoryFn = oldOnePasswordFactory
	})

	called := false
	infisicalStoreFactoryFn = func(context.Context, InfisicalConfig) (Store, error) {
		t.Fatal("infisical factory should not be called")
		return nil, nil
	}
	onePasswordFactoryFn = func(context.Context, OnePasswordConfig) (Store, error) {
		called = true
		return noopStore{}, nil
	}

	_, err := NewStore(context.Background(), StoreConfig{
		Provider:    Provider1Password,
		OnePassword: OnePasswordConfig{Vault: "Employee"},
	})
	if err != nil {
		t.Fatalf("NewStore returned error: %v", err)
	}
	if !called {
		t.Fatal("expected onepassword factory to be called")
	}
}
