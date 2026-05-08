package cmd

import (
	"context"
	"fmt"
	"log/slog"
	"strings"

	"github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
	scw "github.com/scaleway/scaleway-sdk-go/scw"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/operation"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/scaleway"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/talos"
)

func phaseOrderServer(
	ctx context.Context,
	op *operation.Operation,
	cfg *config.Config,
) error {
	pool, err := poolForOperation(cfg, op)
	if err != nil {
		return fmt.Errorf("resolve node pool: %w", err)
	}

	nodeName := nodeNameForOperation(op, cfg.Environment)
	op.SetContext("nodeName", nodeName)
	role := op.GetContextString("role")
	if role == "" {
		role = pool.EffectiveType()
	}
	privateIP := strings.TrimSpace(op.GetContextString("privateIP"))
	if privateIP == "" && role == config.NodeTypeControlPlane {
		if slot, ok := parseControlPlaneSlot(cfg.Environment, pool.Name, nodeName); ok {
			reserved, err := controlPlaneReservedIPForSlot(pool, slot)
			if err != nil {
				return err
			}
			privateIP = strings.TrimSpace(reserved)
			if privateIP != "" {
				op.SetContext("privateIP", privateIP)
			}
		}
	}

	reinstall := operationContextBool(op, opContextReinstall)
	reinstallServerID := strings.TrimSpace(op.GetContextString("serverId"))
	if reinstall && reinstallServerID == "" {
		return fmt.Errorf("missing server ID in operation context for reinstall")
	}

	// Check if server already exists from a previous run
	if serverID := reinstallServerID; serverID != "" && !reinstall {
		slog.Info("server already ordered", "server_id", serverID)
		return nil
	}

	slog.Info("phase order-server: ordering Scaleway bare metal")

	scwAccessKey, scwSecretKey := cfg.ScalewayCredentials()
	scwClient, err := scalewayNewClientFn(
		scwAccessKey,
		scwSecretKey,
		cfg.Scaleway.ProjectID,
		cfg.Scaleway.OrganizationID,
	)
	if err != nil {
		return fmt.Errorf("create scaleway client: %w", err)
	}

	zoneValue := pool.EffectiveZone()
	if zoneValue == "" {
		return fmt.Errorf("node pool %q must define zone", pool.Name)
	}
	op.SetContext("zone", zoneValue)
	zone := scw.Zone(zoneValue)

	// Ensure network foundation
	region, _ := zone.Region()
	vpcName, err := cfg.ScalewayVPCName()
	if err != nil {
		return err
	}
	privateNetworkName, err := cfg.ScalewayPrivateNetworkName()
	if err != nil {
		return err
	}
	network, err := scalewayEnsureNetworkFoundationFn(ctx, scwClient, scaleway.NetworkFoundationParams{
		Region:                 region,
		ProjectID:              cfg.Scaleway.ProjectID,
		VPCName:                vpcName,
		PrivateNetworkName:     privateNetworkName,
		PrivateNetworkIPv4CIDR: cfg.Scaleway.PrivateNetworkIPv4CIDR,
		AllowCIDRReplacement:   reinstall,
		ReplacementServerID:    reinstallServerID,
		ReplacementServerZone:  zone,
	})
	if err != nil {
		return fmt.Errorf("ensure network: %w", err)
	}
	op.SetContext("privateNetworkID", network.PrivateNetworkID)

	// Build Talos pivot cloud-init
	cloudInit := talos.BuildCloudInit(talos.PivotParams{
		TalosVersion:   cfg.Cluster.TalosVersion,
		TalosSchematic: cfg.Cluster.TalosSchematic,
		OSDisk:         pool.Disks.OS,
		DataDisk:       pool.Disks.Data,
		DebugProvision: operationContextBool(op, opContextDebugProvision),
	})
	skipDataDiskPartitioning := strings.TrimSpace(cfg.Storage.Provider) == config.StorageProviderOpenEBSMayastorLab
	if reinstall {
		serverID := reinstallServerID

		existingServer, err := scalewayGetServerFn(ctx, scwClient, zone, serverID)
		if err != nil {
			return fmt.Errorf("load existing server %s for reinstall: %w", serverID, err)
		}
		if existingServer == nil || strings.TrimSpace(existingServer.ID) == "" {
			return fmt.Errorf("existing server %s was not found in zone %s", serverID, zone)
		}

		offerID := strings.TrimSpace(existingServer.OfferID)
		if offerID == "" {
			return fmt.Errorf("existing server %s has empty offer ID", serverID)
		}

		osID, err := scalewayResolveUbuntuOSIDFn(ctx, scwClient, zone, offerID)
		if err != nil {
			return fmt.Errorf("resolve ubuntu OS for existing server offer %s: %w", offerID, err)
		}

		if err := ensureReservedPrivateIPsForReinstall(
			ctx,
			scwClient,
			zone,
			pool,
			serverID,
			network.PrivateNetworkID,
			privateIP,
		); err != nil {
			if reservedIPs := reinstallReservedPrivateIPs(pool, privateIP); len(reservedIPs) > 0 {
				return fmt.Errorf("ensure reserved private ips %s for existing server %s: %w", strings.Join(reservedIPs, ", "), serverID, err)
			}
			return fmt.Errorf("ensure reserved private ip prerequisites for existing server %s: %w", serverID, err)
		}

		if err := scalewayEnsureServerPrivateNetworkFn(
			ctx,
			scwClient,
			zone,
			serverID,
			network.PrivateNetworkID,
			privateIP,
		); err != nil {
			return fmt.Errorf("ensure private network attachment for existing server %s: %w", serverID, err)
		}

		reinstalledServer, err := scalewayReinstallServerFn(ctx, scwClient, scaleway.ReinstallParams{
			ServerID:                 serverID,
			Zone:                     zone,
			OSID:                     osID,
			CloudInitScript:          cloudInit,
			PivotOSDisk:              pool.Disks.OS,
			PivotDataDisk:            pool.Disks.Data,
			SkipDataDiskPartitioning: skipDataDiskPartitioning,
		})
		if err != nil {
			return fmt.Errorf("reinstall server %s: %w", serverID, err)
		}

		reinstalledServer, err = scalewayEnsureServerNameFn(ctx, scwClient, zone, reinstalledServer, nodeName)
		if err != nil {
			return fmt.Errorf("align reinstalled server %s name with node %q: %w", serverID, nodeName, err)
		}

		reinstalledServer, err = scalewayEnsureManagedServerTagsFn(
			ctx,
			scwClient,
			zone,
			reinstalledServer,
			cfg.Environment,
			pool.Name,
			role,
		)
		if err != nil {
			return fmt.Errorf("apply managed tags to reinstalled server %s: %w", serverID, err)
		}

		op.SetContext("serverId", reinstalledServer.ID)
		slog.Info("server reinstall triggered", "server_id", reinstalledServer.ID, "node", nodeName)
		return nil
	}

	// Resolve offer and OS
	offerID, _, err := scalewayResolveOfferForBillingCycleFn(ctx, scwClient, zone, pool.Offer, pool.BillingCycle)
	if err != nil {
		return fmt.Errorf("resolve offer: %w", err)
	}

	osID, err := scalewayResolveUbuntuOSIDFn(ctx, scwClient, zone, offerID)
	if err != nil {
		return fmt.Errorf("resolve ubuntu OS: %w", err)
	}

	// Order the server
	server, err := scalewayOrderServerFn(ctx, scwClient, scaleway.ProvisionParams{
		OfferID:                  offerID,
		Zone:                     zone,
		OSID:                     osID,
		Name:                     nodeName,
		PrivateNetworkID:         network.PrivateNetworkID,
		PrivateNetworkReservedIP: privateIP,
		BillingCycle:             pool.BillingCycle,
		CloudInitScript:          cloudInit,
		SSHKeyGitHubUser:         "", // uses Scaleway API keys, falls back to default
		PivotOSDisk:              pool.Disks.OS,
		PivotDataDisk:            pool.Disks.Data,
		SkipDataDiskPartitioning: skipDataDiskPartitioning,
	})
	if err != nil {
		return fmt.Errorf("order server: %w", err)
	}

	server, err = scalewayEnsureManagedServerTagsFn(
		ctx,
		scwClient,
		zone,
		server,
		cfg.Environment,
		pool.Name,
		role,
	)
	if err != nil {
		return fmt.Errorf("apply managed tags to ordered server %s: %w", server.ID, err)
	}

	op.SetContext("serverId", server.ID)
	slog.Info("server ordered", "server_id", server.ID)
	return nil
}

func phaseWaitServer(
	ctx context.Context,
	op *operation.Operation,
	cfg *config.Config,
) error {
	serverID := op.GetContextString("serverId")
	if serverID == "" {
		return fmt.Errorf("no server ID in operation context")
	}

	slog.Info("phase wait-server: waiting for bare metal provisioning", "server_id", serverID)

	pool, err := poolForOperation(cfg, op)
	if err != nil {
		return fmt.Errorf("resolve node pool: %w", err)
	}

	scwAccessKey, scwSecretKey := cfg.ScalewayCredentials()
	scwClient, err := scaleway.NewClient(
		scwAccessKey,
		scwSecretKey,
		cfg.Scaleway.ProjectID,
		cfg.Scaleway.OrganizationID,
	)
	if err != nil {
		return fmt.Errorf("create scaleway client: %w", err)
	}

	zoneValue := strings.TrimSpace(op.GetContextString("zone"))
	if zoneValue == "" {
		zoneValue = pool.EffectiveZone()
	}
	if zoneValue == "" {
		return fmt.Errorf("node pool %q must define zone", pool.Name)
	}
	zone := scw.Zone(zoneValue)
	server, err := scalewayWaitForReadyFn(ctx, scwClient, serverID, zone)
	if err != nil {
		return fmt.Errorf("wait for server ready: %w", err)
	}

	publicIP, discoveredPrivateIP := scaleway.ExtractServerIPs(server)
	if strings.TrimSpace(op.GetContextString("privateIP")) == "" && discoveredPrivateIP != "" {
		op.SetContext("privateIP", discoveredPrivateIP)
		slog.Info("server ready", "private_ip", discoveredPrivateIP)
	}
	if publicIP != "" {
		op.SetContext("publicIP", publicIP)
		slog.Info("server ready", "public_ip", publicIP)
	}

	return nil
}

func operationContextBool(op *operation.Operation, key string) bool {
	if op == nil {
		return false
	}
	value, ok := op.GetContext(key)
	if !ok {
		return false
	}

	switch typed := value.(type) {
	case bool:
		return typed
	case string:
		normalized := strings.TrimSpace(strings.ToLower(typed))
		return normalized == "true" || normalized == "1" || normalized == "yes"
	default:
		return false
	}
}

func ensureManagedServerTags(
	ctx context.Context,
	client *scaleway.Client,
	zone scw.Zone,
	server *baremetal.Server,
	environment,
	poolName,
	role string,
) (*baremetal.Server, error) {
	if client == nil {
		return nil, fmt.Errorf("scaleway client is required")
	}
	if server == nil || strings.TrimSpace(server.ID) == "" {
		return nil, fmt.Errorf("server is required")
	}

	desired := managedServerTags(environment, poolName, role)
	merged := mergeServerTags(server.Tags, desired)
	if sameStringSet(normalizeNonEmptyStrings(server.Tags...), merged) {
		return server, nil
	}

	updated, err := client.Baremetal.UpdateServer(&baremetal.UpdateServerRequest{
		Zone:     zone,
		ServerID: server.ID,
		Tags:     &merged,
	}, scw.WithContext(ctx))
	if err != nil {
		return nil, fmt.Errorf("update server tags: %w", err)
	}

	return updated, nil
}

func ensureServerName(
	ctx context.Context,
	client *scaleway.Client,
	zone scw.Zone,
	server *baremetal.Server,
	desiredName string,
) (*baremetal.Server, error) {
	if client == nil {
		return nil, fmt.Errorf("scaleway client is required")
	}
	if server == nil || strings.TrimSpace(server.ID) == "" {
		return nil, fmt.Errorf("server is required")
	}

	trimmedName := strings.TrimSpace(desiredName)
	if trimmedName == "" {
		return nil, fmt.Errorf("desired server name is required")
	}
	if strings.TrimSpace(server.Name) == trimmedName {
		return server, nil
	}

	updated, err := client.Baremetal.UpdateServer(&baremetal.UpdateServerRequest{
		Zone:     zone,
		ServerID: server.ID,
		Name:     &trimmedName,
	}, scw.WithContext(ctx))
	if err != nil {
		return nil, fmt.Errorf("update server name: %w", err)
	}

	return updated, nil
}
