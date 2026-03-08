package scaleway

import (
	"context"
	"fmt"
	"log/slog"
	"net"
	"strings"

	baremetalv3 "github.com/scaleway/scaleway-sdk-go/api/baremetal/v3"
	vpc "github.com/scaleway/scaleway-sdk-go/api/vpc/v2"
	"github.com/scaleway/scaleway-sdk-go/scw"
)

var listPrivateNetworksByName = func(ctx context.Context, client *Client, req *vpc.ListPrivateNetworksRequest) ([]*vpc.PrivateNetwork, error) {
	resp, err := client.VPC.ListPrivateNetworks(req, scw.WithAllPages(), scw.WithContext(ctx))
	if err != nil {
		return nil, err
	}
	return resp.PrivateNetworks, nil
}

var createPrivateNetworkResource = func(ctx context.Context, client *Client, req *vpc.CreatePrivateNetworkRequest) (*vpc.PrivateNetwork, error) {
	return client.VPC.CreatePrivateNetwork(req, scw.WithContext(ctx))
}

var updatePrivateNetworkResource = func(ctx context.Context, client *Client, req *vpc.UpdatePrivateNetworkRequest) (*vpc.PrivateNetwork, error) {
	return client.VPC.UpdatePrivateNetwork(req, scw.WithContext(ctx))
}

var enablePrivateNetworkDHCP = func(ctx context.Context, client *Client, req *vpc.EnableDHCPRequest) (*vpc.PrivateNetwork, error) {
	return client.VPC.EnableDHCP(req, scw.WithContext(ctx))
}

var deletePrivateNetworkResource = func(ctx context.Context, client *Client, req *vpc.DeletePrivateNetworkRequest) error {
	return client.VPC.DeletePrivateNetwork(req, scw.WithContext(ctx))
}

var deleteServerPrivateNetworkAttachment = func(ctx context.Context, client *Client, zone scw.Zone, serverID, privateNetworkID string) error {
	return client.BaremetalPrivateNetworkV3.DeleteServerPrivateNetwork(&baremetalv3.PrivateNetworkAPIDeleteServerPrivateNetworkRequest{
		Zone:             zone,
		ServerID:         serverID,
		PrivateNetworkID: privateNetworkID,
	}, scw.WithContext(ctx))
}
var ensureVPCFn = ensureVPC
var ensurePrivateNetworkFn = ensurePrivateNetwork

// NetworkFoundationParams describes the desired VPC/private-network baseline.
type NetworkFoundationParams struct {
	Region                 scw.Region
	ProjectID              string
	VPCID                  string
	VPCName                string
	VPCTags                []string
	PrivateNetworkID       string
	PrivateNetworkName     string
	PrivateNetworkTags     []string
	PrivateNetworkIPv4CIDR string
	AllowCIDRReplacement   bool
	ReplacementServerID    string
	ReplacementServerZone  scw.Zone
}

// NetworkFoundation contains the resolved VPC/private-network details.
type NetworkFoundation struct {
	Region             scw.Region
	ProjectID          string
	VPCID              string
	VPCName            string
	PrivateNetworkID   string
	PrivateNetworkName string
}

// EnsureNetworkFoundation creates or reuses a VPC + private network in an idempotent way.
func EnsureNetworkFoundation(ctx context.Context, client *Client, params NetworkFoundationParams) (*NetworkFoundation, error) {
	projectID, err := resolveProjectID(client, params.ProjectID)
	if err != nil {
		return nil, err
	}
	params.ProjectID = projectID

	if params.Region == "" {
		if region, ok := client.Core.GetDefaultRegion(); ok && region != "" {
			params.Region = region
		} else {
			return nil, fmt.Errorf("region is required (set SCW_DEFAULT_REGION or client default region)")
		}
	}

	normalizedCIDR, subnet, err := parseIPv4CIDR(params.PrivateNetworkIPv4CIDR)
	if err != nil {
		return nil, fmt.Errorf("validate private network ipv4 cidr: %w", err)
	}
	params.PrivateNetworkIPv4CIDR = normalizedCIDR

	vpcResource, err := ensureVPCFn(ctx, client, params.Region, projectID, params.VPCID, params.VPCName, params.VPCTags)
	if err != nil {
		return nil, err
	}

	privateNetwork, err := ensurePrivateNetworkFn(ctx, client, params, vpcResource.ID, *subnet)
	if err != nil {
		return nil, err
	}

	return &NetworkFoundation{
		Region:             params.Region,
		ProjectID:          projectID,
		VPCID:              vpcResource.ID,
		VPCName:            vpcResource.Name,
		PrivateNetworkID:   privateNetwork.ID,
		PrivateNetworkName: privateNetwork.Name,
	}, nil
}

func resolveProjectID(client *Client, explicitProjectID string) (string, error) {
	projectID := strings.TrimSpace(explicitProjectID)
	if projectID != "" {
		return projectID, nil
	}

	defaultProjectID, ok := client.Core.GetDefaultProjectID()
	if !ok || strings.TrimSpace(defaultProjectID) == "" {
		return "", fmt.Errorf("project ID is required (set SCW_DEFAULT_PROJECT_ID)")
	}
	return defaultProjectID, nil
}

func ensureVPC(ctx context.Context, client *Client, region scw.Region, projectID, vpcID, vpcName string, tags []string) (*vpc.VPC, error) {
	if vpcID != "" {
		resource, err := client.VPC.GetVPC(&vpc.GetVPCRequest{
			Region: region,
			VpcID:  vpcID,
		}, scw.WithContext(ctx))
		if err != nil {
			return nil, fmt.Errorf("get vpc %s: %w", vpcID, err)
		}
		if !resource.RoutingEnabled {
			updated, err := client.VPC.EnableRouting(&vpc.EnableRoutingRequest{
				Region: region,
				VpcID:  resource.ID,
			}, scw.WithContext(ctx))
			if err != nil {
				return nil, fmt.Errorf("enable routing on vpc %s: %w", resource.ID, err)
			}
			resource = updated
		}
		return resource, nil
	}

	name := strings.TrimSpace(vpcName)
	if name == "" {
		return nil, fmt.Errorf("vpc name is required when vpc ID is not provided")
	}

	listResp, err := client.VPC.ListVPCs(&vpc.ListVPCsRequest{
		Region:    region,
		ProjectID: &projectID,
		Name:      &name,
	}, scw.WithAllPages(), scw.WithContext(ctx))
	if err != nil {
		return nil, fmt.Errorf("list vpcs: %w", err)
	}

	for _, existing := range listResp.Vpcs {
		if existing == nil || existing.Name != name {
			continue
		}
		if !existing.RoutingEnabled {
			updated, err := client.VPC.EnableRouting(&vpc.EnableRoutingRequest{
				Region: region,
				VpcID:  existing.ID,
			}, scw.WithContext(ctx))
			if err != nil {
				return nil, fmt.Errorf("enable routing on existing vpc %s: %w", existing.ID, err)
			}
			return updated, nil
		}
		return existing, nil
	}

	created, err := client.VPC.CreateVPC(&vpc.CreateVPCRequest{
		Region:        region,
		Name:          name,
		ProjectID:     projectID,
		Tags:          tags,
		EnableRouting: true,
	}, scw.WithContext(ctx))
	if err != nil {
		return nil, fmt.Errorf("create vpc %q: %w", name, err)
	}

	return created, nil
}

func ensurePrivateNetwork(ctx context.Context, client *Client, params NetworkFoundationParams, vpcID string, subnet scw.IPNet) (*vpc.PrivateNetwork, error) {
	region := params.Region
	projectID := params.ProjectID
	privateNetworkID := strings.TrimSpace(params.PrivateNetworkID)
	privateNetworkName := strings.TrimSpace(params.PrivateNetworkName)
	tags := params.PrivateNetworkTags

	if privateNetworkID != "" {
		resource, err := getPrivateNetworkForCIDRLookup(ctx, client, region, privateNetworkID)
		if err != nil {
			return nil, fmt.Errorf("get private network %s: %w", privateNetworkID, err)
		}
		if resource.VpcID != "" && resource.VpcID != vpcID {
			return nil, fmt.Errorf("private network %s belongs to vpc %s, expected %s", resource.ID, resource.VpcID, vpcID)
		}
		resource, err = ensurePrivateNetworkCIDRConformance(ctx, client, params, resource, subnet)
		if err != nil {
			return nil, err
		}
		return ensurePrivateNetworkDefaults(ctx, client, region, resource)
	}

	if privateNetworkName == "" {
		return nil, fmt.Errorf("private network name is required when private network ID is not provided")
	}

	privateNetworks, err := listPrivateNetworksByName(ctx, client, &vpc.ListPrivateNetworksRequest{
		Region:    region,
		ProjectID: &projectID,
		VpcID:     &vpcID,
		Name:      &privateNetworkName,
	})
	if err != nil {
		return nil, fmt.Errorf("list private networks: %w", err)
	}

	for _, existing := range privateNetworks {
		if existing == nil || existing.Name != privateNetworkName {
			continue
		}
		existing, err = ensurePrivateNetworkCIDRConformance(ctx, client, params, existing, subnet)
		if err != nil {
			return nil, err
		}
		return ensurePrivateNetworkDefaults(ctx, client, region, existing)
	}

	created, err := createPrivateNetworkResource(ctx, client, &vpc.CreatePrivateNetworkRequest{
		Region:                         region,
		Name:                           privateNetworkName,
		ProjectID:                      projectID,
		Tags:                           tags,
		Subnets:                        []scw.IPNet{subnet},
		VpcID:                          &vpcID,
		DefaultRoutePropagationEnabled: true,
	})
	if err != nil {
		return nil, fmt.Errorf("create private network %q: %w", privateNetworkName, err)
	}

	return ensurePrivateNetworkDefaults(ctx, client, region, created)
}

func ensurePrivateNetworkCIDRConformance(
	ctx context.Context,
	client *Client,
	params NetworkFoundationParams,
	resource *vpc.PrivateNetwork,
	subnet scw.IPNet,
) (*vpc.PrivateNetwork, error) {
	if resource == nil {
		return nil, fmt.Errorf("private network is nil")
	}

	desiredCIDR := strings.TrimSpace(params.PrivateNetworkIPv4CIDR)
	candidates := ipv4CIDRsFromSubnets(resource)
	if len(candidates) == 0 {
		var err error
		candidates, err = ipv4CIDRsFromIPAM(ctx, client, params.Region, resource.ID)
		if err != nil {
			return nil, fmt.Errorf("discover IPv4 CIDRs for private network %s: %w", resource.ID, err)
		}
	}
	candidates = uniqueSorted(candidates)

	if len(candidates) == 1 && candidates[0] == desiredCIDR {
		return resource, nil
	}

	if !params.AllowCIDRReplacement {
		return nil, fmt.Errorf(
			"private network %s (%s) IPv4 CIDRs %s do not match desired CIDR %s",
			resource.ID,
			strings.TrimSpace(resource.Name),
			displayCIDRs(candidates),
			desiredCIDR,
		)
	}

	replaced, err := replacePrivateNetworkForCIDRMismatch(ctx, client, params, resource, subnet, candidates)
	if err != nil {
		return nil, err
	}
	return replaced, nil
}

func replacePrivateNetworkForCIDRMismatch(
	ctx context.Context,
	client *Client,
	params NetworkFoundationParams,
	resource *vpc.PrivateNetwork,
	subnet scw.IPNet,
	currentCIDRs []string,
) (*vpc.PrivateNetwork, error) {
	if resource == nil {
		return nil, fmt.Errorf("private network is nil")
	}

	slog.Info("private network CIDR mismatch detected; replacing network for reinstall flow",
		"private_network_id", resource.ID,
		"private_network_name", resource.Name,
		"current_ipv4_cidrs", displayCIDRs(currentCIDRs),
		"desired_ipv4_cidr", params.PrivateNetworkIPv4CIDR,
	)

	if serverID := strings.TrimSpace(params.ReplacementServerID); serverID != "" {
		if strings.TrimSpace(string(params.ReplacementServerZone)) == "" {
			return nil, fmt.Errorf("replacement server zone is required when replacement server ID is provided")
		}
		if err := detachServerFromPrivateNetworkIfAttached(ctx, client, params.ReplacementServerZone, serverID, resource.ID); err != nil {
			return nil, err
		}
	}

	blockers, err := attachedPrivateNetworkResources(ctx, client, params.Region, resource.ID)
	if err != nil {
		return nil, err
	}
	if len(blockers) > 0 {
		return nil, fmt.Errorf(
			"private network %s (%s) does not match desired CIDR %s and replacement was refused because other resources are still attached: %s",
			resource.ID,
			strings.TrimSpace(resource.Name),
			params.PrivateNetworkIPv4CIDR,
			strings.Join(blockers, ", "),
		)
	}

	if err := deletePrivateNetworkResource(ctx, client, &vpc.DeletePrivateNetworkRequest{
		Region:           params.Region,
		PrivateNetworkID: resource.ID,
	}); err != nil {
		return nil, fmt.Errorf("delete private network %s for CIDR replacement: %w", resource.ID, err)
	}

	name := strings.TrimSpace(resource.Name)
	if override := strings.TrimSpace(params.PrivateNetworkName); override != "" {
		name = override
	}
	if name == "" {
		return nil, fmt.Errorf("private network name is required for CIDR replacement")
	}

	tags := params.PrivateNetworkTags
	if len(tags) == 0 {
		tags = resource.Tags
	}

	recreated, err := createPrivateNetworkResource(ctx, client, &vpc.CreatePrivateNetworkRequest{
		Region:                         params.Region,
		Name:                           name,
		ProjectID:                      params.ProjectID,
		Tags:                           tags,
		Subnets:                        []scw.IPNet{subnet},
		VpcID:                          &resource.VpcID,
		DefaultRoutePropagationEnabled: true,
	})
	if err != nil {
		return nil, fmt.Errorf("recreate private network %q with cidr %s: %w", name, params.PrivateNetworkIPv4CIDR, err)
	}

	slog.Info("recreated private network with desired CIDR",
		"old_private_network_id", resource.ID,
		"new_private_network_id", recreated.ID,
		"private_network_name", name,
		"desired_ipv4_cidr", params.PrivateNetworkIPv4CIDR,
	)

	return ensurePrivateNetworkDefaults(ctx, client, params.Region, recreated)
}

func detachServerFromPrivateNetworkIfAttached(ctx context.Context, client *Client, zone scw.Zone, serverID, privateNetworkID string) error {
	attachments, err := listServerPrivateNetworkAttachments(ctx, client, zone, serverID)
	if err != nil {
		return fmt.Errorf("list private network attachments for replacement server %s: %w", serverID, err)
	}

	for _, attachment := range attachments {
		if attachment == nil || strings.TrimSpace(attachment.PrivateNetworkID) != privateNetworkID {
			continue
		}

		if err := deleteServerPrivateNetworkAttachment(ctx, client, zone, serverID, privateNetworkID); err != nil {
			if isScalewayNotFound(err) {
				return nil
			}
			return fmt.Errorf("detach replacement server %s from private network %s: %w", serverID, privateNetworkID, err)
		}

		slog.Info("detached replacement server from private network before CIDR replacement",
			"server_id", serverID,
			"private_network_id", privateNetworkID,
			"zone", zone,
		)
		return nil
	}

	return nil
}

func attachedPrivateNetworkResources(ctx context.Context, client *Client, region scw.Region, privateNetworkID string) ([]string, error) {
	ips, err := listPrivateNetworkIPAMIPs(ctx, client, region, privateNetworkID)
	if err != nil {
		return nil, fmt.Errorf("list attached resources for private network %s: %w", privateNetworkID, err)
	}

	blockers := make([]string, 0, len(ips))
	for _, candidate := range ips {
		if candidate == nil || candidate.Resource == nil || strings.TrimSpace(candidate.Resource.ID) == "" {
			continue
		}

		description := fmt.Sprintf("%s:%s", candidate.Resource.Type, strings.TrimSpace(candidate.Resource.ID))
		if candidate.Address.IP != nil {
			description += fmt.Sprintf("(%s)", candidate.Address.IP.String())
		}
		blockers = append(blockers, description)
	}

	return uniqueSorted(blockers), nil
}

func parseIPv4CIDR(value string) (string, *scw.IPNet, error) {
	value = strings.TrimSpace(value)
	if value == "" {
		return "", nil, fmt.Errorf("private network IPv4 CIDR is required")
	}

	parsedIP, network, err := net.ParseCIDR(value)
	if err != nil || parsedIP == nil || parsedIP.To4() == nil {
		return "", nil, fmt.Errorf("private network IPv4 CIDR must be a valid IPv4 CIDR")
	}

	ipNet := scw.IPNet{IPNet: *network}
	return network.String(), &ipNet, nil
}

func displayCIDRs(values []string) string {
	values = uniqueSorted(values)
	if len(values) == 0 {
		return "<none>"
	}
	return strings.Join(values, ", ")
}

func ensurePrivateNetworkDefaults(ctx context.Context, client *Client, region scw.Region, resource *vpc.PrivateNetwork) (*vpc.PrivateNetwork, error) {
	if resource == nil {
		return nil, fmt.Errorf("private network is nil")
	}

	current := resource
	if !current.DefaultRoutePropagationEnabled {
		enablePropagation := true
		updated, err := updatePrivateNetworkResource(ctx, client, &vpc.UpdatePrivateNetworkRequest{
			Region:                         region,
			PrivateNetworkID:               current.ID,
			DefaultRoutePropagationEnabled: &enablePropagation,
		})
		if err != nil {
			return nil, fmt.Errorf("enable default route propagation on private network %s: %w", current.ID, err)
		}
		current = updated
	}

	if !current.DHCPEnabled {
		updated, err := enablePrivateNetworkDHCP(ctx, client, &vpc.EnableDHCPRequest{
			Region:           region,
			PrivateNetworkID: current.ID,
		})
		if err != nil {
			return nil, fmt.Errorf("enable dhcp on private network %s: %w", current.ID, err)
		}
		current = updated
	}

	return current, nil
}
