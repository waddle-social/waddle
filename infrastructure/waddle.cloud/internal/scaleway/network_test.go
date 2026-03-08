package scaleway

import (
	"context"
	"errors"
	"net"
	"strings"
	"testing"

	baremetalv3 "github.com/scaleway/scaleway-sdk-go/api/baremetal/v3"
	ipam "github.com/scaleway/scaleway-sdk-go/api/ipam/v1"
	vpc "github.com/scaleway/scaleway-sdk-go/api/vpc/v2"
	"github.com/scaleway/scaleway-sdk-go/scw"
)

func restoreNetworkTestFns() {
	ensureVPCFn = ensureVPC
	ensurePrivateNetworkFn = ensurePrivateNetwork
	listPrivateNetworksByName = func(ctx context.Context, client *Client, req *vpc.ListPrivateNetworksRequest) ([]*vpc.PrivateNetwork, error) {
		resp, err := client.VPC.ListPrivateNetworks(req, scw.WithAllPages(), scw.WithContext(ctx))
		if err != nil {
			return nil, err
		}
		return resp.PrivateNetworks, nil
	}
	createPrivateNetworkResource = func(ctx context.Context, client *Client, req *vpc.CreatePrivateNetworkRequest) (*vpc.PrivateNetwork, error) {
		return client.VPC.CreatePrivateNetwork(req, scw.WithContext(ctx))
	}
	updatePrivateNetworkResource = func(ctx context.Context, client *Client, req *vpc.UpdatePrivateNetworkRequest) (*vpc.PrivateNetwork, error) {
		return client.VPC.UpdatePrivateNetwork(req, scw.WithContext(ctx))
	}
	enablePrivateNetworkDHCP = func(ctx context.Context, client *Client, req *vpc.EnableDHCPRequest) (*vpc.PrivateNetwork, error) {
		return client.VPC.EnableDHCP(req, scw.WithContext(ctx))
	}
	deletePrivateNetworkResource = func(ctx context.Context, client *Client, req *vpc.DeletePrivateNetworkRequest) error {
		return client.VPC.DeletePrivateNetwork(req, scw.WithContext(ctx))
	}
	deleteServerPrivateNetworkAttachment = func(ctx context.Context, client *Client, zone scw.Zone, serverID, privateNetworkID string) error {
		return client.BaremetalPrivateNetworkV3.DeleteServerPrivateNetwork(&baremetalv3.PrivateNetworkAPIDeleteServerPrivateNetworkRequest{
			Zone:             zone,
			ServerID:         serverID,
			PrivateNetworkID: privateNetworkID,
		}, scw.WithContext(ctx))
	}
	getPrivateNetworkForCIDRLookup = func(ctx context.Context, client *Client, region scw.Region, privateNetworkID string) (*vpc.PrivateNetwork, error) {
		return client.VPC.GetPrivateNetwork(&vpc.GetPrivateNetworkRequest{
			Region:           region,
			PrivateNetworkID: privateNetworkID,
		}, scw.WithContext(ctx))
	}
	listPrivateNetworkIPAMIPs = func(ctx context.Context, client *Client, region scw.Region, privateNetworkID string) ([]*ipam.IP, error) {
		resp, err := client.IPAM.ListIPs(&ipam.ListIPsRequest{
			Region:           region,
			PrivateNetworkID: &privateNetworkID,
		}, scw.WithAllPages(), scw.WithContext(ctx))
		if err != nil {
			return nil, err
		}
		return resp.IPs, nil
	}
}

func TestEnsureNetworkFoundationCreatesPrivateNetworkWithConfiguredCIDR(t *testing.T) {
	restoreNetworkTestFns()
	t.Cleanup(restoreNetworkTestFns)

	ensureVPCFn = func(context.Context, *Client, scw.Region, string, string, string, []string) (*vpc.VPC, error) {
		return &vpc.VPC{ID: "vpc-123", Name: "waddle-cloud"}, nil
	}
	listPrivateNetworksByName = func(context.Context, *Client, *vpc.ListPrivateNetworksRequest) ([]*vpc.PrivateNetwork, error) {
		return nil, nil
	}

	var gotCreateCIDR string
	createPrivateNetworkResource = func(_ context.Context, _ *Client, req *vpc.CreatePrivateNetworkRequest) (*vpc.PrivateNetwork, error) {
		if len(req.Subnets) != 1 {
			t.Fatalf("create private network subnets len = %d, want 1", len(req.Subnets))
		}
		gotCreateCIDR = req.Subnets[0].String()
		return &vpc.PrivateNetwork{
			ID:                             "pn-new",
			Name:                           req.Name,
			VpcID:                          "vpc-123",
			DHCPEnabled:                    true,
			DefaultRoutePropagationEnabled: true,
			Subnets:                        []*vpc.Subnet{{Subnet: req.Subnets[0]}},
		}, nil
	}

	network, err := EnsureNetworkFoundation(context.Background(), &Client{}, NetworkFoundationParams{
		Region:                 scw.RegionFrPar,
		ProjectID:              "project-id",
		VPCName:                "waddle-cloud",
		PrivateNetworkName:     "waddle-cloud-private",
		PrivateNetworkIPv4CIDR: "172.16.16.0/24",
	})
	if err != nil {
		t.Fatalf("EnsureNetworkFoundation returned error: %v", err)
	}
	if gotCreateCIDR != "172.16.16.0/24" {
		t.Fatalf("create private network cidr = %q, want %q", gotCreateCIDR, "172.16.16.0/24")
	}
	if network.PrivateNetworkID != "pn-new" {
		t.Fatalf("private network id = %q, want %q", network.PrivateNetworkID, "pn-new")
	}
}

func TestEnsureNetworkFoundationReusesConformingPrivateNetwork(t *testing.T) {
	restoreNetworkTestFns()
	t.Cleanup(restoreNetworkTestFns)

	ensureVPCFn = func(context.Context, *Client, scw.Region, string, string, string, []string) (*vpc.VPC, error) {
		return &vpc.VPC{ID: "vpc-123", Name: "waddle-cloud"}, nil
	}
	listPrivateNetworksByName = func(context.Context, *Client, *vpc.ListPrivateNetworksRequest) ([]*vpc.PrivateNetwork, error) {
		return []*vpc.PrivateNetwork{
			{
				ID:                             "pn-existing",
				Name:                           "waddle-cloud-private",
				VpcID:                          "vpc-123",
				DHCPEnabled:                    true,
				DefaultRoutePropagationEnabled: true,
				Subnets:                        []*vpc.Subnet{{Subnet: mustNetworkIPNet(t, "172.16.16.0/24")}},
			},
		}, nil
	}
	createPrivateNetworkResource = func(context.Context, *Client, *vpc.CreatePrivateNetworkRequest) (*vpc.PrivateNetwork, error) {
		t.Fatal("create private network should not run for conforming network")
		return nil, nil
	}

	network, err := EnsureNetworkFoundation(context.Background(), &Client{}, NetworkFoundationParams{
		Region:                 scw.RegionFrPar,
		ProjectID:              "project-id",
		VPCName:                "waddle-cloud",
		PrivateNetworkName:     "waddle-cloud-private",
		PrivateNetworkIPv4CIDR: "172.16.16.0/24",
	})
	if err != nil {
		t.Fatalf("EnsureNetworkFoundation returned error: %v", err)
	}
	if network.PrivateNetworkID != "pn-existing" {
		t.Fatalf("private network id = %q, want %q", network.PrivateNetworkID, "pn-existing")
	}
}

func TestEnsureNetworkFoundationFailsOnNonConformingNetworkWithoutReplacement(t *testing.T) {
	restoreNetworkTestFns()
	t.Cleanup(restoreNetworkTestFns)

	ensureVPCFn = func(context.Context, *Client, scw.Region, string, string, string, []string) (*vpc.VPC, error) {
		return &vpc.VPC{ID: "vpc-123", Name: "waddle-cloud"}, nil
	}
	listPrivateNetworksByName = func(context.Context, *Client, *vpc.ListPrivateNetworksRequest) ([]*vpc.PrivateNetwork, error) {
		return []*vpc.PrivateNetwork{
			{
				ID:      "pn-existing",
				Name:    "waddle-cloud-private",
				VpcID:   "vpc-123",
				Subnets: []*vpc.Subnet{{Subnet: mustNetworkIPNet(t, "172.16.4.0/22")}},
			},
		}, nil
	}

	_, err := EnsureNetworkFoundation(context.Background(), &Client{}, NetworkFoundationParams{
		Region:                 scw.RegionFrPar,
		ProjectID:              "project-id",
		VPCName:                "waddle-cloud",
		PrivateNetworkName:     "waddle-cloud-private",
		PrivateNetworkIPv4CIDR: "172.16.16.0/24",
	})
	if err == nil {
		t.Fatal("expected non-conforming private network error, got nil")
	}
	if !strings.Contains(err.Error(), "do not match desired CIDR 172.16.16.0/24") {
		t.Fatalf("expected cidr mismatch error, got %q", err)
	}
}

func TestEnsureNetworkFoundationReplacesNonConformingNetworkDuringReinstall(t *testing.T) {
	restoreNetworkTestFns()
	t.Cleanup(restoreNetworkTestFns)

	ensureVPCFn = func(context.Context, *Client, scw.Region, string, string, string, []string) (*vpc.VPC, error) {
		return &vpc.VPC{ID: "vpc-123", Name: "waddle-cloud"}, nil
	}
	listPrivateNetworksByName = func(context.Context, *Client, *vpc.ListPrivateNetworksRequest) ([]*vpc.PrivateNetwork, error) {
		return []*vpc.PrivateNetwork{
			{
				ID:      "pn-old",
				Name:    "waddle-cloud-private",
				VpcID:   "vpc-123",
				Tags:    []string{"waddle-cloud:managed"},
				Subnets: []*vpc.Subnet{{Subnet: mustNetworkIPNet(t, "172.16.4.0/22")}},
			},
		}, nil
	}
	listServerPrivateNetworkAttachments = func(context.Context, *Client, scw.Zone, string) ([]*baremetalv3.ServerPrivateNetwork, error) {
		return []*baremetalv3.ServerPrivateNetwork{
			{ServerID: "srv-123", PrivateNetworkID: "pn-old"},
		}, nil
	}

	var detached bool
	deleteServerPrivateNetworkAttachment = func(_ context.Context, _ *Client, zone scw.Zone, serverID, privateNetworkID string) error {
		detached = true
		if zone != scw.ZoneFrPar2 {
			t.Fatalf("detach zone = %q, want %q", zone, scw.ZoneFrPar2)
		}
		if serverID != "srv-123" || privateNetworkID != "pn-old" {
			t.Fatalf("detach target = (%q,%q), want (%q,%q)", serverID, privateNetworkID, "srv-123", "pn-old")
		}
		return nil
	}
	listPrivateNetworkIPAMIPs = func(context.Context, *Client, scw.Region, string) ([]*ipam.IP, error) {
		return nil, nil
	}

	var deletedID string
	deletePrivateNetworkResource = func(_ context.Context, _ *Client, req *vpc.DeletePrivateNetworkRequest) error {
		deletedID = req.PrivateNetworkID
		return nil
	}

	var recreatedCIDR string
	createPrivateNetworkResource = func(_ context.Context, _ *Client, req *vpc.CreatePrivateNetworkRequest) (*vpc.PrivateNetwork, error) {
		recreatedCIDR = req.Subnets[0].String()
		return &vpc.PrivateNetwork{
			ID:                             "pn-new",
			Name:                           req.Name,
			VpcID:                          "vpc-123",
			DHCPEnabled:                    true,
			DefaultRoutePropagationEnabled: true,
			Subnets:                        []*vpc.Subnet{{Subnet: req.Subnets[0]}},
		}, nil
	}

	network, err := EnsureNetworkFoundation(context.Background(), &Client{}, NetworkFoundationParams{
		Region:                 scw.RegionFrPar,
		ProjectID:              "project-id",
		VPCName:                "waddle-cloud",
		PrivateNetworkName:     "waddle-cloud-private",
		PrivateNetworkIPv4CIDR: "172.16.16.0/24",
		AllowCIDRReplacement:   true,
		ReplacementServerID:    "srv-123",
		ReplacementServerZone:  scw.ZoneFrPar2,
	})
	if err != nil {
		t.Fatalf("EnsureNetworkFoundation returned error: %v", err)
	}
	if !detached {
		t.Fatal("expected target server attachment to be detached")
	}
	if deletedID != "pn-old" {
		t.Fatalf("deleted private network id = %q, want %q", deletedID, "pn-old")
	}
	if recreatedCIDR != "172.16.16.0/24" {
		t.Fatalf("recreated private network cidr = %q, want %q", recreatedCIDR, "172.16.16.0/24")
	}
	if network.PrivateNetworkID != "pn-new" {
		t.Fatalf("private network id = %q, want %q", network.PrivateNetworkID, "pn-new")
	}
}

func TestEnsureNetworkFoundationRefusesReplacementWhenOtherResourcesRemainAttached(t *testing.T) {
	restoreNetworkTestFns()
	t.Cleanup(restoreNetworkTestFns)

	ensureVPCFn = func(context.Context, *Client, scw.Region, string, string, string, []string) (*vpc.VPC, error) {
		return &vpc.VPC{ID: "vpc-123", Name: "waddle-cloud"}, nil
	}
	listPrivateNetworksByName = func(context.Context, *Client, *vpc.ListPrivateNetworksRequest) ([]*vpc.PrivateNetwork, error) {
		return []*vpc.PrivateNetwork{
			{
				ID:      "pn-old",
				Name:    "waddle-cloud-private",
				VpcID:   "vpc-123",
				Subnets: []*vpc.Subnet{{Subnet: mustNetworkIPNet(t, "172.16.4.0/22")}},
			},
		}, nil
	}
	listServerPrivateNetworkAttachments = func(context.Context, *Client, scw.Zone, string) ([]*baremetalv3.ServerPrivateNetwork, error) {
		return nil, nil
	}
	listPrivateNetworkIPAMIPs = func(context.Context, *Client, scw.Region, string) ([]*ipam.IP, error) {
		return []*ipam.IP{
			{
				ID:      "ipam-1",
				Address: mustAddressIPNet(t, "172.16.4.10"),
				Resource: &ipam.Resource{
					ID:   "other-resource",
					Type: ipam.ResourceTypeBaremetalPrivateNic,
				},
			},
		}, nil
	}
	deletePrivateNetworkResource = func(context.Context, *Client, *vpc.DeletePrivateNetworkRequest) error {
		t.Fatal("delete private network should not run when blockers remain")
		return nil
	}
	createPrivateNetworkResource = func(context.Context, *Client, *vpc.CreatePrivateNetworkRequest) (*vpc.PrivateNetwork, error) {
		t.Fatal("recreate private network should not run when blockers remain")
		return nil, nil
	}

	_, err := EnsureNetworkFoundation(context.Background(), &Client{}, NetworkFoundationParams{
		Region:                 scw.RegionFrPar,
		ProjectID:              "project-id",
		VPCName:                "waddle-cloud",
		PrivateNetworkName:     "waddle-cloud-private",
		PrivateNetworkIPv4CIDR: "172.16.16.0/24",
		AllowCIDRReplacement:   true,
		ReplacementServerID:    "srv-123",
		ReplacementServerZone:  scw.ZoneFrPar2,
	})
	if err == nil {
		t.Fatal("expected replacement refusal error, got nil")
	}
	if !strings.Contains(err.Error(), "replacement was refused because other resources are still attached") {
		t.Fatalf("expected blocker refusal error, got %q", err)
	}
	if !strings.Contains(err.Error(), "other-resource") {
		t.Fatalf("expected blocker identifier in error, got %q", err)
	}
}

func mustNetworkIPNet(t *testing.T, cidr string) scw.IPNet {
	t.Helper()

	_, network, err := net.ParseCIDR(cidr)
	if err != nil {
		t.Fatalf("parse cidr %q: %v", cidr, err)
	}

	return scw.IPNet{IPNet: *network}
}

func mustAddressIPNet(t *testing.T, ip string) scw.IPNet {
	t.Helper()

	parsed := net.ParseIP(ip)
	if parsed == nil || parsed.To4() == nil {
		t.Fatalf("parse ip %q: invalid", ip)
	}

	return scw.IPNet{
		IPNet: net.IPNet{
			IP:   parsed.To4(),
			Mask: net.CIDRMask(32, 32),
		},
	}
}

func TestEnsureNetworkFoundationWrapsDeleteErrors(t *testing.T) {
	restoreNetworkTestFns()
	t.Cleanup(restoreNetworkTestFns)

	ensureVPCFn = func(context.Context, *Client, scw.Region, string, string, string, []string) (*vpc.VPC, error) {
		return &vpc.VPC{ID: "vpc-123", Name: "waddle-cloud"}, nil
	}
	listPrivateNetworksByName = func(context.Context, *Client, *vpc.ListPrivateNetworksRequest) ([]*vpc.PrivateNetwork, error) {
		return []*vpc.PrivateNetwork{
			{
				ID:      "pn-old",
				Name:    "waddle-cloud-private",
				VpcID:   "vpc-123",
				Subnets: []*vpc.Subnet{{Subnet: mustNetworkIPNet(t, "172.16.4.0/22")}},
			},
		}, nil
	}
	listPrivateNetworkIPAMIPs = func(context.Context, *Client, scw.Region, string) ([]*ipam.IP, error) {
		return nil, nil
	}
	deletePrivateNetworkResource = func(context.Context, *Client, *vpc.DeletePrivateNetworkRequest) error {
		return errors.New("delete failed")
	}

	_, err := EnsureNetworkFoundation(context.Background(), &Client{}, NetworkFoundationParams{
		Region:                 scw.RegionFrPar,
		ProjectID:              "project-id",
		VPCName:                "waddle-cloud",
		PrivateNetworkName:     "waddle-cloud-private",
		PrivateNetworkIPv4CIDR: "172.16.16.0/24",
		AllowCIDRReplacement:   true,
	})
	if err == nil {
		t.Fatal("expected delete failure, got nil")
	}
	if !strings.Contains(err.Error(), "delete private network pn-old for CIDR replacement") {
		t.Fatalf("expected wrapped delete error, got %q", err)
	}
}
