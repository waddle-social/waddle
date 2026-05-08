package cmd

import (
	"context"
	"fmt"
	"log/slog"
	"strings"
	"sync"
	"time"

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
