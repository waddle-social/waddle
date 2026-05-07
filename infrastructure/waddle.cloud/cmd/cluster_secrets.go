package cmd

import (
	"context"
	"fmt"
	"log/slog"
	"strings"

	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/secrets"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/talos"
)

func newSecretStore(ctx context.Context, cfg *config.Config) (secrets.Store, error) {
	store, err := getOrCreateSecretStore(ctx, cfg)
	if err != nil {
		return nil, err
	}

	secretPath := secretPathForCluster(cfg)
	if err := store.EnsurePath(ctx, secretPath); err != nil {
		return nil, fmt.Errorf("ensure secret path %q: %w", secretPath, err)
	}

	return store, nil
}

func getOrCreateSecretStore(ctx context.Context, cfg *config.Config) (secrets.Store, error) {
	storeCfg, err := cfg.SecretStoreConfig()
	if err != nil {
		return nil, err
	}

	cacheKey, err := secretsCacheKeyFn(storeCfg)
	if err != nil {
		return nil, err
	}

	secretStoreCache.mu.Lock()
	defer secretStoreCache.mu.Unlock()

	if secretStoreCache.store != nil && secretStoreCache.key == cacheKey {
		return secretStoreCache.store, nil
	}

	store, err := secretsNewStoreFn(ctx, storeCfg)
	if err != nil {
		return nil, fmt.Errorf("create %s secret store: %w", strings.TrimSpace(storeCfg.Provider), err)
	}

	secretStoreCache.store = store
	secretStoreCache.key = cacheKey

	return store, nil
}

func ensureTalosSecretsYAML(ctx context.Context, cfg *config.Config, store secrets.Store) ([]byte, error) {
	secretPath := secretPathForCluster(cfg)
	all, err := store.GetSecrets(ctx, secretPath)
	if err != nil {
		return nil, fmt.Errorf("load Talos secrets from secret backend: %w", err)
	}

	if existing := strings.TrimSpace(all[talosSecretsSecretKey]); existing != "" {
		return []byte(existing), nil
	}

	slog.Info("no Talos secrets found in secret backend; generating new secrets")
	secretsYAML, err := talos.GenerateSecretsYAML(ctx)
	if err != nil {
		return nil, fmt.Errorf("generate talos secrets: %w", err)
	}

	if err := store.SetSecret(ctx, secretPath, talosSecretsSecretKey, string(secretsYAML)); err != nil {
		return nil, fmt.Errorf("store talos secrets in secret backend: %w", err)
	}

	return secretsYAML, nil
}

func ensureTalosAssets(ctx context.Context, cfg *config.Config, endpoint string, store secrets.Store) (*talos.GenConfigResult, error) {
	secretsYAML, err := ensureTalosSecretsYAML(ctx, cfg, store)
	if err != nil {
		return nil, err
	}
	kubernetesAPIEndpoint, err := canonicalKubernetesAPIEndpoint(cfg)
	if err != nil {
		return nil, fmt.Errorf("derive kubernetes API endpoint: %w", err)
	}

	installDisk := ""
	if controlPlanePool, err := cfg.FirstNodePoolByType(config.NodeTypeControlPlane); err == nil {
		installDisk = strings.TrimSpace(controlPlanePool.Disks.OS)
	}

	assets, err := talos.GenerateConfig(ctx, talos.GenConfigParams{
		ClusterName:                 cfg.Environment,
		Endpoint:                    endpoint,
		TalosVersion:                cfg.Cluster.TalosVersion,
		TalosSchematic:              cfg.Cluster.TalosSchematic,
		KubernetesVersion:           cfg.Cluster.KubernetesVersion,
		InstallDisk:                 installDisk,
		ControlPlaneTaints:          cfg.Cluster.EffectiveControlPlaneTaints(),
		KubernetesAPIServerCertSANs: []string{kubernetesAPIEndpoint},
		SecretsYAML:                 secretsYAML,
	})
	if err != nil {
		return nil, fmt.Errorf("generate talos assets: %w", err)
	}
	if strings.TrimSpace(cfg.Storage.Provider) == config.StorageProviderOpenEBSMayastorLab {
		assets.ControlPlane, err = talos.WithMayastorLabConfig(assets.ControlPlane)
		if err != nil {
			return nil, fmt.Errorf("apply mayastor Talos control-plane prerequisites: %w", err)
		}
	}

	secretPath := secretPathForCluster(cfg)
	if err := store.SetSecret(ctx, secretPath, talosControlPlaneSecretKey, string(assets.ControlPlane)); err != nil {
		return nil, fmt.Errorf("store control-plane config in secret backend: %w", err)
	}
	if err := store.SetSecret(ctx, secretPath, talosWorkerSecretKey, string(assets.Worker)); err != nil {
		return nil, fmt.Errorf("store worker config in secret backend: %w", err)
	}
	if err := store.SetSecret(ctx, secretPath, talosConfigSecretKey, string(assets.Talosconfig)); err != nil {
		return nil, fmt.Errorf("store talosconfig in secret backend: %w", err)
	}

	return assets, nil
}
