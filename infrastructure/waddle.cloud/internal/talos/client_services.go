package talos

import (
	"context"
	"fmt"
	"strings"
	"time"

	"google.golang.org/protobuf/types/known/emptypb"
)

func (c *Client) HealthCheck(ctx context.Context) error {
	if c.machine == nil {
		return fmt.Errorf("talos client is not initialized")
	}

	timeout := time.NewTimer(5 * time.Minute)
	defer timeout.Stop()

	ticker := time.NewTicker(5 * time.Second)
	defer ticker.Stop()

	var lastErr error

	for {
		if _, err := c.machine.ServiceList(ctx, &emptypb.Empty{}); err == nil {
			return nil
		} else {
			lastErr = err
		}

		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-timeout.C:
			if lastErr == nil {
				lastErr = fmt.Errorf("node health check timed out")
			}
			return fmt.Errorf("health check failed: %w", lastErr)
		case <-ticker.C:
		}
	}
}

// WaitForServiceRunning waits until a Talos service reaches running state.
func (c *Client) WaitForServiceRunning(ctx context.Context, serviceID string, timeout time.Duration) error {
	if c.machine == nil {
		return fmt.Errorf("talos client is not initialized")
	}

	serviceID = strings.TrimSpace(serviceID)
	if serviceID == "" {
		return fmt.Errorf("service ID is required")
	}
	if timeout <= 0 {
		timeout = 5 * time.Minute
	}

	deadline := time.NewTimer(timeout)
	defer deadline.Stop()
	ticker := time.NewTicker(5 * time.Second)
	defer ticker.Stop()

	var lastState string
	for {
		response, err := c.machine.ServiceList(ctx, &emptypb.Empty{})
		if err == nil {
			found := false
			for _, message := range response.GetMessages() {
				for _, service := range message.GetServices() {
					if strings.TrimSpace(service.GetId()) != serviceID {
						continue
					}

					found = true
					lastState = strings.TrimSpace(service.GetState())
					if strings.EqualFold(lastState, "running") {
						return nil
					}
				}
			}
			if !found {
				lastState = "not found"
			}
		}

		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-deadline.C:
			if lastState == "" {
				lastState = "unknown"
			}
			return fmt.Errorf("service %q not running after %s (last state: %s)", serviceID, timeout, lastState)
		case <-ticker.C:
		}
	}
}

// Upgrade triggers a Talos OS upgrade on the node.
