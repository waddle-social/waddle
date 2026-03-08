package scaleway

import (
	"context"
	"net"
	"strings"
	"testing"

	baremetal "github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
	scw "github.com/scaleway/scaleway-sdk-go/scw"
)

func restoreInventoryTestFns() {
	baremetalZonesFn = func(api *baremetal.API) []scw.Zone {
		return api.Zones()
	}
	baremetalListServersFn = func(ctx context.Context, api *baremetal.API, req *baremetal.ListServersRequest) (*baremetal.ListServersResponse, error) {
		return api.ListServers(req, scw.WithAllPages(), scw.WithContext(ctx))
	}
}

func TestListBareMetalServerInventoryListsAcrossZonesAndSorts(t *testing.T) {
	restoreInventoryTestFns()
	t.Cleanup(restoreInventoryTestFns)

	baremetalZonesFn = func(*baremetal.API) []scw.Zone {
		return []scw.Zone{scw.ZoneNlAms1, scw.ZoneFrPar2}
	}

	var requests []*baremetal.ListServersRequest
	baremetalListServersFn = func(ctx context.Context, api *baremetal.API, req *baremetal.ListServersRequest) (*baremetal.ListServersResponse, error) {
		requests = append(requests, req)

		switch req.Zone {
		case scw.ZoneNlAms1:
			return &baremetal.ListServersResponse{
				Servers: []*baremetal.Server{
					{
						ID:        "srv-b",
						Name:      "bravo",
						OfferID:   "offer-bravo",
						OfferName: "",
						Zone:      scw.ZoneNlAms1,
						IPs: []*baremetal.IP{
							{Version: baremetal.IPVersionIPv4, Address: net.ParseIP("10.0.0.10")},
						},
					},
				},
			}, nil
		case scw.ZoneFrPar2:
			return &baremetal.ListServersResponse{
				Servers: []*baremetal.Server{
					nil,
					{
						ID:        "srv-a",
						Name:      "alpha",
						OfferID:   "offer-alpha",
						OfferName: "EM-A610R-NVMe",
						Zone:      scw.ZoneFrPar2,
						IPs: []*baremetal.IP{
							{Version: baremetal.IPVersionIPv4, Address: net.ParseIP("203.0.113.10")},
							{Version: baremetal.IPVersionIPv4, Address: net.ParseIP("172.16.16.10")},
						},
					},
					{
						ID:        "",
						Name:      "skip-empty-id",
						OfferName: "ignored",
						Zone:      scw.ZoneFrPar2,
					},
				},
			}, nil
		default:
			return &baremetal.ListServersResponse{}, nil
		}
	}

	items, err := ListBareMetalServerInventory(context.Background(), &Client{Baremetal: &baremetal.API{}}, "org-123", "proj-456")
	if err != nil {
		t.Fatalf("ListBareMetalServerInventory returned error: %v", err)
	}

	if len(requests) != 2 {
		t.Fatalf("expected 2 list requests, got %d", len(requests))
	}
	for _, req := range requests {
		if req.OrganizationID == nil || *req.OrganizationID != "org-123" {
			t.Fatalf("organization ID request filter = %v, want org-123", req.OrganizationID)
		}
		if req.ProjectID == nil || *req.ProjectID != "proj-456" {
			t.Fatalf("project ID request filter = %v, want proj-456", req.ProjectID)
		}
	}

	if len(items) != 2 {
		t.Fatalf("expected 2 inventory items, got %d", len(items))
	}

	if items[0].Zone != "fr-par-2" || items[0].Name != "alpha" || items[0].Type != "EM-A610R-NVMe" || items[0].IPAddress != "203.0.113.10" {
		t.Fatalf("unexpected first item: %+v", items[0])
	}
	if items[1].Zone != "nl-ams-1" || items[1].Name != "bravo" || items[1].Type != "offer-bravo" || items[1].IPAddress != "10.0.0.10" {
		t.Fatalf("unexpected second item: %+v", items[1])
	}
}

func TestListBareMetalServerInventoryRequiresScope(t *testing.T) {
	_, err := ListBareMetalServerInventory(context.Background(), &Client{Baremetal: &baremetal.API{}}, "", "proj-456")
	if err == nil || !strings.Contains(err.Error(), "organization ID is required") {
		t.Fatalf("expected organization ID validation error, got %v", err)
	}

	_, err = ListBareMetalServerInventory(context.Background(), &Client{Baremetal: &baremetal.API{}}, "org-123", "")
	if err == nil || !strings.Contains(err.Error(), "project ID is required") {
		t.Fatalf("expected project ID validation error, got %v", err)
	}
}

func TestServerDisplayIPPrefersPublicIPv4(t *testing.T) {
	server := &baremetal.Server{
		IPs: []*baremetal.IP{
			{Version: baremetal.IPVersionIPv4, Address: net.ParseIP("172.16.16.10")},
			{Version: baremetal.IPVersionIPv4, Address: net.ParseIP("203.0.113.10")},
		},
	}

	publicIP, privateIP := ExtractServerIPs(server)
	if publicIP != "203.0.113.10" || privateIP != "172.16.16.10" {
		t.Fatalf("ExtractServerIPs() = (%q, %q), want (%q, %q)", publicIP, privateIP, "203.0.113.10", "172.16.16.10")
	}

	if got := ServerDisplayIP(server); got != "203.0.113.10" {
		t.Fatalf("ServerDisplayIP() = %q, want %q", got, "203.0.113.10")
	}
}

func TestServerDisplayIPFallsBackToPrivateIP(t *testing.T) {
	server := &baremetal.Server{
		IPs: []*baremetal.IP{
			{Version: baremetal.IPVersionIPv4, Address: net.ParseIP("10.0.0.10")},
		},
	}

	if got := ServerDisplayIP(server); got != "10.0.0.10" {
		t.Fatalf("ServerDisplayIP() = %q, want %q", got, "10.0.0.10")
	}
}
