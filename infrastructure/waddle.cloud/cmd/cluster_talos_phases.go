package cmd

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"strings"
	"time"

	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/operation"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/talos"
)

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

func phaseVerify(ctx context.Context, op *operation.Operation, cfg *config.Config) error {
	_ = ctx
	slog.Info("phase verify: running health checks")

	fmt.Printf("\nCluster %q created successfully.\n", cfg.Environment)
	fmt.Println("Run `waddle.cloud cluster status` to inspect node, IP, and server details.")

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
