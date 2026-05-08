package talos

import (
	"context"
	"fmt"
	"log/slog"
	"net"
	"strings"
	"time"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/emptypb"
)

var (
	waitForMaintenancePollInterval = 15 * time.Second
	waitForMaintenanceProbeTimeout = 5 * time.Second
	waitForMaintenanceProbeFn      = probeTalosMaintenance
)

// MaintenanceTimeoutError is returned when a node never reaches Talos
// maintenance mode within the allotted timeout.
type MaintenanceTimeoutError struct {
	Target  string
	Timeout time.Duration
}

func (e *MaintenanceTimeoutError) Error() string {
	return fmt.Sprintf("talos maintenance mode not reachable at %s within %s", e.Target, e.Timeout)
}

func probeTalosMaintenance(ctx context.Context, endpoint string) error {
	client, err := NewInsecureClient(endpoint)
	if err != nil {
		return err
	}
	defer client.Close()

	if _, err := client.machine.Version(ctx, &emptypb.Empty{}); err != nil {
		if isMaintenanceModeVersionUnimplementedError(err) {
			return nil
		}
		return fmt.Errorf("query talos version: %w", err)
	}

	return nil
}

func isMaintenanceModeVersionUnimplementedError(err error) bool {
	if err == nil {
		return false
	}
	if status.Code(err) != codes.Unimplemented {
		return false
	}

	return strings.Contains(strings.ToLower(err.Error()), "maintenance mode")
}

// WaitForMaintenance polls until the Talos API is reachable in maintenance mode.
func WaitForMaintenance(ctx context.Context, ip string, timeout time.Duration) error {
	target := net.JoinHostPort(ip, talosAPIDefaultPort)
	deadline := time.NewTimer(timeout)
	defer deadline.Stop()
	ticker := time.NewTicker(waitForMaintenancePollInterval)
	defer ticker.Stop()

	slog.Info("waiting for talos maintenance mode", "target", target)

	attempt := 0
	for {
		attempt++
		probeCtx, cancel := context.WithTimeout(ctx, waitForMaintenanceProbeTimeout)
		err := waitForMaintenanceProbeFn(probeCtx, ip)
		cancel()
		if err == nil {
			slog.Info("talos maintenance mode reachable", "target", target)
			return nil
		}

		slog.Debug("talos not yet reachable", "target", target, "attempt", attempt, "error", err)

		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-deadline.C:
			return &MaintenanceTimeoutError{
				Target:  target,
				Timeout: timeout,
			}
		case <-ticker.C:
		}
	}
}
