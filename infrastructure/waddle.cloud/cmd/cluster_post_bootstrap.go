package cmd

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"strings"

	scw "github.com/scaleway/scaleway-sdk-go/scw"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/cilium"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/flux"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/operation"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/scaleway"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/storage"
)

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
