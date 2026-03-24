package cmd

import (
	"context"
	"fmt"
	"strings"

	scw "github.com/scaleway/scaleway-sdk-go/scw"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/scaleway"
)

type cleanupDeleteServer struct {
	ServerID string `json:"serverId"`
	Zone     string `json:"zone"`
}

func runDeleteServerCleanupAction(ctx context.Context, cfg *config.Config, payload cleanupDeleteServer) error {
	if strings.TrimSpace(payload.ServerID) == "" {
		return fmt.Errorf("cleanup payload missing serverId")
	}
	if strings.TrimSpace(payload.Zone) == "" {
		return fmt.Errorf("cleanup payload missing zone")
	}

	accessKey, secretKey := cfg.ScalewayCredentials()
	scwClient, err := scaleway.NewClient(accessKey, secretKey, cfg.Scaleway.ProjectID, cfg.Scaleway.OrganizationID)
	if err != nil {
		return fmt.Errorf("create scaleway client: %w", err)
	}

	return scaleway.CleanupProvisionedServer(ctx, scwClient, payload.ServerID, scw.Zone(payload.Zone))
}
