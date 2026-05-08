package scaleway

import (
	"context"
	"encoding/base64"
	"fmt"
	"log/slog"
	"math/rand"
	"strings"
	"time"

	baremetal "github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
	iam "github.com/scaleway/scaleway-sdk-go/api/iam/v1alpha1"
	"github.com/scaleway/scaleway-sdk-go/scw"
)

const defaultGitHubSSHKeyUser = "rawkode"

type ProvisionParams struct {
	OfferID                  string
	Zone                     scw.Zone
	OSID                     string
	Name                     string
	PrivateNetworkID         string
	BillingCycle             string
	CloudInitScript          string // Talos pivot cloud-init script
	SSHKeyGitHubUser         string
	PivotOSDisk              string
	PivotDataDisk            string
	SkipDataDiskPartitioning bool
	PrivateNetworkReservedIP string
}

// ReinstallParams holds all parameters for reinstalling an existing server with OS install.
type ReinstallParams struct {
	ServerID                 string
	Zone                     scw.Zone
	OSID                     string
	CloudInitScript          string // Talos pivot cloud-init script
	PivotOSDisk              string
	PivotDataDisk            string
	SkipDataDiskPartitioning bool
	Hostname                 string
	SSHKeyGitHubUser         string
}

// OrderServer creates a new bare metal server with Scaleway and triggers OS
// installation with a Talos pivot cloud-init script.
func OrderServer(ctx context.Context, client *Client, params ProvisionParams) (*baremetal.Server, error) {
	sshKeyIDs, err := listSSHKeyIDs(client.IAM)
	if err != nil {
		return nil, fmt.Errorf("list SSH keys: %w", err)
	}

	cloudInitBytes := []byte(params.CloudInitScript)

	effectiveOfferID, effectiveSubscriptionPeriod, err := resolveOfferForBillingCycle(ctx, client, params.Zone, params.OfferID, params.BillingCycle)
	if err != nil {
		return nil, fmt.Errorf("resolve offer for billing cycle: %w", err)
	}
	privateNetworkID := strings.TrimSpace(params.PrivateNetworkID)
	privateNetworkOptionIDs := []string(nil)
	if privateNetworkID != "" {
		offer, err := client.Baremetal.GetOffer(&baremetal.GetOfferRequest{
			Zone:    params.Zone,
			OfferID: effectiveOfferID,
		}, scw.WithContext(ctx))
		if err != nil {
			return nil, fmt.Errorf("get offer %s: %w", effectiveOfferID, err)
		}

		optionIDs, optionAlreadyEnabled, err := privateNetworkOptionIDsForOffer(offer, effectiveSubscriptionPeriod)
		if err != nil {
			return nil, fmt.Errorf("resolve private-network option for offer %s: %w", effectiveOfferID, err)
		}
		privateNetworkOptionIDs = optionIDs

		if optionAlreadyEnabled {
			slog.Info("offer already includes private-network option", "offer_id", effectiveOfferID)
		} else {
			slog.Info("enabling private-network option during server order", "offer_id", effectiveOfferID, "option_ids", optionIDs)
		}
	}

	osInfo, err := client.Baremetal.GetOS(&baremetal.GetOSRequest{
		Zone: params.Zone,
		OsID: params.OSID,
	}, scw.WithContext(ctx))
	if err != nil {
		return nil, fmt.Errorf("get OS %s: %w", params.OSID, err)
	}
	if !osInfo.CustomPartitioningSupported {
		return nil, fmt.Errorf("OS %s (%s) does not support custom partitioning", osInfo.Name, osInfo.ID)
	}

	partitioningSchema, err := buildInstallPartitioningSchema(params)
	if err != nil {
		return nil, fmt.Errorf("build install partitioning schema: %w", err)
	}

	serverName := strings.TrimSpace(params.Name)
	if serverName == "" {
		suffix := rand.Intn(99999) //nolint:gosec
		serverName = fmt.Sprintf("waddle-cloud-%s-%05d", time.Now().Format("20060102-150405"), suffix)
	}

	server, err := client.Baremetal.CreateServer(&baremetal.CreateServerRequest{
		Zone:        params.Zone,
		OfferID:     effectiveOfferID,
		Name:        serverName,
		Description: "Provisioned by waddle-cloud CLI (Talos)",
		Install: &baremetal.CreateServerRequestInstall{
			OsID:               params.OSID,
			Hostname:           "talos-pivot",
			SSHKeyIDs:          sshKeyIDs,
			PartitioningSchema: partitioningSchema,
		},
		OptionIDs: privateNetworkOptionIDs,
		UserData:  &cloudInitBytes,
	})
	if err != nil {
		return nil, fmt.Errorf("create server: %w", err)
	}

	slog.Info("server ordered with OS install",
		"server_id", server.ID,
		"offer", effectiveOfferID,
		"billing_cycle", effectiveSubscriptionPeriod,
		"os", params.OSID,
		"zone", params.Zone,
	)

	if privateNetworkID != "" {
		err = EnsureServerPrivateNetworkAttachment(ctx, client, params.Zone, server.ID, privateNetworkID, params.PrivateNetworkReservedIP)
		if err != nil {
			return nil, fmt.Errorf("attach server to private network: %w", err)
		}
		slog.Info("server attached to private network", "server_id", server.ID, "private_network_id", privateNetworkID)
	}

	return server, nil
}

// ReinstallServer reinstalls a bare metal server with a Talos pivot cloud-init script.
func ReinstallServer(ctx context.Context, client *Client, params ReinstallParams) (*baremetal.Server, error) {
	serverID := strings.TrimSpace(params.ServerID)
	if serverID == "" {
		return nil, fmt.Errorf("server ID is required")
	}

	sshKeyIDs, err := listSSHKeyIDs(client.IAM)
	if err != nil {
		return nil, fmt.Errorf("list SSH keys: %w", err)
	}

	osInfo, err := client.Baremetal.GetOS(&baremetal.GetOSRequest{
		Zone: params.Zone,
		OsID: params.OSID,
	}, scw.WithContext(ctx))
	if err != nil {
		return nil, fmt.Errorf("get OS %s: %w", params.OSID, err)
	}
	if !osInfo.CustomPartitioningSupported {
		return nil, fmt.Errorf("OS %s (%s) does not support custom partitioning", osInfo.Name, osInfo.ID)
	}

	partitioningSchema, err := buildInstallPartitioningSchema(ProvisionParams{
		PivotOSDisk:              params.PivotOSDisk,
		PivotDataDisk:            params.PivotDataDisk,
		SkipDataDiskPartitioning: params.SkipDataDiskPartitioning,
	})
	if err != nil {
		return nil, fmt.Errorf("build install partitioning schema: %w", err)
	}

	hostname := strings.TrimSpace(params.Hostname)
	if hostname == "" {
		hostname = "talos-pivot"
	}

	cloudInitFile := installUserDataFile([]byte(params.CloudInitScript))
	server, err := client.Baremetal.InstallServer(&baremetal.InstallServerRequest{
		Zone:               params.Zone,
		ServerID:           serverID,
		OsID:               params.OSID,
		Hostname:           hostname,
		SSHKeyIDs:          sshKeyIDs,
		UserData:           cloudInitFile,
		PartitioningSchema: partitioningSchema,
	}, scw.WithContext(ctx))
	if err != nil {
		return nil, fmt.Errorf("install server %s: %w", serverID, err)
	}

	slog.Info("server reinstall triggered with OS install",
		"server_id", serverID,
		"os", params.OSID,
		"zone", params.Zone,
	)

	return server, nil
}

func installUserDataFile(cloudInitBytes []byte) *scw.File {
	// InstallServer sends user_data through a protobuf-backed JSON field where
	// nested bytes content must be base64-encoded.
	encoded := base64.StdEncoding.EncodeToString(cloudInitBytes)
	return &scw.File{
		Name:        "user-data",
		ContentType: "text/plain",
		Content:     strings.NewReader(encoded),
	}
}

// EnsureServerPrivateNetworkAttachment attaches a server to the target private network if needed.

func listSSHKeyIDs(iamAPI *iam.API) ([]string, error) {
	resp, err := iamAPI.ListSSHKeys(&iam.ListSSHKeysRequest{})
	if err != nil {
		return nil, fmt.Errorf("list ssh keys: %w", err)
	}
	if len(resp.SSHKeys) == 0 {
		return nil, fmt.Errorf("no SSH keys found in Scaleway org")
	}
	ids := make([]string, len(resp.SSHKeys))
	for i, key := range resp.SSHKeys {
		ids[i] = key.ID
	}
	return ids, nil
}
