package cmd

import (
	"context"
	"fmt"
	"net"
	"strings"
	"time"

	baremetal "github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
	scw "github.com/scaleway/scaleway-sdk-go/scw"
	"github.com/spf13/cobra"
	clusterstate "github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/cluster"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/scaleway"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/talos"
)

var nodeCmd = &cobra.Command{
	Use:   "node",
	Short: "Node lifecycle management",
}

var nodeAddCmd = &cobra.Command{
	Use:   "add",
	Short: "Add a control plane or worker node",
	RunE:  runNodeAdd,
}

var nodeRemoveCmd = &cobra.Command{
	Use:   "remove",
	Short: "Drain and remove a node",
	RunE:  runNodeRemove,
}

func runNodeAdd(cmd *cobra.Command, args []string) error {
	ctx := cmd.Context()

	nameFlag, _ := cmd.Flags().GetString("name")
	roleRaw, _ := cmd.Flags().GetString("role")
	clusterName, _ := cmd.Flags().GetString("cluster")
	cfgFile, _ := cmd.Flags().GetString("file")
	poolName, _ := cmd.Flags().GetString("pool")
	serverIDFlag, _ := cmd.Flags().GetString("server-id")
	confirmReinstall, _ := cmd.Flags().GetBool("confirm-reinstall")
	debugProvision, _ := cmd.Flags().GetBool("debug-provision")

	if err := validateReinstallFlags(serverIDFlag, confirmReinstall); err != nil {
		return err
	}
	reinstallServerID := strings.TrimSpace(serverIDFlag)

	role := config.NormalizeNodePoolType(roleRaw)
	if role == "" {
		return fmt.Errorf("--role must be one of: control-plane, worker")
	}

	cfg, cfgPath, err := loadConfigForClusterOrFile(clusterName, cfgFile)
	if err != nil {
		return err
	}

	state, err := loadNodeState(ctx, cfg)
	if err != nil {
		return err
	}

	pool, err := resolveAddPool(cfg, poolName, role)
	if err != nil {
		return err
	}

	var (
		name      string
		privateIP string
	)

	if strings.TrimSpace(nameFlag) != "" {
		return fmt.Errorf("--name is no longer supported; names are auto-generated from pool %q", pool.Name)
	}

	slot := nextNodePoolSlot(state, cfg.Environment, pool.Name, role)
	if slot > 99 {
		return fmt.Errorf("no available naming slot for pool %q", pool.Name)
	}
	name = pooledNodeName(cfg.Environment, pool.Name, slot)

	if role == config.NodeTypeControlPlane {
		privateIP, err = controlPlaneReservedIPForSlot(pool, slot)
		if err != nil {
			return err
		}
	}

	if reinstallServerID == "" {
		if existing, ok := findNodeByName(state, name); ok && existing.Status != clusterstate.NodeStatusDeleted {
			return fmt.Errorf("node %q already exists in Scaleway inventory with status=%s", name, existing.Status)
		}
	}

	serverReady, nodeName, err := provisionNodeServer(
		ctx,
		cfg,
		pool,
		role,
		name,
		reinstallServerID,
		privateIP,
		debugProvision,
	)
	if err != nil {
		return err
	}

	publicIP := ""
	for _, ip := range serverReady.IPs {
		address := ip.Address.String()
		parsed := net.ParseIP(address)
		if parsed != nil && parsed.IsPrivate() && strings.TrimSpace(privateIP) == "" {
			privateIP = address
			continue
		}
		if ip.Version == "IPv4" {
			publicIP = address
		}
	}
	if strings.TrimSpace(publicIP) == "" {
		return fmt.Errorf("server %s has no public IPv4", serverReady.ID)
	}

	if err := enhanceTalosMaintenanceWaitError(talosWaitForMaintenanceFn(ctx, publicIP, 30*time.Minute), debugProvision); err != nil {
		return fmt.Errorf("wait for talos maintenance: %w", err)
	}

	store, err := newSecretStore(ctx, cfg)
	if err != nil {
		return err
	}

	endpoint, err := controlPlaneEndpointFromState(state)
	if err != nil {
		return fmt.Errorf("resolve control-plane endpoint for join: %w", err)
	}
	assets, err := ensureTalosAssets(ctx, cfg, endpoint, store)
	if err != nil {
		return err
	}

	nodeConfig := assets.Worker
	if role == config.NodeTypeControlPlane {
		nodeConfig = assets.ControlPlane
	}
	nodeConfig, err = renderNodeTalosConfig(nodeConfig, nodeName)
	if err != nil {
		return fmt.Errorf("render node-specific Talos config for %q: %w", nodeName, err)
	}
	netbirdSetupKey, err := loadOptionalNetbirdSetupKeyFromSecretStore(ctx, cfg, store)
	if err != nil {
		return fmt.Errorf("load netbird setup key: %w", err)
	}
	nodeConfig, err = appendNetbirdExtensionServiceConfig(nodeConfig, netbirdSetupKey)
	if err != nil {
		return fmt.Errorf("append netbird extension service config: %w", err)
	}

	talosClient, err := talos.NewInsecureClient(publicIP)
	if err != nil {
		return fmt.Errorf("create talos client: %w", err)
	}
	defer talosClient.Close()

	if err := talosClient.ApplyConfig(ctx, nodeConfig); err != nil {
		return fmt.Errorf("apply node config: %w", err)
	}

	fmt.Printf("Added %s node %q to cluster %q (config=%s, server=%s, public_ip=%s, private_ip=%s)\n", role, nodeName, cfg.Environment, cfgPath, serverReady.ID, publicIP, privateIP)
	return nil
}

func provisionNodeServer(
	ctx context.Context,
	cfg *config.Config,
	pool *config.NodePoolConfig,
	role, desiredName, reinstallServerID, privateIP string,
	debugProvision bool,
) (*baremetal.Server, string, error) {
	accessKey, secretKey := cfg.ScalewayCredentials()
	scwClient, err := scalewayNewClientFn(accessKey, secretKey, cfg.Scaleway.ProjectID, cfg.Scaleway.OrganizationID)
	if err != nil {
		return nil, "", fmt.Errorf("create scaleway client: %w", err)
	}

	zoneValue := pool.EffectiveZone()
	if zoneValue == "" {
		return nil, "", fmt.Errorf("node pool %q must define zone", pool.Name)
	}
	zone := scw.Zone(zoneValue)

	nodeName := strings.TrimSpace(desiredName)
	offerID := ""
	if reinstallServerID != "" {
		existingServer, err := scalewayGetServerFn(ctx, scwClient, zone, reinstallServerID)
		if err != nil {
			return nil, "", fmt.Errorf("load existing server %s for reinstall: %w", reinstallServerID, err)
		}
		if existingServer == nil || strings.TrimSpace(existingServer.ID) == "" {
			return nil, "", fmt.Errorf("existing server %s was not found in zone %s", reinstallServerID, zone)
		}
		offerID = strings.TrimSpace(existingServer.OfferID)
		if offerID == "" {
			return nil, "", fmt.Errorf("existing server %s has empty offer ID", reinstallServerID)
		}
	} else {
		resolvedOfferID, _, err := scalewayResolveOfferForBillingCycleFn(ctx, scwClient, zone, pool.Offer, pool.BillingCycle)
		if err != nil {
			return nil, "", fmt.Errorf("resolve offer: %w", err)
		}
		offerID = resolvedOfferID
	}

	osID, err := scalewayResolveUbuntuOSIDFn(ctx, scwClient, zone, offerID)
	if err != nil {
		return nil, "", fmt.Errorf("resolve ubuntu OS: %w", err)
	}

	region, _ := zone.Region()
	vpcName, err := cfg.ScalewayVPCName()
	if err != nil {
		return nil, "", err
	}
	privateNetworkName, err := cfg.ScalewayPrivateNetworkName()
	if err != nil {
		return nil, "", err
	}
	network, err := scalewayEnsureNetworkFoundationFn(ctx, scwClient, scaleway.NetworkFoundationParams{
		Region:                 region,
		ProjectID:              cfg.Scaleway.ProjectID,
		VPCName:                vpcName,
		PrivateNetworkName:     privateNetworkName,
		PrivateNetworkIPv4CIDR: cfg.Scaleway.PrivateNetworkIPv4CIDR,
		AllowCIDRReplacement:   strings.TrimSpace(reinstallServerID) != "",
		ReplacementServerID:    reinstallServerID,
		ReplacementServerZone:  zone,
	})
	if err != nil {
		return nil, "", fmt.Errorf("ensure network: %w", err)
	}

	cloudInit := talos.BuildCloudInit(talos.PivotParams{
		TalosVersion:   cfg.Cluster.TalosVersion,
		TalosSchematic: cfg.Cluster.TalosSchematic,
		OSDisk:         pool.Disks.OS,
		DataDisk:       pool.Disks.Data,
		DebugProvision: debugProvision,
	})
	skipDataDiskPartitioning := strings.TrimSpace(cfg.Storage.Provider) == config.StorageProviderOpenEBSMayastorLab

	var server *baremetal.Server
	if reinstallServerID != "" {
		if err := ensureReservedPrivateIPsForReinstall(
			ctx,
			scwClient,
			zone,
			pool,
			reinstallServerID,
			network.PrivateNetworkID,
			privateIP,
		); err != nil {
			if reservedIPs := reinstallReservedPrivateIPs(pool, privateIP); len(reservedIPs) > 0 {
				return nil, "", fmt.Errorf("ensure reserved private ips %s for existing server %s: %w", strings.Join(reservedIPs, ", "), reinstallServerID, err)
			}
			return nil, "", fmt.Errorf("ensure reserved private ip prerequisites for existing server %s: %w", reinstallServerID, err)
		}

		if err := scalewayEnsureServerPrivateNetworkFn(
			ctx,
			scwClient,
			zone,
			reinstallServerID,
			network.PrivateNetworkID,
			privateIP,
		); err != nil {
			return nil, "", fmt.Errorf("ensure private network attachment for existing server %s: %w", reinstallServerID, err)
		}

		server, err = scalewayReinstallServerFn(ctx, scwClient, scaleway.ReinstallParams{
			ServerID:                 reinstallServerID,
			Zone:                     zone,
			OSID:                     osID,
			CloudInitScript:          cloudInit,
			PivotOSDisk:              pool.Disks.OS,
			PivotDataDisk:            pool.Disks.Data,
			SkipDataDiskPartitioning: skipDataDiskPartitioning,
		})
		if err != nil {
			return nil, "", fmt.Errorf("reinstall server: %w", err)
		}

		server, err = scalewayEnsureServerNameFn(ctx, scwClient, zone, server, nodeName)
		if err != nil {
			return nil, "", fmt.Errorf("align reinstalled server %s name with node %q: %w", reinstallServerID, nodeName, err)
		}
	} else {
		server, err = scalewayOrderServerFn(ctx, scwClient, scaleway.ProvisionParams{
			OfferID:                  offerID,
			Zone:                     zone,
			OSID:                     osID,
			Name:                     nodeName,
			PrivateNetworkID:         network.PrivateNetworkID,
			PrivateNetworkReservedIP: privateIP,
			BillingCycle:             pool.BillingCycle,
			CloudInitScript:          cloudInit,
			PivotOSDisk:              pool.Disks.OS,
			PivotDataDisk:            pool.Disks.Data,
			SkipDataDiskPartitioning: skipDataDiskPartitioning,
		})
		if err != nil {
			return nil, "", fmt.Errorf("order server: %w", err)
		}
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
		return nil, "", fmt.Errorf("apply managed server tags: %w", err)
	}

	serverReady, err := scalewayWaitForReadyFn(ctx, scwClient, server.ID, zone)
	if err != nil {
		return nil, "", fmt.Errorf("wait for server ready: %w", err)
	}

	return serverReady, nodeName, nil
}

func runNodeRemove(cmd *cobra.Command, args []string) error {
	ctx := cmd.Context()

	name, _ := cmd.Flags().GetString("name")
	clusterName, _ := cmd.Flags().GetString("cluster")
	cfgFile, _ := cmd.Flags().GetString("file")

	if strings.TrimSpace(name) == "" {
		return fmt.Errorf("--name is required")
	}

	cfg, cfgPath, err := loadConfigForClusterOrFile(clusterName, cfgFile)
	if err != nil {
		return err
	}

	state, err := loadNodeState(ctx, cfg)
	if err != nil {
		return err
	}

	node, ok := findNodeByName(state, name)
	if !ok {
		return fmt.Errorf("node %q not found in Scaleway inventory", name)
	}
	if node.Status == clusterstate.NodeStatusDeleted {
		fmt.Printf("Node %q is already marked deleted.\n", name)
		return nil
	}

	if strings.TrimSpace(node.ServerID) != "" {
		accessKey, secretKey := cfg.ScalewayCredentials()
		scwClient, err := scaleway.NewClient(accessKey, secretKey, cfg.Scaleway.ProjectID, cfg.Scaleway.OrganizationID)
		if err != nil {
			return fmt.Errorf("create scaleway client: %w", err)
		}

		pool, err := cfg.FindNodePool(node.Pool)
		if err != nil {
			return fmt.Errorf("resolve node pool %q for node %q: %w", node.Pool, node.Name, err)
		}
		zoneValue := pool.EffectiveZone()
		if zoneValue == "" {
			return fmt.Errorf("node pool %q must define zone", pool.Name)
		}

		if err := scaleway.CleanupProvisionedServer(ctx, scwClient, node.ServerID, scw.Zone(zoneValue)); err != nil {
			return fmt.Errorf("cleanup server %s: %w", node.ServerID, err)
		}
	}

	fmt.Printf("Removed node %q from cluster %q (config=%s)\n", name, cfg.Environment, cfgPath)
	return nil
}

func resolveAddPool(cfg *config.Config, poolName, role string) (*config.NodePoolConfig, error) {
	if strings.TrimSpace(poolName) != "" {
		pool, err := cfg.FindNodePool(poolName)
		if err != nil {
			return nil, err
		}
		if pool.EffectiveType() != role {
			return nil, fmt.Errorf("pool %q has type %q, expected %q", pool.Name, pool.EffectiveType(), role)
		}
		return pool, nil
	}

	return cfg.FirstNodePoolByType(role)
}

func init() {
	nodeCmd.AddCommand(nodeAddCmd)
	nodeCmd.AddCommand(nodeRemoveCmd)

	nodeAddCmd.Flags().String("cluster", "", "Cluster/environment name")
	nodeAddCmd.Flags().StringP("file", "f", "", "Path to cluster config YAML")
	nodeAddCmd.Flags().String("name", "", "Node name (unsupported; names are auto-generated from pool slots)")
	nodeAddCmd.Flags().String("pool", "", "Node pool name (optional)")
	nodeAddCmd.Flags().String("role", "worker", "Node role (control-plane or worker)")
	nodeAddCmd.Flags().String("server-id", "", "Existing Scaleway server ID to reinstall and reuse")
	nodeAddCmd.Flags().Bool("confirm-reinstall", false, "Confirm destructive reinstall when --server-id is provided")
	nodeAddCmd.Flags().Bool("debug-provision", false, "Emit verbose cloud-init and pivot logs to the machine console during provision/reinstall")

	nodeRemoveCmd.Flags().String("cluster", "", "Cluster/environment name")
	nodeRemoveCmd.Flags().StringP("file", "f", "", "Path to cluster config YAML")
	nodeRemoveCmd.Flags().String("name", "", "Node name")
}
