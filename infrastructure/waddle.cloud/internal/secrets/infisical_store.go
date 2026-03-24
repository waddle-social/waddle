package secrets

import (
	"context"
	"fmt"

	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/infisical"
)

type infisicalStore struct {
	client      *infisical.Client
	projectID   string
	environment string
}

func newInfisicalStore(ctx context.Context, cfg InfisicalConfig) (Store, error) {
	client, err := infisical.NewClient(ctx, cfg.SiteURL, cfg.ClientID, cfg.ClientSecret)
	if err != nil {
		return nil, fmt.Errorf("create infisical client: %w", err)
	}

	return &infisicalStore{
		client:      client,
		projectID:   cfg.ProjectID,
		environment: cfg.Environment,
	}, nil
}

func (s *infisicalStore) EnsurePath(ctx context.Context, path string) error {
	if err := s.client.EnsureSecretPath(ctx, s.projectID, s.environment, path); err != nil {
		return fmt.Errorf("ensure infisical secret path: %w", err)
	}
	return nil
}

func (s *infisicalStore) GetSecret(ctx context.Context, path, key string) (string, error) {
	value, err := s.client.GetSecret(ctx, s.projectID, s.environment, path, key)
	if err != nil {
		return "", fmt.Errorf("load %s from infisical: %w", key, err)
	}
	return value, nil
}

func (s *infisicalStore) GetSecrets(ctx context.Context, path string) (map[string]string, error) {
	values, err := s.client.GetSecrets(ctx, s.projectID, s.environment, path)
	if err != nil {
		return nil, fmt.Errorf("load infisical secrets: %w", err)
	}
	return values, nil
}

func (s *infisicalStore) SetSecret(ctx context.Context, path, key, value string) error {
	if err := s.client.SetSecret(ctx, s.projectID, s.environment, path, key, value); err != nil {
		return fmt.Errorf("store %s in infisical: %w", key, err)
	}
	return nil
}
