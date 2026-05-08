package cmd

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	baremetal "github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
	"github.com/scaleway/scaleway-sdk-go/scw"
	clusterstate "github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/cluster"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/scaleway"
)

const (
	defaultTalosNodeFQDNSuffix   = "infra.waddle.social"
	defaultTalosAPINetbirdSubnet = "100.64.0.0/10"
	envTalosAllowedSubnets       = "TALOS_API_ALLOWED_SUBNETS"
	netbirdSetupKeyPrimary       = "NB_SETUP_KEY"
	netbirdSetupKeyCompatibility = "NETBIRD_SETUP_KEY"
	managedServerTagManaged      = "waddle-cloud:managed"
	managedServerTagEnvPrefix    = "waddle-cloud:env="
	managedServerTagPoolPrefix   = "waddle-cloud:pool="
	managedServerTagRolePrefix   = "waddle-cloud:role="
)

func loadConfigForClusterOrFile(clusterName, filePath string) (*config.Config, string, error) {
	resolved, err := resolveConfigPath(clusterName, filePath)
	if err != nil {
		return nil, "", err
	}

	cfg, err := config.Load(resolved)
	if err != nil {
		return nil, "", fmt.Errorf("load config %s: %w", resolved, err)
	}

	secretStore, err := getOrCreateSecretStore(context.Background(), cfg)
	if err != nil {
		return nil, "", fmt.Errorf("create secret store: %w", err)
	}
	if err := cfg.LoadRuntimeSecretsWithStore(context.Background(), secretStore); err != nil {
		return nil, "", fmt.Errorf("load runtime secrets: %w", err)
	}

	return cfg, resolved, nil
}

func resolveConfigPath(clusterName, filePath string) (string, error) {
	if p := strings.TrimSpace(filePath); p != "" {
		if _, err := os.Stat(p); err != nil {
			return "", fmt.Errorf("config file %s: %w", p, err)
		}
		return p, nil
	}

	clusterName = strings.TrimSpace(clusterName)
	if clusterName == "" {
		return "", fmt.Errorf("either --file or --cluster is required")
	}

	candidates := []string{
		clusterName + ".yaml",
		filepath.Join("clusters", clusterName+".yaml"),
	}

	for _, candidate := range candidates {
		if _, err := os.Stat(candidate); err == nil {
			return candidate, nil
		}
	}

	return "", fmt.Errorf("could not find config for cluster %q (checked %s)", clusterName, strings.Join(candidates, ", "))
}

func loadNodeState(ctx context.Context, cfg *config.Config) (*clusterstate.NodesState, error) {
	if cfg == nil {
		return nil, fmt.Errorf("config is required")
	}

	accessKey, secretKey := cfg.ScalewayCredentials()
	scwClient, err := scaleway.NewClient(accessKey, secretKey, cfg.Scaleway.ProjectID, cfg.Scaleway.OrganizationID)
	if err != nil {
		return nil, fmt.Errorf("create scaleway client: %w", err)
	}

	now := time.Now().UTC()
	seen := map[string]struct{}{}
	nodes := make([]clusterstate.NodeState, 0, len(cfg.NodePools))
	for i := range cfg.NodePools {
		pool := &cfg.NodePools[i]
		zoneValue := strings.TrimSpace(pool.EffectiveZone())
		if zoneValue == "" {
			return nil, fmt.Errorf("node pool %q must define zone", pool.Name)
		}

		req := &baremetal.ListServersRequest{
			Zone: scw.Zone(zoneValue),
		}
		if projectID := strings.TrimSpace(cfg.Scaleway.ProjectID); projectID != "" {
			req.ProjectID = &projectID
		}

		resp, err := scwClient.Baremetal.ListServers(req, scw.WithAllPages(), scw.WithContext(ctx))
		if err != nil {
			return nil, fmt.Errorf("list scaleway servers for pool %q in zone %q: %w", pool.Name, zoneValue, err)
		}

		for _, server := range resp.Servers {
			if server == nil {
				continue
			}
			if !serverBelongsToPool(cfg.Environment, pool, strings.TrimSpace(server.Name), server.Tags) {
				continue
			}
			if strings.TrimSpace(server.ID) == "" {
				continue
			}
			if _, exists := seen[server.ID]; exists {
				continue
			}
			seen[server.ID] = struct{}{}

			publicIP, privateIP := scaleway.ExtractServerIPs(server)
			role := nodeRoleForServer(pool, server.Tags)
			status := nodeStatusFromServerStatus(server.Status)
			nodes = append(nodes, clusterstate.NodeState{
				Name:      strings.TrimSpace(server.Name),
				Role:      role,
				Pool:      pool.Name,
				PublicIP:  publicIP,
				PrivateIP: privateIP,
				ServerID:  strings.TrimSpace(server.ID),
				Status:    status,
				CreatedAt: now,
				UpdatedAt: now,
			})
		}
	}

	return &clusterstate.NodesState{
		Environment: cfg.Environment,
		UpdatedAt:   now,
		Nodes:       nodes,
	}, nil
}
