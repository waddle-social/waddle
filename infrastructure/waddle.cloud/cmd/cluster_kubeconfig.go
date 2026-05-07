package cmd

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"strings"
	"time"

	scw "github.com/scaleway/scaleway-sdk-go/scw"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/operation"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/scaleway"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/talos"
	"k8s.io/client-go/discovery"
	"k8s.io/client-go/tools/clientcmd"
)

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
