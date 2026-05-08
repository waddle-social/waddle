package scaleway

import (
	"context"
	"fmt"
	"log/slog"
	"time"

	baremetal "github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
	"github.com/scaleway/scaleway-sdk-go/scw"
)

func WaitForReady(ctx context.Context, client *Client, serverID string, zone scw.Zone) (*baremetal.Server, error) {
	ticker := time.NewTicker(30 * time.Second)
	defer ticker.Stop()

	timeout := time.After(45 * time.Minute)

	for {
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-timeout:
			return nil, fmt.Errorf("server %s did not become ready within 45 minutes", serverID)
		case <-ticker.C:
			server, err := client.Baremetal.GetServer(&baremetal.GetServerRequest{
				Zone:     zone,
				ServerID: serverID,
			})
			if err != nil {
				slog.Warn("poll failed, retrying", "error", err)
				continue
			}

			slog.Info("server status", "status", server.Status, "server_id", serverID)

			switch server.Status {
			case baremetal.ServerStatusReady:
				return server, nil
			case baremetal.ServerStatusError:
				return nil, fmt.Errorf("server entered error state: %s", serverID)
			case baremetal.ServerStatusLocked:
				return nil, fmt.Errorf("server is locked (billing issue?): %s", serverID)
			default:
				continue
			}
		}
	}
}
