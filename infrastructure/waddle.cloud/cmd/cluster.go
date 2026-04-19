package cmd

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"strings"
	"sync"

	"github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
	scw "github.com/scaleway/scaleway-sdk-go/scw"
	"github.com/spf13/cobra"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/cilium"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/flux"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/operation"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/scaleway"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/secrets"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/storage"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/talos"
	"k8s.io/client-go/discovery"
	"k8s.io/client-go/tools/clientcmd"

	"time"
)

var clusterCmd = &cobra.Command{
	Use:   "cluster",
	Short: "Cluster lifecycle management",
}

const (
	talosSecretsSecretKey                = "TALOS_SECRETS_YAML"
	talosControlPlaneSecretKey           = "TALOS_CONTROL_PLANE_CONFIG_YAML"
	talosWorkerSecretKey                 = "TALOS_WORKER_CONFIG_YAML"
	talosConfigSecretKey                 = "TALOSCONFIG_YAML"
	opContextNetbirdSecretPath           = "netbirdSecretPath"
	opContextNetbirdSecretKey            = "netbirdSecretKey"
	opContextReinstall                   = "reinstall"
	opContextDebugProvision              = "debugProvision"
	fluxSubstituteStorageClass           = "WADDLE_STORAGE_CLASS_NAME"
	fluxSubstituteStorageNode            = "WADDLE_STORAGE_NODE_NAME"
	fluxSubstituteDiskPoolDisk           = "WADDLE_STORAGE_DISKPOOL_DISK_BY_ID"
	fluxSubstituteEnvironment            = "WADDLE_CLUSTER_ENVIRONMENT"
	fluxSubstituteTeleportGitHubClientID = "WADDLE_TELEPORT_GITHUB_CLIENT_ID"
)

var secretStoreCache struct {
	mu    sync.Mutex
	key   string
	store secrets.Store
}

var clusterCreateCmd = &cobra.Command{
	Use:   "create",
	Short: "Create a new Talos Kubernetes cluster",
	RunE:  runClusterCreate,
}

var clusterStatusCmd = &cobra.Command{
	Use:   "status",
	Short: "Show cluster health and drift detection",
	RunE: func(cmd *cobra.Command, args []string) error {
		cfgPath, _ := cmd.Flags().GetString("config")
		cfg, err := config.Load(cfgPath)
		if err != nil {
			return fmt.Errorf("load config: %w", err)
		}

		fmt.Printf("Cluster: %s\n", cfg.Environment)
		fmt.Printf("Talos:   %s\n", cfg.Cluster.TalosVersion)
		fmt.Printf("K8s:     %s\n", cfg.Cluster.KubernetesVersion)
		fmt.Printf("Cilium:  %s\n", cfg.Cluster.EffectiveCiliumVersion())
		fmt.Printf("Flux:    %s\n", cfg.Cluster.EffectiveFluxVersion())
		fmt.Printf("Pools:   %d\n", len(cfg.NodePools))

		for _, pool := range cfg.NodePools {
			fmt.Printf("  - %s (offer=%s, billing=%s)\n", pool.Name, pool.Offer, pool.BillingCycle)
		}

		return nil
	},
}

func init() {
	clusterCmd.AddCommand(clusterCreateCmd)
	clusterCmd.AddCommand(clusterDeleteCmd)
	clusterCmd.AddCommand(clusterStatusCmd)
	clusterCmd.AddCommand(clusterScaffoldCmd)

	clusterCreateCmd.Flags().StringP("environment", "e", "", "Cluster/environment name")
	clusterCreateCmd.Flags().StringP("file", "f", "", "Path to cluster config YAML")
	clusterCreateCmd.Flags().String("node-name", "", "Deprecated: control-plane names are now auto-generated from the pool")
	clusterCreateCmd.Flags().String("pool", "", "Node pool name (defaults to first control-plane pool)")
	clusterCreateCmd.Flags().String("netbird-secret-path", "", "Secret backend path for Netbird setup key lookup (overrides secrets.netbirdSecretPath; defaults to secrets.secretPath)")
	clusterCreateCmd.Flags().String("netbird-secret-key", "", "Secret backend key for Netbird setup key lookup (overrides secrets.netbirdSecretKey)")
	clusterCreateCmd.Flags().String("server-id", "", "Existing Scaleway server ID to reinstall and reuse")
	clusterCreateCmd.Flags().Bool("confirm-reinstall", false, "Confirm destructive reinstall when --server-id is provided")
	clusterCreateCmd.Flags().Bool("debug-provision", false, "Emit verbose cloud-init and pivot logs to the machine console during provision/reinstall")

	clusterDeleteCmd.Flags().StringP("environment", "e", "", "Cluster/environment name")
	clusterDeleteCmd.Flags().StringP("file", "f", "", "Path to cluster config YAML")

	clusterStatusCmd.Flags().String("config", "", "Path to cluster config YAML")
}

// createClusterPhases defines the phase order for creating a cluster.
var createClusterPhases = []string{
	"init",
	"generate-config",
	"order-server",
	"wait-server",
	"wait-talos",
	"apply-config",
	"bootstrap",
	"post-bootstrap",
	"verify",
	"restrict-talos-api",
}

var (
	ciliumInstallFn                          = cilium.Install
	fluxBootstrapFn                          = flux.Bootstrap
	postBootstrapKubeconfigPathFn            = postBootstrapKubeconfigPath
	postBootstrapKubeconfigRetryInterval     = 15 * time.Second
	postBootstrapKubeconfigRetryTimeout      = 30 * time.Minute
	postBootstrapKubernetesAPIProbeFn        = postBootstrapKubernetesAPIReachable
	postBootstrapKubernetesAPIWaitFn         = waitForKubernetesAPIWithRetry
	postBootstrapKubernetesAPIRetryInterval  = 5 * time.Second
	postBootstrapKubernetesAPIRetryTimeout   = 10 * time.Minute
	storagePrepareOpenEBSMayastorFn          = storage.PrepareOpenEBSMayastorRawDisk
	postBootstrapBootstrapSecretsFn          = prepareBootstrapSecrets
	scalewayNewClientFn                      = scaleway.NewClient
	scalewayResolveOfferForBillingCycleFn    = scaleway.ResolveOfferForBillingCycle
	scalewayResolveUbuntuOSIDFn              = scaleway.ResolveUbuntuOSID
	scalewayOrderServerFn                    = scaleway.OrderServer
	scalewayReinstallServerFn                = scaleway.ReinstallServer
	scalewayWaitForReadyFn                   = scaleway.WaitForReady
	scalewayEnsureReservedPrivateNetworkIPFn = scaleway.EnsureReservedPrivateNetworkIP
	scalewayEnsureServerPrivateNetworkFn     = scaleway.EnsureServerPrivateNetworkAttachment
	scalewayGetServerFn                      = func(ctx context.Context, client *scaleway.Client, zone scw.Zone, serverID string) (*baremetal.Server, error) {
		return client.Baremetal.GetServer(&baremetal.GetServerRequest{
			Zone:     zone,
			ServerID: serverID,
		}, scw.WithContext(ctx))
	}
	scalewayEnsureNetworkFoundationFn       = scaleway.EnsureNetworkFoundation
	scalewayResolvePrivateNetworkIPv4CIDRFn = scaleway.ResolvePrivateNetworkIPv4CIDR
	scalewayEnsureServerNameFn              = ensureServerName
	scalewayEnsureManagedServerTagsFn       = ensureManagedServerTags
	secretsNewStoreFn                       = secrets.NewStore
	secretsCacheKeyFn                       = secrets.CacheKey
	talosWaitForMaintenanceFn               = talos.WaitForMaintenance
)

func runClusterCreate(cmd *cobra.Command, args []string) error {
	ctx := cmd.Context()
	clusterName, _ := cmd.Flags().GetString("environment")
	cfgPathFlag, _ := cmd.Flags().GetString("file")
	nodeNameFlag, _ := cmd.Flags().GetString("node-name")
	poolName, _ := cmd.Flags().GetString("pool")
	netbirdSecretPathFlag, _ := cmd.Flags().GetString("netbird-secret-path")
	netbirdSecretKeyFlag, _ := cmd.Flags().GetString("netbird-secret-key")
	serverIDFlag, _ := cmd.Flags().GetString("server-id")
	confirmReinstall, _ := cmd.Flags().GetBool("confirm-reinstall")
	debugProvision, _ := cmd.Flags().GetBool("debug-provision")

	if err := validateReinstallFlags(serverIDFlag, confirmReinstall); err != nil {
		return err
	}

	cfg, cfgPath, err := loadConfigForClusterOrFile(clusterName, cfgPathFlag)
	if err != nil {
		return err
	}

	pool, err := selectCreatePool(cfg, poolName)
	if err != nil {
		return err
	}

	if pool.EffectiveType() != config.NodeTypeControlPlane {
		return fmt.Errorf("cluster create currently supports control-plane pools only (pool %q is %q)", pool.Name, pool.EffectiveType())
	}
	if strings.TrimSpace(nodeNameFlag) != "" {
		return fmt.Errorf(
			"--node-name is no longer supported; first control-plane node will be %q",
			controlPlaneNodeName(cfg.Environment, pool.Name, 1),
		)
	}

	nodeName := controlPlaneNodeName(cfg.Environment, pool.Name, 1)
	privateIP, err := controlPlaneReservedIPForSlot(pool, 1)
	if err != nil {
		return err
	}
	netbirdSecretPath := strings.TrimSpace(netbirdSecretPathFlag)
	if netbirdSecretPath == "" {
		netbirdSecretPath = strings.TrimSpace(cfg.Secrets.NetbirdSecretPath)
	}
	if netbirdSecretPath == "" {
		netbirdSecretPath = strings.TrimSpace(cfg.Secrets.SecretPath)
	}
	if netbirdSecretPath == "" {
		netbirdSecretPath = secretPathForCluster(cfg)
	}
	netbirdSecretKey := strings.TrimSpace(netbirdSecretKeyFlag)
	if netbirdSecretKey == "" {
		netbirdSecretKey = strings.TrimSpace(cfg.Secrets.NetbirdSecretKey)
	}

	op, err := operation.New(operation.GenerateID(), operation.TypeCreateCluster, cfg.Environment, createClusterPhases)
	if err != nil {
		return fmt.Errorf("create operation: %w", err)
	}
	op.SetContext("nodeName", nodeName)
	op.SetContext("role", pool.EffectiveType())
	op.SetContext("poolName", pool.Name)
	op.SetContext("controlPlaneSlot", "1")
	op.SetContext(opContextReinstall, confirmReinstall)
	op.SetContext(opContextDebugProvision, debugProvision)
	if privateIP != "" {
		op.SetContext("privateIP", privateIP)
	}
	if serverID := strings.TrimSpace(serverIDFlag); serverID != "" {
		op.SetContext("serverId", serverID)
	}
	op.SetContext(opContextNetbirdSecretPath, netbirdSecretPath)
	op.SetContext(opContextNetbirdSecretKey, netbirdSecretKey)

	resumePhase := op.ResumePhase()
	if resumePhase == "" {
		fmt.Println("Operation already complete.")
		return nil
	}

	slog.Info("starting create-cluster operation",
		"operation", op.ID,
		"cluster", cfg.Environment,
		"config", cfgPath,
		"pool", op.GetContextString("poolName"),
		"resume_from", resumePhase,
	)

	return executeCreateCluster(ctx, op, cfg)
}

func executeCreateCluster(
	ctx context.Context,
	op *operation.Operation,
	cfg *config.Config,
) error {
	for {
		phase := op.ResumePhase()
		if phase == "" {
			fmt.Println("Cluster creation complete!")
			return nil
		}

		slog.Info("executing phase", "phase", phase, "operation", op.ID)

		if err := op.StartPhase(phase); err != nil {
			return fmt.Errorf("start phase %s: %w", phase, err)
		}

		var phaseErr error
		switch phase {
		case "init":
			phaseErr = phaseInit(ctx, op, cfg)
		case "generate-config":
			phaseErr = phaseGenerateConfig(ctx, op, cfg)
		case "order-server":
			phaseErr = phaseOrderServer(ctx, op, cfg)
		case "wait-server":
			phaseErr = phaseWaitServer(ctx, op, cfg)
		case "wait-talos":
			phaseErr = phaseWaitTalos(ctx, op, cfg)
		case "apply-config":
			phaseErr = phaseApplyConfig(ctx, op, cfg)
		case "bootstrap":
			phaseErr = phaseBootstrap(ctx, op, cfg)
		case "post-bootstrap":
			phaseErr = phasePostBootstrap(ctx, op, cfg)
		case "verify":
			phaseErr = phaseVerify(ctx, op, cfg)
		case "restrict-talos-api":
			phaseErr = phaseRestrictTalosAPI(ctx, op, cfg)
		default:
			phaseErr = fmt.Errorf("unknown phase %q", phase)
		}

		if phaseErr != nil {
			_ = op.FailPhase(phase, phaseErr)
			return fmt.Errorf("phase %s failed: %w", phase, phaseErr)
		}

		if err := op.CompletePhase(phase, nil); err != nil {
			return fmt.Errorf("complete phase %s: %w", phase, err)
		}
	}
}

// Phase implementations

func phaseInit(ctx context.Context, op *operation.Operation, cfg *config.Config) error {
	slog.Info("phase init: ensuring Talos secrets are available in secret backend")

	store, err := newSecretStore(ctx, cfg)
	if err != nil {
		return err
	}
	if _, err := ensureTalosSecretsYAML(ctx, cfg, store); err != nil {
		return err
	}

	op.SetContext("secretsPath", secretPathForCluster(cfg))
	return nil
}

func phaseGenerateConfig(ctx context.Context, op *operation.Operation, cfg *config.Config) error {
	slog.Info("phase generate-config: validating Talos generation prerequisites")

	store, err := newSecretStore(ctx, cfg)
	if err != nil {
		return err
	}
	if _, err := ensureTalosSecretsYAML(ctx, cfg, store); err != nil {
		return err
	}

	return nil
}

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

func phaseWaitTalos(ctx context.Context, op *operation.Operation, cfg *config.Config) error {
	publicIP := op.GetContextString("publicIP")
	if publicIP == "" {
		return fmt.Errorf("no public IP in operation context")
	}

	slog.Info("phase wait-talos: waiting for Talos maintenance mode", "ip", publicIP)
	err := talosWaitForMaintenanceFn(ctx, publicIP, 30*time.Minute)
	return enhanceTalosMaintenanceWaitError(err, operationContextBool(op, opContextDebugProvision))
}

func enhanceTalosMaintenanceWaitError(err error, debugProvision bool) error {
	if err == nil || !debugProvision {
		return err
	}

	var timeoutErr *talos.MaintenanceTimeoutError
	if errors.As(err, &timeoutErr) {
		return fmt.Errorf(
			"%w; inspect the Scaleway remote console and the pivot Ubuntu logs /var/log/cloud-init-output.log and /var/log/waddle-cloud-pivot.log",
			err,
		)
	}

	return err
}

func phaseApplyConfig(ctx context.Context, op *operation.Operation, cfg *config.Config) error {
	publicIP := op.GetContextString("publicIP")
	if publicIP == "" {
		return fmt.Errorf("no public IP in operation context")
	}

	endpoint := controlPlaneEndpoint(op.GetContextString("privateIP"), publicIP)
	op.SetContext("controlPlaneEndpoint", endpoint)

	store, err := newSecretStore(ctx, cfg)
	if err != nil {
		return err
	}
	assets, err := ensureTalosAssets(ctx, cfg, endpoint, store)
	if err != nil {
		return err
	}
	nodeName := nodeNameForOperation(op, cfg.Environment)
	nodeConfig, err := renderNodeTalosConfig(assets.ControlPlane, nodeName)
	if err != nil {
		return fmt.Errorf("render node-specific Talos config for %q: %w", nodeName, err)
	}
	netbirdSecretPath := strings.TrimSpace(op.GetContextString(opContextNetbirdSecretPath))
	netbirdSecretKey := strings.TrimSpace(op.GetContextString(opContextNetbirdSecretKey))
	netbirdSetupKey, err := loadOptionalNetbirdSetupKeyFromSecretStoreWithOverrides(ctx, cfg, store, netbirdSecretPath, netbirdSecretKey)
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

	slog.Info("phase apply-config: applying Talos control-plane config", "ip", publicIP, "endpoint", endpoint, "node", nodeName)
	if err := talosClient.ApplyConfig(ctx, nodeConfig); err != nil {
		return err
	}
	op.SetContext("talosActivated", true)

	return nil
}

func phaseBootstrap(ctx context.Context, op *operation.Operation, cfg *config.Config) error {
	publicIP := op.GetContextString("publicIP")
	if publicIP == "" {
		return fmt.Errorf("no public IP in operation context")
	}

	endpoint := op.GetContextString("controlPlaneEndpoint")
	if endpoint == "" {
		endpoint = controlPlaneEndpoint(op.GetContextString("privateIP"), publicIP)
	}

	store, err := newSecretStore(ctx, cfg)
	if err != nil {
		return err
	}
	assets, err := ensureTalosAssets(ctx, cfg, endpoint, store)
	if err != nil {
		return err
	}

	talosClient, err := talos.NewClient(publicIP, assets.Talosconfig)
	if err != nil {
		return fmt.Errorf("create talos mTLS client: %w", err)
	}
	defer talosClient.Close()

	// apply-config reboots into configured mode; bootstrap can race the API coming up.
	var lastErr error
	for attempt := 1; attempt <= 40; attempt++ {
		lastErr = talosClient.Bootstrap(ctx)
		if lastErr == nil {
			return nil
		}
		slog.Info("waiting to retry bootstrap", "attempt", attempt, "error", lastErr)
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(15 * time.Second):
		}
	}

	return fmt.Errorf("bootstrap did not succeed after retries: %w", lastErr)
}

func phasePostBootstrap(ctx context.Context, op *operation.Operation, cfg *config.Config) error {
	if op == nil {
		return fmt.Errorf("operation context is required for post-bootstrap")
	}

	maybeRefreshOperationServerIPs(ctx, op, cfg)

	zoneValue := strings.TrimSpace(op.GetContextString("zone"))
	if zoneValue == "" {
		return fmt.Errorf("missing zone in operation context")
	}

	zone := scw.Zone(zoneValue)
	region, err := zone.Region()
	if err != nil {
		return fmt.Errorf("derive region from zone %q: %w", zoneValue, err)
	}

	preferredPrivateIP := strings.TrimSpace(op.GetContextString("privateIP"))

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

	privateNetworkID := strings.TrimSpace(op.GetContextString("privateNetworkID"))
	if privateNetworkID == "" {
		vpcName, err := cfg.ScalewayVPCName()
		if err != nil {
			return fmt.Errorf("resolve missing privateNetworkID context: %w", err)
		}
		privateNetworkName, err := cfg.ScalewayPrivateNetworkName()
		if err != nil {
			return fmt.Errorf("resolve missing privateNetworkID context: %w", err)
		}

		network, err := scalewayEnsureNetworkFoundationFn(ctx, scwClient, scaleway.NetworkFoundationParams{
			Region:                 region,
			ProjectID:              cfg.Scaleway.ProjectID,
			VPCName:                vpcName,
			PrivateNetworkName:     privateNetworkName,
			PrivateNetworkIPv4CIDR: cfg.Scaleway.PrivateNetworkIPv4CIDR,
		})
		if err != nil {
			return fmt.Errorf("resolve missing privateNetworkID context: %w", err)
		}

		privateNetworkID = strings.TrimSpace(network.PrivateNetworkID)
		if privateNetworkID == "" {
			return fmt.Errorf("resolve missing privateNetworkID context: resolved empty private network ID")
		}
		op.SetContext("privateNetworkID", privateNetworkID)
		slog.Info("resolved missing private network id from scaleway network foundation",
			"private_network_id", privateNetworkID,
			"vpc", vpcName,
			"private_network", privateNetworkName,
		)
	}

	ipv4NativeRoutingCIDR, err := scalewayResolvePrivateNetworkIPv4CIDRFn(
		ctx,
		scwClient,
		region,
		privateNetworkID,
		preferredPrivateIP,
	)
	if err != nil {
		return fmt.Errorf("resolve cilium ipv4 native routing cidr: %w", err)
	}
	op.SetContext("ipv4NativeRoutingCIDR", ipv4NativeRoutingCIDR)

	slog.Info("phase post-bootstrap: installing Cilium and FluxCD")
	slog.Info("resolved cilium native routing cidr",
		"private_network_id", privateNetworkID,
		"preferred_private_ip", preferredPrivateIP,
		"cidr", ipv4NativeRoutingCIDR,
	)

	kubeconfigPath, cleanupKubeconfig, err := prepareBootstrapKubeconfigWithRetry(ctx, op, cfg)
	if err != nil {
		return fmt.Errorf("prepare bootstrap kubeconfig: %w", err)
	}
	if cleanupKubeconfig != nil {
		defer cleanupKubeconfig()
	}

	if err := postBootstrapKubernetesAPIWaitFn(ctx, kubeconfigPath); err != nil {
		return fmt.Errorf("wait for kubernetes API readiness: %w", err)
	}

	var bootstrapErrors []error
	storagePrepBlockedFlux := false

	// Install Cilium CNI
	if err := ciliumInstallFn(ctx, cilium.InstallParams{
		Kubeconfig:            kubeconfigPath,
		Version:               cfg.Cluster.EffectiveCiliumVersion(),
		Hubble:                true,
		IPv4NativeRoutingCIDR: ipv4NativeRoutingCIDR,
	}); err != nil {
		slog.Warn("cilium install failed", "error", err)
		bootstrapErrors = append(bootstrapErrors, fmt.Errorf("install cilium: %w", err))
	}

	if len(bootstrapErrors) == 0 {
		switch strings.TrimSpace(cfg.Storage.Provider) {
		case "":
			// No storage provider configured.
		case config.StorageProviderOpenEBSMayastorLab:
			pool, err := cfg.FirstNodePoolByType(config.NodeTypeControlPlane)
			if err != nil {
				slog.Warn("storage prep skipped", "error", err)
				bootstrapErrors = append(bootstrapErrors, fmt.Errorf("resolve storage node pool: %w", err))
				break
			}

			nodeName := nodeNameForOperation(op, cfg.Environment)
			slog.Info("preparing raw disk prerequisites for OpenEBS Mayastor",
				"node", nodeName,
				"device", pool.Disks.Data,
			)
			if err := storagePrepareOpenEBSMayastorFn(ctx, storage.PrepareOpenEBSMayastorParams{
				Kubeconfig: kubeconfigPath,
				NodeName:   nodeName,
				Device:     pool.Disks.Data,
			}); err != nil {
				slog.Warn("openebs mayastor raw disk preparation failed", "error", err)
				bootstrapErrors = append(bootstrapErrors, fmt.Errorf("prepare openebs mayastor raw disk: %w", err))
				storagePrepBlockedFlux = true
			}
		default:
			bootstrapErrors = append(bootstrapErrors, fmt.Errorf("unsupported storage provider %q", strings.TrimSpace(cfg.Storage.Provider)))
			storagePrepBlockedFlux = true
		}
	}

	ociRepo := strings.TrimSpace(cfg.Flux.OCIRepo)
	if ociRepo == "" {
		slog.Warn("flux.ociRepo is empty; skipping GitOps OCI source configuration during bootstrap (can be configured manually later)")
	}

	var bootstrapSubstitute map[string]string
	if !storagePrepBlockedFlux && ociRepo != "" {
		bootstrapSubstitute, err = postBootstrapBootstrapSecretsFn(ctx, kubeconfigPath, cfg)
		if err != nil {
			slog.Warn("bootstrap secret preparation failed", "error", err)
			bootstrapErrors = append(bootstrapErrors, fmt.Errorf("prepare bootstrap secrets: %w", err))
		}
	}

	// Install FluxCD (and optionally configure OCI source).
	if !storagePrepBlockedFlux && len(bootstrapErrors) == 0 {
		fluxSubstitute := mergeBootstrapSubstituteMaps(
			fluxBootstrapSubstitute(op, cfg),
			bootstrapSubstitute,
		)
		if err := fluxBootstrapFn(ctx, flux.BootstrapParams{
			Kubeconfig: kubeconfigPath,
			OCIRepo:    ociRepo,
			Substitute: fluxSubstitute,
			Version:    cfg.Cluster.EffectiveFluxVersion(),
		}); err != nil {
			slog.Warn("flux bootstrap failed", "error", err)
			bootstrapErrors = append(bootstrapErrors, fmt.Errorf("bootstrap flux: %w", err))
		}
	}

	if len(bootstrapErrors) == 0 {
		ingressPublicIPv4, err := ingressPublicIPv4ForOperation(cfg, op)
		if err != nil {
			slog.Warn("ingress gateway target configuration skipped", "error", err)
			bootstrapErrors = append(bootstrapErrors, fmt.Errorf("resolve ingress public IPv4: %w", err))
		} else {
			op.SetContext(opContextIngressPublicIPv4, ingressPublicIPv4)
			if err := postBootstrapIngressGatewaySyncFn(ctx, kubeconfigPath, ingressPublicIPv4); err != nil {
				if errors.Is(err, errIngressGatewayNotFound) {
					slog.Warn("ingress gateway target configuration deferred; gateway not present yet", "error", err, "target_ipv4", ingressPublicIPv4)
				} else {
					slog.Warn("ingress gateway target configuration failed", "error", err, "target_ipv4", ingressPublicIPv4)
					bootstrapErrors = append(bootstrapErrors, fmt.Errorf("configure ingress gateway target: %w", err))
				}
			}
		}
	}

	if len(bootstrapErrors) > 0 {
		return fmt.Errorf("post-bootstrap component installation failed: %w", errors.Join(bootstrapErrors...))
	}

	return nil
}

func mergeBootstrapSubstituteMaps(values ...map[string]string) map[string]string {
	var merged map[string]string

	for _, value := range values {
		if len(value) == 0 {
			continue
		}
		if merged == nil {
			merged = make(map[string]string)
		}
		for key, item := range value {
			merged[key] = item
		}
	}

	return merged
}

func fluxBootstrapSubstitute(op *operation.Operation, cfg *config.Config) map[string]string {
	if cfg == nil {
		return nil
	}

	substitute := map[string]string{}
	if environment := strings.TrimSpace(cfg.Environment); environment != "" {
		substitute[fluxSubstituteEnvironment] = environment
	}

	if strings.TrimSpace(cfg.Storage.Provider) == config.StorageProviderOpenEBSMayastorLab {
		nodeName := ""
		if op != nil {
			nodeName = nodeNameForOperation(op, cfg.Environment)
		}

		substitute[fluxSubstituteStorageClass] = strings.TrimSpace(cfg.Storage.StorageClassName)
		substitute[fluxSubstituteStorageNode] = strings.TrimSpace(nodeName)
		substitute[fluxSubstituteDiskPoolDisk] = strings.TrimSpace(cfg.Storage.DiskPoolDiskByID)
	}

	if len(substitute) == 0 {
		return nil
	}

	return substitute
}

func waitForKubernetesAPIWithRetry(ctx context.Context, kubeconfigPath string) error {
	timeout := postBootstrapKubernetesAPIRetryTimeout
	if timeout <= 0 {
		timeout = time.Second
	}

	interval := postBootstrapKubernetesAPIRetryInterval
	if interval <= 0 {
		interval = time.Second
	}

	deadline := time.NewTimer(timeout)
	defer deadline.Stop()

	attempt := 0
	var lastErr error
	for {
		attempt++

		err := postBootstrapKubernetesAPIProbeFn(ctx, kubeconfigPath)
		if err == nil {
			if attempt > 1 {
				slog.Info("kubernetes API server became reachable", "attempt", attempt)
			}
			return nil
		}

		lastErr = err
		slog.Info("waiting for kubernetes API server readiness",
			"attempt", attempt,
			"interval", interval,
			"error", err,
		)

		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-deadline.C:
			if lastErr == nil {
				lastErr = fmt.Errorf("unknown kubernetes API connectivity error")
			}
			return fmt.Errorf("kubernetes API not ready after %s: %w", timeout, lastErr)
		case <-time.After(interval):
		}
	}
}

func postBootstrapKubernetesAPIReachable(ctx context.Context, kubeconfigPath string) error {
	cfg, err := clientcmd.BuildConfigFromFlags("", strings.TrimSpace(kubeconfigPath))
	if err != nil {
		return fmt.Errorf("load kube config: %w", err)
	}
	cfg.Timeout = 10 * time.Second

	discoveryClient, err := discovery.NewDiscoveryClientForConfig(cfg)
	if err != nil {
		return fmt.Errorf("create kubernetes discovery client: %w", err)
	}

	if _, err := discoveryClient.RESTClient().Get().AbsPath("/version").DoRaw(ctx); err != nil {
		return fmt.Errorf("query kubernetes API server version: %w", err)
	}

	return nil
}

func prepareBootstrapKubeconfigWithRetry(
	ctx context.Context,
	op *operation.Operation,
	cfg *config.Config,
) (string, func(), error) {
	timeout := postBootstrapKubeconfigRetryTimeout
	if timeout <= 0 {
		timeout = time.Second
	}

	interval := postBootstrapKubeconfigRetryInterval
	if interval <= 0 {
		interval = time.Second
	}

	deadline := time.NewTimer(timeout)
	defer deadline.Stop()

	attempt := 0
	var lastErr error
	for {
		attempt++
		maybeRefreshOperationServerIPs(ctx, op, cfg)

		kubeconfigPath, cleanup, err := postBootstrapKubeconfigPathFn(ctx, op, cfg)
		if err == nil {
			return kubeconfigPath, cleanup, nil
		}

		lastErr = err
		slog.Info("waiting to retry bootstrap kubeconfig preparation",
			"attempt", attempt,
			"interval", interval,
			"error", err,
		)

		select {
		case <-ctx.Done():
			return "", nil, ctx.Err()
		case <-deadline.C:
			if lastErr == nil {
				lastErr = fmt.Errorf("unknown talos connectivity error")
			}
			return "", nil, fmt.Errorf("bootstrap kubeconfig not ready after %s: %w", timeout, lastErr)
		case <-time.After(interval):
		}
	}
}

func postBootstrapKubeconfigPath(ctx context.Context, op *operation.Operation, cfg *config.Config) (string, func(), error) {
	publicIP := strings.TrimSpace(op.GetContextString("publicIP"))
	if publicIP == "" {
		return "", nil, fmt.Errorf("no public IP in operation context")
	}

	endpoint := strings.TrimSpace(op.GetContextString("controlPlaneEndpoint"))
	if endpoint == "" {
		endpoint = controlPlaneEndpoint(op.GetContextString("privateIP"), publicIP)
	}

	store, err := newSecretStore(ctx, cfg)
	if err != nil {
		return "", nil, err
	}
	assets, err := ensureTalosAssets(ctx, cfg, endpoint, store)
	if err != nil {
		return "", nil, err
	}

	candidates := []string{publicIP, strings.TrimSpace(op.GetContextString("privateIP"))}
	seen := map[string]struct{}{}
	var (
		kubeconfig       []byte
		selectedEndpoint string
		lastErr          error
	)

	for _, candidate := range candidates {
		candidate = strings.TrimSpace(candidate)
		if candidate == "" {
			continue
		}
		if _, ok := seen[candidate]; ok {
			continue
		}
		seen[candidate] = struct{}{}

		talosClient, err := talos.NewClient(candidate, assets.Talosconfig)
		if err != nil {
			lastErr = fmt.Errorf("create talos client via %s: %w", candidate, err)
			continue
		}

		kubeconfig, err = talosClient.Kubeconfig(ctx)
		closeErr := talosClient.Close()
		if err == nil && closeErr == nil {
			selectedEndpoint = candidate
			break
		}
		if err != nil {
			lastErr = fmt.Errorf("fetch kubeconfig via %s: %w", candidate, err)
		} else {
			lastErr = fmt.Errorf("close talos client via %s: %w", candidate, closeErr)
		}
	}

	if selectedEndpoint == "" {
		if lastErr == nil {
			lastErr = fmt.Errorf("unknown talos connectivity error")
		}
		return "", nil, lastErr
	}

	rewrittenKubeconfig, err := rewriteKubeconfigServerIfNeeded(kubeconfig, selectedEndpoint)
	if err != nil {
		return "", nil, fmt.Errorf("rewrite kubeconfig server: %w", err)
	}

	tempFile, err := os.CreateTemp("", "waddle-cloud-kubeconfig-*.yaml")
	if err != nil {
		return "", nil, fmt.Errorf("create temporary kubeconfig: %w", err)
	}

	cleanup := func() {
		if removeErr := os.Remove(tempFile.Name()); removeErr != nil && !errors.Is(removeErr, os.ErrNotExist) {
			slog.Warn("failed to remove temporary bootstrap kubeconfig", "path", tempFile.Name(), "error", removeErr)
		}
	}

	if _, err := tempFile.Write(rewrittenKubeconfig); err != nil {
		_ = tempFile.Close()
		cleanup()
		return "", nil, fmt.Errorf("write temporary kubeconfig: %w", err)
	}
	if err := tempFile.Close(); err != nil {
		cleanup()
		return "", nil, fmt.Errorf("close temporary kubeconfig: %w", err)
	}
	if err := os.Chmod(tempFile.Name(), 0o600); err != nil {
		cleanup()
		return "", nil, fmt.Errorf("chmod temporary kubeconfig: %w", err)
	}

	return tempFile.Name(), cleanup, nil
}

func maybeRefreshOperationServerIPs(ctx context.Context, op *operation.Operation, cfg *config.Config) {
	if op == nil || cfg == nil {
		return
	}

	serverID := strings.TrimSpace(op.GetContextString("serverId"))
	zoneValue := strings.TrimSpace(op.GetContextString("zone"))
	if serverID == "" || zoneValue == "" {
		return
	}

	zone := scw.Zone(zoneValue)
	scwAccessKey, scwSecretKey := cfg.ScalewayCredentials()
	scwClient, err := scalewayNewClientFn(
		scwAccessKey,
		scwSecretKey,
		cfg.Scaleway.ProjectID,
		cfg.Scaleway.OrganizationID,
	)
	if err != nil {
		slog.Debug("skipping server IP refresh (create scaleway client failed)", "error", err)
		return
	}

	server, err := scalewayGetServerFn(ctx, scwClient, zone, serverID)
	if err != nil {
		slog.Debug("skipping server IP refresh (load server failed)", "server_id", serverID, "zone", zoneValue, "error", err)
		return
	}

	publicIP, privateIP := scaleway.ExtractServerIPs(server)
	currentPublic := strings.TrimSpace(op.GetContextString("publicIP"))
	currentPrivate := strings.TrimSpace(op.GetContextString("privateIP"))

	updated := false
	if publicIP != "" && publicIP != currentPublic {
		op.SetContext("publicIP", publicIP)
		updated = true
	}
	if privateIP != "" && privateIP != currentPrivate {
		op.SetContext("privateIP", privateIP)
		updated = true
	}
	if !updated {
		return
	}

	slog.Info("refreshed control-plane IPs from scaleway",
		"server_id", serverID,
		"public_ip", op.GetContextString("publicIP"),
		"private_ip", op.GetContextString("privateIP"),
	)
}

func phaseVerify(ctx context.Context, op *operation.Operation, cfg *config.Config) error {
	_ = ctx
	slog.Info("phase verify: running health checks")

	nodeName := op.GetContextString("nodeName")
	if nodeName == "" {
		poolName := strings.TrimSpace(op.GetContextString("poolName"))
		if poolName == "" {
			poolName = "control-plane"
		}
		nodeName = controlPlaneNodeName(cfg.Environment, poolName, 1)
	}

	fmt.Printf("\nCluster %q created successfully!\n", cfg.Environment)

	return nil
}

func phaseRestrictTalosAPI(ctx context.Context, op *operation.Operation, cfg *config.Config) error {
	publicIP := strings.TrimSpace(op.GetContextString("publicIP"))
	if publicIP == "" {
		return fmt.Errorf("no public IP in operation context")
	}

	endpoint := strings.TrimSpace(op.GetContextString("controlPlaneEndpoint"))
	if endpoint == "" {
		endpoint = controlPlaneEndpoint(op.GetContextString("privateIP"), publicIP)
	}

	store, err := newSecretStore(ctx, cfg)
	if err != nil {
		return err
	}

	netbirdSecretPath := strings.TrimSpace(op.GetContextString(opContextNetbirdSecretPath))
	netbirdSecretKey := strings.TrimSpace(op.GetContextString(opContextNetbirdSecretKey))
	netbirdLookupPath, netbirdKeyCandidates := netbirdSetupKeyLookupTargets(cfg, netbirdSecretPath, netbirdSecretKey)
	netbirdSetupKey, err := loadOptionalNetbirdSetupKeyFromSecretStoreWithOverrides(ctx, cfg, store, netbirdSecretPath, netbirdSecretKey)
	if err != nil {
		return fmt.Errorf("load netbird setup key: %w", err)
	}
	if netbirdSetupKey == "" {
		slog.Info(
			"phase restrict-talos-api: skipping (Netbird setup key not found in secret backend)",
			"secret_path",
			netbirdLookupPath,
			"secret_key_candidates",
			netbirdKeyCandidates,
		)
		return nil
	}

	assets, err := ensureTalosAssets(ctx, cfg, endpoint, store)
	if err != nil {
		return err
	}

	nodeName := nodeNameForOperation(op, cfg.Environment)
	nodeConfig, err := renderNodeTalosConfig(assets.ControlPlane, nodeName)
	if err != nil {
		return fmt.Errorf("render node-specific Talos config for %q: %w", nodeName, err)
	}
	nodeConfig, err = appendNetbirdExtensionServiceConfig(nodeConfig, netbirdSetupKey)
	if err != nil {
		return fmt.Errorf("append netbird extension service config: %w", err)
	}

	allowedSubnets := talosAPIAllowedSubnets()
	nodeConfig, err = appendTalosAPIIngressRestriction(nodeConfig, allowedSubnets)
	if err != nil {
		return fmt.Errorf("append Talos API ingress restriction: %w", err)
	}

	talosClient, err := talos.NewClient(publicIP, assets.Talosconfig)
	if err != nil {
		return fmt.Errorf("create talos client: %w", err)
	}
	defer talosClient.Close()

	if err := talosClient.WaitForServiceRunning(ctx, "ext-netbird", 10*time.Minute); err != nil {
		return fmt.Errorf("wait for netbird service: %w", err)
	}

	if err := talosClient.ApplyConfig(ctx, nodeConfig); err != nil {
		return fmt.Errorf("apply Talos API ingress restriction: %w", err)
	}

	op.SetContext("talosAPIRestricted", true)
	slog.Info("phase restrict-talos-api: applied ingress restriction for Talos API", "allowed_subnets", allowedSubnets)

	return nil
}

func selectCreatePool(cfg *config.Config, poolName string) (*config.NodePoolConfig, error) {
	if strings.TrimSpace(poolName) != "" {
		pool, err := cfg.FindNodePool(poolName)
		if err != nil {
			return nil, fmt.Errorf("resolve --pool %q: %w", poolName, err)
		}
		if pool.EffectiveType() == "" {
			return nil, fmt.Errorf("pool %q has invalid type %q", pool.Name, pool.Type)
		}
		return pool, nil
	}

	pool, err := cfg.FirstNodePoolByType(config.NodeTypeControlPlane)
	if err != nil {
		return nil, fmt.Errorf("select default control-plane pool: %w", err)
	}
	return pool, nil
}

func poolForOperation(cfg *config.Config, op *operation.Operation) (*config.NodePoolConfig, error) {
	poolName := op.GetContextString("poolName")
	if strings.TrimSpace(poolName) != "" {
		return cfg.FindNodePool(poolName)
	}

	pool, err := cfg.FirstNodePoolByType(config.NodeTypeControlPlane)
	if err != nil {
		return nil, err
	}
	op.SetContext("poolName", pool.Name)
	return pool, nil
}

func nodeNameForOperation(op *operation.Operation, environment string) string {
	if nodeName := op.GetContextString("nodeName"); strings.TrimSpace(nodeName) != "" {
		return nodeName
	}
	poolName := strings.TrimSpace(op.GetContextString("poolName"))
	if poolName == "" {
		poolName = "control-plane"
	}
	return controlPlaneNodeName(environment, poolName, 1)
}

func controlPlaneEndpoint(privateIP, publicIP string) string {
	if ip := strings.TrimSpace(privateIP); ip != "" {
		return ip
	}
	return strings.TrimSpace(publicIP)
}

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
