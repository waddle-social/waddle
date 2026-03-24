package scaleway

import (
	"context"
	"fmt"
	"sort"
	"strings"

	baremetal "github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
	scw "github.com/scaleway/scaleway-sdk-go/scw"
)

// BareMetalServerInventoryItem is a provider-neutral summary of a Scaleway bare metal server.
type BareMetalServerInventoryItem struct {
	Name      string `json:"name"`
	Type      string `json:"type"`
	ServerID  string `json:"serverId"`
	Zone      string `json:"zone"`
	IPAddress string `json:"ipAddress,omitempty"`
}

var baremetalZonesFn = func(api *baremetal.API) []scw.Zone {
	return api.Zones()
}

var baremetalListServersFn = func(ctx context.Context, api *baremetal.API, req *baremetal.ListServersRequest) (*baremetal.ListServersResponse, error) {
	return api.ListServers(req, scw.WithAllPages(), scw.WithContext(ctx))
}

// ListBareMetalServerInventory lists every bare metal server in the configured organization/project scope.
func ListBareMetalServerInventory(ctx context.Context, client *Client, organizationID, projectID string) ([]BareMetalServerInventoryItem, error) {
	if client == nil || client.Baremetal == nil {
		return nil, fmt.Errorf("scaleway bare metal client is required")
	}

	organizationID = strings.TrimSpace(organizationID)
	projectID = strings.TrimSpace(projectID)
	if organizationID == "" {
		return nil, fmt.Errorf("organization ID is required")
	}
	if projectID == "" {
		return nil, fmt.Errorf("project ID is required")
	}

	seen := make(map[string]struct{})
	items := make([]BareMetalServerInventoryItem, 0)
	for _, zone := range baremetalZonesFn(client.Baremetal) {
		req := &baremetal.ListServersRequest{
			Zone:           zone,
			OrganizationID: &organizationID,
			ProjectID:      &projectID,
		}

		resp, err := baremetalListServersFn(ctx, client.Baremetal, req)
		if err != nil {
			return nil, fmt.Errorf("list bare metal servers in zone %s: %w", zone, err)
		}

		for _, server := range resp.Servers {
			if server == nil {
				continue
			}

			serverID := strings.TrimSpace(server.ID)
			if serverID == "" {
				continue
			}
			if _, ok := seen[serverID]; ok {
				continue
			}
			seen[serverID] = struct{}{}

			items = append(items, BareMetalServerInventoryItem{
				Name:      strings.TrimSpace(server.Name),
				Type:      inventoryServerType(server),
				ServerID:  serverID,
				Zone:      inventoryServerZone(server, zone),
				IPAddress: ServerDisplayIP(server),
			})
		}
	}

	sort.Slice(items, func(i, j int) bool {
		if items[i].Zone != items[j].Zone {
			return items[i].Zone < items[j].Zone
		}
		if items[i].Name != items[j].Name {
			return items[i].Name < items[j].Name
		}
		return items[i].ServerID < items[j].ServerID
	})

	return items, nil
}

func inventoryServerType(server *baremetal.Server) string {
	if server == nil {
		return ""
	}
	if offerName := strings.TrimSpace(server.OfferName); offerName != "" {
		return offerName
	}
	return strings.TrimSpace(server.OfferID)
}

func inventoryServerZone(server *baremetal.Server, fallback scw.Zone) string {
	if server != nil {
		if zone := strings.TrimSpace(server.Zone.String()); zone != "" {
			return zone
		}
	}
	return strings.TrimSpace(fallback.String())
}
