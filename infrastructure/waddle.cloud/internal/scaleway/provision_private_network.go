package scaleway

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"strings"

	baremetal "github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
	baremetalv3 "github.com/scaleway/scaleway-sdk-go/api/baremetal/v3"
	ipam "github.com/scaleway/scaleway-sdk-go/api/ipam/v1"
	"github.com/scaleway/scaleway-sdk-go/scw"
)

var errPrivateNetworkReservedIPNotFound = errors.New("reserved private IP not found")

var privateNetworkIPIDLookup = findPrivateNetworkIPIDByAddress
var bookPrivateNetworkIP = func(ctx context.Context, client *Client, region scw.Region, projectID, privateNetworkID string, address net.IP, tags []string) (*ipam.IP, error) {
	return client.IPAM.BookIP(&ipam.BookIPRequest{
		Region:    region,
		ProjectID: projectID,
		Source: &ipam.Source{
			PrivateNetworkID: &privateNetworkID,
		},
		Address: &address,
		Tags:    tags,
	}, scw.WithContext(ctx))
}
var addServerPrivateNetworkWithIPAMIDs = func(ctx context.Context, client *Client, req *baremetalv3.PrivateNetworkAPIAddServerPrivateNetworkRequest) (*baremetalv3.ServerPrivateNetwork, error) {
	return baremetalv3.NewPrivateNetworkAPI(client.Core).AddServerPrivateNetwork(req, scw.WithContext(ctx))
}
var listServerPrivateNetworkAttachments = func(ctx context.Context, client *Client, zone scw.Zone, serverID string) ([]*baremetalv3.ServerPrivateNetwork, error) {
	serverIDFilter := strings.TrimSpace(serverID)
	resp, err := client.BaremetalPrivateNetworkV3.ListServerPrivateNetworks(&baremetalv3.PrivateNetworkAPIListServerPrivateNetworksRequest{
		Zone:     zone,
		ServerID: &serverIDFilter,
	}, scw.WithAllPages(), scw.WithContext(ctx))
	if err != nil {
		return nil, err
	}

	return resp.ServerPrivateNetworks, nil
}
var addServerPrivateNetworkWithoutReservedIP = func(ctx context.Context, client *Client, zone scw.Zone, serverID, privateNetworkID string) error {
	_, err := client.BaremetalPrivateNetwork.AddServerPrivateNetwork(&baremetal.PrivateNetworkAPIAddServerPrivateNetworkRequest{
		Zone:             zone,
		ServerID:         serverID,
		PrivateNetworkID: privateNetworkID,
	}, scw.WithContext(ctx))
	return err
}

func EnsureServerPrivateNetworkAttachment(
	ctx context.Context,
	client *Client,
	zone scw.Zone,
	serverID,
	privateNetworkID,
	reservedPrivateIP string,
) error {
	serverID = strings.TrimSpace(serverID)
	privateNetworkID = strings.TrimSpace(privateNetworkID)
	if serverID == "" {
		return fmt.Errorf("server ID is required")
	}
	if privateNetworkID == "" {
		return fmt.Errorf("private network ID is required")
	}

	attachments, err := listServerPrivateNetworkAttachments(ctx, client, zone, serverID)
	if err != nil {
		return fmt.Errorf("list private network attachments for server %s: %w", serverID, err)
	}
	for _, attachment := range attachments {
		if attachment == nil {
			continue
		}
		if strings.TrimSpace(attachment.PrivateNetworkID) == privateNetworkID {
			return nil
		}
	}

	if strings.TrimSpace(reservedPrivateIP) != "" {
		if err := addServerPrivateNetworkWithReservedIP(ctx, client, zone, serverID, privateNetworkID, reservedPrivateIP); err != nil {
			return err
		}
		return nil
	}

	if err := addServerPrivateNetworkWithoutReservedIP(ctx, client, zone, serverID, privateNetworkID); err != nil {
		return fmt.Errorf("attach server %s to private network %s: %w", serverID, privateNetworkID, err)
	}

	return nil
}

// EnsureReservedPrivateNetworkIP makes sure a requested private IPv4 exists in Scaleway IPAM
// before a reinstall flow attempts to attach the server with that address.
func EnsureReservedPrivateNetworkIP(
	ctx context.Context,
	client *Client,
	zone scw.Zone,
	serverID,
	privateNetworkID,
	reservedIP string,
) error {
	serverID = strings.TrimSpace(serverID)
	privateNetworkID = strings.TrimSpace(privateNetworkID)
	reservedIP = strings.TrimSpace(reservedIP)

	if serverID == "" {
		return fmt.Errorf("server ID is required")
	}
	if privateNetworkID == "" {
		return fmt.Errorf("private network ID is required")
	}
	if reservedIP == "" {
		return fmt.Errorf("reserved private IP cannot be empty")
	}

	parsedIP := net.ParseIP(reservedIP)
	if parsedIP == nil || parsedIP.To4() == nil {
		return fmt.Errorf("reserved private IP must be a valid IPv4 address, got %q", reservedIP)
	}
	parsedIP = parsedIP.To4()

	attachments, err := listServerPrivateNetworkAttachments(ctx, client, zone, serverID)
	if err != nil {
		return fmt.Errorf("list private network attachments for server %s: %w", serverID, err)
	}
	for _, attachment := range attachments {
		if attachment == nil {
			continue
		}
		if strings.TrimSpace(attachment.PrivateNetworkID) == privateNetworkID {
			slog.Info("reserved private ip already satisfied by existing private network attachment",
				"server_id", serverID,
				"private_network_id", privateNetworkID,
				"reserved_ip", reservedIP,
			)
			return nil
		}
	}

	region, err := zone.Region()
	if err != nil {
		return fmt.Errorf("derive region from zone %q: %w", zone, err)
	}

	privateNetwork, err := getPrivateNetworkForCIDRLookup(ctx, client, region, privateNetworkID)
	if err != nil {
		return fmt.Errorf("get private network %s: %w", privateNetworkID, err)
	}

	candidates := ipv4CIDRsFromSubnets(privateNetwork)
	if len(candidates) == 0 {
		candidates, err = ipv4CIDRsFromIPAM(ctx, client, region, privateNetworkID)
		if err != nil {
			return fmt.Errorf("list ipam ips for private network %s: %w", privateNetworkID, err)
		}
	}
	if len(candidates) == 0 {
		return fmt.Errorf("no IPv4 CIDRs could be discovered for private network %s", privateNetworkID)
	}

	matchesNetwork := false
	for _, candidate := range candidates {
		_, network, err := net.ParseCIDR(candidate)
		if err != nil {
			continue
		}
		if network.Contains(parsedIP) {
			matchesNetwork = true
			break
		}
	}
	if !matchesNetwork {
		return fmt.Errorf("reserved ip %s is outside the private network %s IPv4 CIDRs (%s)", reservedIP, privateNetworkID, strings.Join(candidates, ", "))
	}

	ips, err := listPrivateNetworkIPAMIPs(ctx, client, region, privateNetworkID)
	if err != nil {
		return fmt.Errorf("list ipam ips for private network %s: %w", privateNetworkID, err)
	}
	for _, candidate := range ips {
		if candidate == nil || candidate.Address.IP == nil || candidate.Address.IP.To4() == nil {
			continue
		}
		if !candidate.Address.IP.To4().Equal(parsedIP) {
			continue
		}
		if candidate.Resource != nil && strings.TrimSpace(candidate.Resource.ID) != "" {
			return fmt.Errorf("reserved ip %s (%s) is already attached to resource %s (%s)", reservedIP, candidate.ID, candidate.Resource.ID, candidate.Resource.Type)
		}

		slog.Info("reserved private ip already exists",
			"private_network_id", privateNetworkID,
			"reserved_ip", reservedIP,
			"ipam_ip_id", candidate.ID,
		)
		return nil
	}

	projectID := strings.TrimSpace(privateNetwork.ProjectID)
	if projectID == "" {
		projectID, err = resolveProjectID(client, "")
		if err != nil {
			return fmt.Errorf("resolve project id for private network %s: %w", privateNetworkID, err)
		}
	}

	if _, err := bookPrivateNetworkIP(ctx, client, region, projectID, privateNetworkID, parsedIP, []string{"waddle-cloud:managed"}); err != nil {
		return fmt.Errorf("reserve private ip %s in private network %s: %w", reservedIP, privateNetworkID, err)
	}

	slog.Info("reserved private ip created",
		"private_network_id", privateNetworkID,
		"reserved_ip", reservedIP,
		"project_id", projectID,
	)

	return nil
}

// WaitForReady polls Scaleway until the server reaches a terminal state.

func addServerPrivateNetworkWithReservedIP(ctx context.Context, client *Client, zone scw.Zone, serverID, privateNetworkID, reservedIP string) error {
	reservedIP = strings.TrimSpace(reservedIP)
	if reservedIP == "" {
		return fmt.Errorf("reserved private IP cannot be empty")
	}
	if parsed := net.ParseIP(reservedIP); parsed == nil || parsed.To4() == nil {
		return fmt.Errorf("reserved private IP must be a valid IPv4 address, got %q", reservedIP)
	}

	region, err := zone.Region()
	if err != nil {
		return fmt.Errorf("derive region from zone %q: %w", zone, err)
	}

	reservedIPID, err := privateNetworkIPIDLookup(ctx, client.IPAM, region, privateNetworkID, reservedIP)
	if err != nil {
		if errors.Is(err, errPrivateNetworkReservedIPNotFound) {
			return explainReservedPrivateIPLookupFailure(ctx, client, region, privateNetworkID, reservedIP)
		}
		return err
	}

	req := &baremetalv3.PrivateNetworkAPIAddServerPrivateNetworkRequest{
		Zone:             zone,
		ServerID:         serverID,
		PrivateNetworkID: privateNetworkID,
		IpamIPIDs:        []string{reservedIPID},
	}

	if _, err := addServerPrivateNetworkWithIPAMIDs(ctx, client, req); err != nil {
		return fmt.Errorf("attach reserved private IP %s via baremetal v3: %w", reservedIP, err)
	}

	return nil
}

func explainReservedPrivateIPLookupFailure(ctx context.Context, client *Client, region scw.Region, privateNetworkID, reservedIP string) error {
	privateNetwork, err := getPrivateNetworkForCIDRLookup(ctx, client, region, privateNetworkID)
	if err != nil {
		return fmt.Errorf("lookup private network %s while validating reserved ip %s: %w", privateNetworkID, reservedIP, err)
	}

	candidates := ipv4CIDRsFromSubnets(privateNetwork)
	if len(candidates) == 0 {
		candidates, err = ipv4CIDRsFromIPAM(ctx, client, region, privateNetworkID)
		if err != nil {
			return fmt.Errorf("reserved ip %s was not found in private network %s, and IPv4 CIDR discovery failed: %w", reservedIP, privateNetworkID, err)
		}
	}

	if len(candidates) == 0 {
		return fmt.Errorf("reserved ip %s was not found in private network %s, and no IPv4 CIDRs could be discovered for that network", reservedIP, privateNetworkID)
	}

	parsedIP := net.ParseIP(reservedIP)
	if parsedIP == nil || parsedIP.To4() == nil {
		return fmt.Errorf("reserved private IP must be a valid IPv4 address, got %q", reservedIP)
	}

	for _, candidate := range candidates {
		_, network, err := net.ParseCIDR(candidate)
		if err != nil {
			continue
		}
		if network.Contains(parsedIP) {
			return fmt.Errorf("reserved ip %s was not found in private network %s; reserve it first in Scaleway IPAM (discovered IPv4 CIDRs: %s)", reservedIP, privateNetworkID, strings.Join(candidates, ", "))
		}
	}

	return fmt.Errorf("reserved ip %s is outside the private network %s IPv4 CIDRs (%s)", reservedIP, privateNetworkID, strings.Join(candidates, ", "))
}

func findPrivateNetworkIPIDByAddress(ctx context.Context, ipamAPI *ipam.API, region scw.Region, privateNetworkID, targetIPv4 string) (string, error) {
	resp, err := ipamAPI.ListIPs(&ipam.ListIPsRequest{
		Region:           region,
		PrivateNetworkID: &privateNetworkID,
	}, scw.WithAllPages(), scw.WithContext(ctx))
	if err != nil {
		return "", fmt.Errorf("list ipam ips for private network %s: %w", privateNetworkID, err)
	}

	for _, candidate := range resp.IPs {
		if candidate == nil || candidate.Address.IP == nil || candidate.Address.IP.To4() == nil {
			continue
		}
		if candidate.Address.IP.String() != targetIPv4 {
			continue
		}
		if candidate.Resource != nil && candidate.Resource.ID != "" {
			return "", fmt.Errorf("reserved ip %s (%s) is already attached to resource %s (%s)", targetIPv4, candidate.ID, candidate.Resource.ID, candidate.Resource.Type)
		}
		return candidate.ID, nil
	}

	return "", fmt.Errorf("%w: reserved ip %s was not found in private network %s", errPrivateNetworkReservedIPNotFound, targetIPv4, privateNetworkID)
}
