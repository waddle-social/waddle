package cmd

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
	scw "github.com/scaleway/scaleway-sdk-go/scw"
	"github.com/spf13/cobra"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/operation"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/scaleway"
)

func restoreProvisioningFns() {
	scalewayNewClientFn = scaleway.NewClient
	scalewayResolveOfferForBillingCycleFn = scaleway.ResolveOfferForBillingCycle
	scalewayResolveUbuntuOSIDFn = scaleway.ResolveUbuntuOSID
	scalewayOrderServerFn = scaleway.OrderServer
	scalewayReinstallServerFn = scaleway.ReinstallServer
	scalewayWaitForReadyFn = scaleway.WaitForReady
	scalewayEnsureReservedPrivateNetworkIPFn = scaleway.EnsureReservedPrivateNetworkIP
	scalewayEnsureServerPrivateNetworkFn = scaleway.EnsureServerPrivateNetworkAttachment
	scalewayGetServerFn = func(ctx context.Context, client *scaleway.Client, zone scw.Zone, serverID string) (*baremetal.Server, error) {
		return client.Baremetal.GetServer(&baremetal.GetServerRequest{
			Zone:     zone,
			ServerID: serverID,
		}, scw.WithContext(ctx))
	}
	scalewayEnsureNetworkFoundationFn = scaleway.EnsureNetworkFoundation
	scalewayEnsureServerNameFn = ensureServerName
	scalewayEnsureManagedServerTagsFn = ensureManagedServerTags
}

func newNodePool(name, role string) config.NodePoolConfig {
	return config.NodePoolConfig{
		Name:         name,
		Type:         role,
		Zone:         string(scw.ZoneFrPar1),
		Offer:        "EM-A610R-NVMe",
		BillingCycle: "hourly",
		Disks: config.DiskConfig{
			OS:   "/dev/nvme0n1",
			Data: "/dev/nvme1n1",
		},
	}
}

func newProvisionConfig(pool config.NodePoolConfig) *config.Config {
	return &config.Config{
		Environment: "production",
		Cluster: config.ClusterConfig{
			TalosVersion:      "v1.12.0",
			KubernetesVersion: "v1.35.0",
			TalosSchematic:    "schematic-id",
		},
		Scaleway: config.ScalewayConfig{
			ProjectID:              "project-id",
			OrganizationID:         "org-id",
			PrivateNetworkIPv4CIDR: "172.16.16.0/24",
		},
		NodePools: []config.NodePoolConfig{pool},
	}
}

func newClusterCreateValidationCmd(serverID string, confirm bool) *cobra.Command {
	cmd := &cobra.Command{}
	cmd.Flags().StringP("environment", "e", "", "")
	cmd.Flags().StringP("file", "f", "", "")
	cmd.Flags().String("node-name", "", "")
	cmd.Flags().String("pool", "", "")
	cmd.Flags().String("netbird-secret-path", "", "")
	cmd.Flags().String("netbird-secret-key", "", "")
	cmd.Flags().String("server-id", "", "")
	cmd.Flags().Bool("confirm-reinstall", false, "")
	_ = cmd.Flags().Set("server-id", serverID)
	if confirm {
		_ = cmd.Flags().Set("confirm-reinstall", "true")
	}
	return cmd
}

func newNodeAddValidationCmd(serverID string, confirm bool) *cobra.Command {
	cmd := &cobra.Command{}
	cmd.Flags().String("name", "", "")
	cmd.Flags().String("role", "worker", "")
	cmd.Flags().String("cluster", "", "")
	cmd.Flags().StringP("file", "f", "", "")
	cmd.Flags().String("pool", "", "")
	cmd.Flags().String("server-id", "", "")
	cmd.Flags().Bool("confirm-reinstall", false, "")
	_ = cmd.Flags().Set("server-id", serverID)
	if confirm {
		_ = cmd.Flags().Set("confirm-reinstall", "true")
	}
	return cmd
}

func TestRunClusterCreateValidatesReinstallFlags(t *testing.T) {
	err := runClusterCreate(newClusterCreateValidationCmd("srv-123", false), nil)
	if err == nil || !strings.Contains(err.Error(), "--server-id requires --confirm-reinstall") {
		t.Fatalf("expected server-id validation error, got %v", err)
	}

	err = runClusterCreate(newClusterCreateValidationCmd("", true), nil)
	if err == nil || !strings.Contains(err.Error(), "--confirm-reinstall requires --server-id") {
		t.Fatalf("expected confirm-reinstall validation error, got %v", err)
	}
}

func TestRunNodeAddValidatesReinstallFlags(t *testing.T) {
	err := runNodeAdd(newNodeAddValidationCmd("srv-123", false), nil)
	if err == nil || !strings.Contains(err.Error(), "--server-id requires --confirm-reinstall") {
		t.Fatalf("expected server-id validation error, got %v", err)
	}

	err = runNodeAdd(newNodeAddValidationCmd("", true), nil)
	if err == nil || !strings.Contains(err.Error(), "--confirm-reinstall requires --server-id") {
		t.Fatalf("expected confirm-reinstall validation error, got %v", err)
	}
}

func TestRunClusterCreateAcceptsValidReinstallFlags(t *testing.T) {
	err := runClusterCreate(newClusterCreateValidationCmd("srv-123", true), nil)
	if err == nil {
		t.Fatal("expected config resolution error, got nil")
	}
	if strings.Contains(err.Error(), "requires --confirm-reinstall") {
		t.Fatalf("did not expect reinstall validation error, got %v", err)
	}
}

func TestRunNodeAddAcceptsValidReinstallFlags(t *testing.T) {
	err := runNodeAdd(newNodeAddValidationCmd("srv-123", true), nil)
	if err == nil {
		t.Fatal("expected config resolution error, got nil")
	}
	if strings.Contains(err.Error(), "requires --confirm-reinstall") {
		t.Fatalf("did not expect reinstall validation error, got %v", err)
	}
}

func TestPhaseOrderServerReinstallUsesExistingServer(t *testing.T) {
	restoreProvisioningFns()
	t.Cleanup(restoreProvisioningFns)

	cfg := newProvisionConfig(newNodePool("control-plane", config.NodeTypeControlPlane))
	op := operation.New("op-test", operation.TypeCreateCluster, cfg.Environment, []string{"order-server"})
	op.SetContext("poolName", "control-plane")
	op.SetContext("role", config.NodeTypeControlPlane)
	op.SetContext("serverId", "srv-existing")
	op.SetContext(opContextReinstall, true)
	op.SetContext("privateIP", "172.16.16.16")

	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayEnsureNetworkFoundationFn = func(_ context.Context, _ *scaleway.Client, params scaleway.NetworkFoundationParams) (*scaleway.NetworkFoundation, error) {
		if !params.AllowCIDRReplacement {
			t.Fatal("expected CIDR replacement to be enabled for reinstall path")
		}
		if params.PrivateNetworkIPv4CIDR != "172.16.16.0/24" {
			t.Fatalf("private network cidr = %q, want %q", params.PrivateNetworkIPv4CIDR, "172.16.16.0/24")
		}
		if params.ReplacementServerID != "srv-existing" {
			t.Fatalf("replacement server id = %q, want %q", params.ReplacementServerID, "srv-existing")
		}
		return &scaleway.NetworkFoundation{PrivateNetworkID: "pn-123"}, nil
	}

	scalewayResolveOfferForBillingCycleFn = func(context.Context, *scaleway.Client, scw.Zone, string, string) (string, baremetal.OfferSubscriptionPeriod, error) {
		t.Fatal("resolve offer should not run for reinstall path")
		return "", baremetal.OfferSubscriptionPeriodUnknownSubscriptionPeriod, nil
	}

	scalewayGetServerFn = func(context.Context, *scaleway.Client, scw.Zone, string) (*baremetal.Server, error) {
		return &baremetal.Server{
			ID:      "srv-existing",
			Name:    "legacy-control-plane-name",
			OfferID: "offer-existing",
			Tags:    []string{"owner=legacy"},
		}, nil
	}

	var gotResolveOfferID string
	scalewayResolveUbuntuOSIDFn = func(_ context.Context, _ *scaleway.Client, _ scw.Zone, offerID string) (string, error) {
		gotResolveOfferID = offerID
		return "ubuntu-os", nil
	}

	var gotAttachReservedIP string
	var gotEnsuredReservedIP string
	var gotRenamedServerName string
	scalewayEnsureReservedPrivateNetworkIPFn = func(_ context.Context, _ *scaleway.Client, _ scw.Zone, serverID, privateNetworkID, reservedPrivateIP string) error {
		if serverID != "srv-existing" {
			t.Fatalf("ensure reserved IP server id = %q, want %q", serverID, "srv-existing")
		}
		if privateNetworkID != "pn-123" {
			t.Fatalf("ensure reserved IP private network id = %q, want %q", privateNetworkID, "pn-123")
		}
		gotEnsuredReservedIP = reservedPrivateIP
		return nil
	}
	scalewayEnsureServerPrivateNetworkFn = func(_ context.Context, _ *scaleway.Client, _ scw.Zone, serverID, privateNetworkID, reservedPrivateIP string) error {
		if serverID != "srv-existing" {
			t.Fatalf("attach server id = %q, want %q", serverID, "srv-existing")
		}
		if privateNetworkID != "pn-123" {
			t.Fatalf("attach private network id = %q, want %q", privateNetworkID, "pn-123")
		}
		gotAttachReservedIP = reservedPrivateIP
		return nil
	}
	scalewayEnsureServerNameFn = func(_ context.Context, _ *scaleway.Client, _ scw.Zone, server *baremetal.Server, desiredName string) (*baremetal.Server, error) {
		if server == nil || server.ID != "srv-existing" {
			t.Fatalf("rename server = %#v, want server ID %q", server, "srv-existing")
		}
		gotRenamedServerName = desiredName
		server.Name = desiredName
		return server, nil
	}

	var reinstallCalled bool
	scalewayReinstallServerFn = func(_ context.Context, _ *scaleway.Client, params scaleway.ReinstallParams) (*baremetal.Server, error) {
		reinstallCalled = true
		if params.ServerID != "srv-existing" {
			t.Fatalf("reinstall server id = %q, want %q", params.ServerID, "srv-existing")
		}
		return &baremetal.Server{
			ID:   "srv-existing",
			Name: "legacy-control-plane-name",
			Tags: []string{"owner=legacy"},
		}, nil
	}

	scalewayOrderServerFn = func(context.Context, *scaleway.Client, scaleway.ProvisionParams) (*baremetal.Server, error) {
		t.Fatal("order server should not run for reinstall path")
		return nil, nil
	}

	var gotTagsRole string
	scalewayEnsureManagedServerTagsFn = func(_ context.Context, _ *scaleway.Client, _ scw.Zone, server *baremetal.Server, environment, poolName, role string) (*baremetal.Server, error) {
		gotTagsRole = role
		return server, nil
	}

	if err := phaseOrderServer(context.Background(), op, cfg); err != nil {
		t.Fatalf("phaseOrderServer returned error: %v", err)
	}
	if !reinstallCalled {
		t.Fatal("expected reinstall to be called")
	}
	if gotResolveOfferID != "offer-existing" {
		t.Fatalf("resolve ubuntu offer id = %q, want %q", gotResolveOfferID, "offer-existing")
	}
	if gotAttachReservedIP != "172.16.16.16" {
		t.Fatalf("reserved private ip = %q, want %q", gotAttachReservedIP, "172.16.16.16")
	}
	if gotEnsuredReservedIP != "172.16.16.16" {
		t.Fatalf("ensured reserved private ip = %q, want %q", gotEnsuredReservedIP, "172.16.16.16")
	}
	if gotRenamedServerName != "production-control-plane-01" {
		t.Fatalf("renamed server name = %q, want %q", gotRenamedServerName, "production-control-plane-01")
	}
	if got := op.GetContextString("nodeName"); got != "production-control-plane-01" {
		t.Fatalf("operation nodeName = %q, want %q", got, "production-control-plane-01")
	}
	if gotTagsRole != config.NodeTypeControlPlane {
		t.Fatalf("managed tags role = %q, want %q", gotTagsRole, config.NodeTypeControlPlane)
	}
}

func TestPhaseOrderServerOrderPathUnaffected(t *testing.T) {
	restoreProvisioningFns()
	t.Cleanup(restoreProvisioningFns)

	cfg := newProvisionConfig(newNodePool("control-plane", config.NodeTypeControlPlane))
	op := operation.New("op-test", operation.TypeCreateCluster, cfg.Environment, []string{"order-server"})
	op.SetContext("poolName", "control-plane")
	op.SetContext("role", config.NodeTypeControlPlane)
	op.SetContext("nodeName", "production-control-plane-01")

	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayEnsureNetworkFoundationFn = func(_ context.Context, _ *scaleway.Client, params scaleway.NetworkFoundationParams) (*scaleway.NetworkFoundation, error) {
		if params.AllowCIDRReplacement {
			t.Fatal("did not expect CIDR replacement for order path")
		}
		return &scaleway.NetworkFoundation{PrivateNetworkID: "pn-123"}, nil
	}
	scalewayResolveOfferForBillingCycleFn = func(context.Context, *scaleway.Client, scw.Zone, string, string) (string, baremetal.OfferSubscriptionPeriod, error) {
		return "offer-1", baremetal.OfferSubscriptionPeriodHourly, nil
	}
	scalewayResolveUbuntuOSIDFn = func(context.Context, *scaleway.Client, scw.Zone, string) (string, error) {
		return "ubuntu-os", nil
	}
	scalewayEnsureReservedPrivateNetworkIPFn = func(context.Context, *scaleway.Client, scw.Zone, string, string, string) error {
		t.Fatal("reserved IP ensure should not run for order path")
		return nil
	}

	var orderCalled bool
	scalewayOrderServerFn = func(_ context.Context, _ *scaleway.Client, _ scaleway.ProvisionParams) (*baremetal.Server, error) {
		orderCalled = true
		return &baremetal.Server{ID: "srv-ordered", Name: "production-control-plane-01"}, nil
	}
	scalewayReinstallServerFn = func(context.Context, *scaleway.Client, scaleway.ReinstallParams) (*baremetal.Server, error) {
		t.Fatal("reinstall should not run for order path")
		return nil, nil
	}
	scalewayEnsureManagedServerTagsFn = func(_ context.Context, _ *scaleway.Client, _ scw.Zone, server *baremetal.Server, _, _, _ string) (*baremetal.Server, error) {
		return server, nil
	}

	if err := phaseOrderServer(context.Background(), op, cfg); err != nil {
		t.Fatalf("phaseOrderServer returned error: %v", err)
	}
	if !orderCalled {
		t.Fatal("expected order server to be called")
	}
	if got := op.GetContextString("serverId"); got != "srv-ordered" {
		t.Fatalf("operation serverId = %q, want %q", got, "srv-ordered")
	}
}

func TestProvisionNodeServerReinstallWorker(t *testing.T) {
	restoreProvisioningFns()
	t.Cleanup(restoreProvisioningFns)

	cfg := newProvisionConfig(newNodePool("workers", config.NodeTypeWorker))

	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayGetServerFn = func(context.Context, *scaleway.Client, scw.Zone, string) (*baremetal.Server, error) {
		return &baremetal.Server{ID: "srv-worker", Name: "legacy-worker-node", OfferID: "offer-existing"}, nil
	}
	scalewayResolveUbuntuOSIDFn = func(context.Context, *scaleway.Client, scw.Zone, string) (string, error) {
		return "ubuntu-os", nil
	}
	scalewayEnsureNetworkFoundationFn = func(_ context.Context, _ *scaleway.Client, params scaleway.NetworkFoundationParams) (*scaleway.NetworkFoundation, error) {
		if !params.AllowCIDRReplacement {
			t.Fatal("expected CIDR replacement for worker reinstall path")
		}
		if params.ReplacementServerID != "srv-worker" {
			t.Fatalf("replacement server id = %q, want %q", params.ReplacementServerID, "srv-worker")
		}
		return &scaleway.NetworkFoundation{PrivateNetworkID: "pn-123"}, nil
	}

	var gotAttachReservedIP string
	var gotRenamedServerName string
	scalewayEnsureReservedPrivateNetworkIPFn = func(context.Context, *scaleway.Client, scw.Zone, string, string, string) error {
		t.Fatal("reserved IP ensure should not run for worker reinstall without private IP")
		return nil
	}
	scalewayEnsureServerPrivateNetworkFn = func(context.Context, *scaleway.Client, scw.Zone, string, string, string) error {
		gotAttachReservedIP = ""
		return nil
	}
	scalewayEnsureServerNameFn = func(_ context.Context, _ *scaleway.Client, _ scw.Zone, server *baremetal.Server, desiredName string) (*baremetal.Server, error) {
		gotRenamedServerName = desiredName
		server.Name = desiredName
		return server, nil
	}
	scalewayReinstallServerFn = func(context.Context, *scaleway.Client, scaleway.ReinstallParams) (*baremetal.Server, error) {
		return &baremetal.Server{ID: "srv-worker"}, nil
	}
	scalewayEnsureManagedServerTagsFn = func(_ context.Context, _ *scaleway.Client, _ scw.Zone, server *baremetal.Server, _, _, _ string) (*baremetal.Server, error) {
		return server, nil
	}
	scalewayWaitForReadyFn = func(context.Context, *scaleway.Client, string, scw.Zone) (*baremetal.Server, error) {
		return &baremetal.Server{ID: "srv-worker"}, nil
	}
	scalewayOrderServerFn = func(context.Context, *scaleway.Client, scaleway.ProvisionParams) (*baremetal.Server, error) {
		t.Fatal("order server should not run when reinstalling existing worker")
		return nil, nil
	}

	_, nodeName, err := provisionNodeServer(
		context.Background(),
		cfg,
		&cfg.NodePools[0],
		config.NodeTypeWorker,
		"production-workers-01",
		"srv-worker",
		"",
	)
	if err != nil {
		t.Fatalf("provisionNodeServer returned error: %v", err)
	}
	if nodeName != "production-workers-01" {
		t.Fatalf("node name = %q, want %q", nodeName, "production-workers-01")
	}
	if gotRenamedServerName != "production-workers-01" {
		t.Fatalf("renamed server name = %q, want %q", gotRenamedServerName, "production-workers-01")
	}
	if gotAttachReservedIP != "" {
		t.Fatalf("worker reserved private ip = %q, want empty", gotAttachReservedIP)
	}
}

func TestProvisionNodeServerReinstallControlPlaneUsesReservedIP(t *testing.T) {
	restoreProvisioningFns()
	t.Cleanup(restoreProvisioningFns)

	cfg := newProvisionConfig(newNodePool("control-plane", config.NodeTypeControlPlane))

	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayGetServerFn = func(context.Context, *scaleway.Client, scw.Zone, string) (*baremetal.Server, error) {
		return &baremetal.Server{ID: "srv-cp", Name: "legacy-cp-node", OfferID: "offer-existing"}, nil
	}
	scalewayResolveUbuntuOSIDFn = func(context.Context, *scaleway.Client, scw.Zone, string) (string, error) {
		return "ubuntu-os", nil
	}
	scalewayEnsureNetworkFoundationFn = func(_ context.Context, _ *scaleway.Client, params scaleway.NetworkFoundationParams) (*scaleway.NetworkFoundation, error) {
		if !params.AllowCIDRReplacement {
			t.Fatal("expected CIDR replacement for control-plane reinstall path")
		}
		if params.ReplacementServerID != "srv-cp" {
			t.Fatalf("replacement server id = %q, want %q", params.ReplacementServerID, "srv-cp")
		}
		return &scaleway.NetworkFoundation{PrivateNetworkID: "pn-123"}, nil
	}

	var gotAttachReservedIP string
	var gotEnsuredReservedIP string
	var gotRenamedServerName string
	scalewayEnsureReservedPrivateNetworkIPFn = func(_ context.Context, _ *scaleway.Client, _ scw.Zone, serverID, privateNetworkID, reservedPrivateIP string) error {
		if serverID != "srv-cp" {
			t.Fatalf("ensure reserved IP server id = %q, want %q", serverID, "srv-cp")
		}
		if privateNetworkID != "pn-123" {
			t.Fatalf("ensure reserved IP private network id = %q, want %q", privateNetworkID, "pn-123")
		}
		gotEnsuredReservedIP = reservedPrivateIP
		return nil
	}
	scalewayEnsureServerPrivateNetworkFn = func(_ context.Context, _ *scaleway.Client, _ scw.Zone, _ string, _ string, reservedPrivateIP string) error {
		gotAttachReservedIP = reservedPrivateIP
		return nil
	}
	scalewayEnsureServerNameFn = func(_ context.Context, _ *scaleway.Client, _ scw.Zone, server *baremetal.Server, desiredName string) (*baremetal.Server, error) {
		gotRenamedServerName = desiredName
		server.Name = desiredName
		return server, nil
	}
	scalewayReinstallServerFn = func(context.Context, *scaleway.Client, scaleway.ReinstallParams) (*baremetal.Server, error) {
		return &baremetal.Server{ID: "srv-cp"}, nil
	}
	scalewayEnsureManagedServerTagsFn = func(_ context.Context, _ *scaleway.Client, _ scw.Zone, server *baremetal.Server, _, _, _ string) (*baremetal.Server, error) {
		return server, nil
	}
	scalewayWaitForReadyFn = func(context.Context, *scaleway.Client, string, scw.Zone) (*baremetal.Server, error) {
		return &baremetal.Server{ID: "srv-cp"}, nil
	}

	_, _, err := provisionNodeServer(
		context.Background(),
		cfg,
		&cfg.NodePools[0],
		config.NodeTypeControlPlane,
		"production-control-plane-02",
		"srv-cp",
		"172.16.16.17",
	)
	if err != nil {
		t.Fatalf("provisionNodeServer returned error: %v", err)
	}
	if gotAttachReservedIP != "172.16.16.17" {
		t.Fatalf("reserved private ip = %q, want %q", gotAttachReservedIP, "172.16.16.17")
	}
	if gotEnsuredReservedIP != "172.16.16.17" {
		t.Fatalf("ensured reserved private ip = %q, want %q", gotEnsuredReservedIP, "172.16.16.17")
	}
	if gotRenamedServerName != "production-control-plane-02" {
		t.Fatalf("renamed server name = %q, want %q", gotRenamedServerName, "production-control-plane-02")
	}
}

func TestProvisionNodeServerKeepsOrderPathBehavior(t *testing.T) {
	restoreProvisioningFns()
	t.Cleanup(restoreProvisioningFns)

	cfg := newProvisionConfig(newNodePool("workers", config.NodeTypeWorker))

	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayResolveOfferForBillingCycleFn = func(context.Context, *scaleway.Client, scw.Zone, string, string) (string, baremetal.OfferSubscriptionPeriod, error) {
		return "offer-1", baremetal.OfferSubscriptionPeriodHourly, nil
	}
	scalewayResolveUbuntuOSIDFn = func(context.Context, *scaleway.Client, scw.Zone, string) (string, error) {
		return "ubuntu-os", nil
	}
	scalewayEnsureNetworkFoundationFn = func(context.Context, *scaleway.Client, scaleway.NetworkFoundationParams) (*scaleway.NetworkFoundation, error) {
		return &scaleway.NetworkFoundation{PrivateNetworkID: "pn-123"}, nil
	}
	scalewayEnsureReservedPrivateNetworkIPFn = func(context.Context, *scaleway.Client, scw.Zone, string, string, string) error {
		t.Fatal("reserved IP ensure should not run for order path")
		return nil
	}
	scalewayReinstallServerFn = func(context.Context, *scaleway.Client, scaleway.ReinstallParams) (*baremetal.Server, error) {
		t.Fatal("reinstall should not run for order path")
		return nil, nil
	}

	orderCalled := false
	scalewayOrderServerFn = func(context.Context, *scaleway.Client, scaleway.ProvisionParams) (*baremetal.Server, error) {
		orderCalled = true
		return &baremetal.Server{ID: "srv-new", Name: "production-workers-01"}, nil
	}
	scalewayEnsureManagedServerTagsFn = func(_ context.Context, _ *scaleway.Client, _ scw.Zone, server *baremetal.Server, _, _, _ string) (*baremetal.Server, error) {
		return server, nil
	}
	scalewayWaitForReadyFn = func(context.Context, *scaleway.Client, string, scw.Zone) (*baremetal.Server, error) {
		return &baremetal.Server{ID: "srv-new"}, nil
	}

	_, nodeName, err := provisionNodeServer(
		context.Background(),
		cfg,
		&cfg.NodePools[0],
		config.NodeTypeWorker,
		"production-workers-01",
		"",
		"",
	)
	if err != nil {
		t.Fatalf("provisionNodeServer returned error: %v", err)
	}
	if !orderCalled {
		t.Fatal("expected order server path to be used")
	}
	if nodeName != "production-workers-01" {
		t.Fatalf("node name = %q, want %q", nodeName, "production-workers-01")
	}
}

func TestProvisionNodeServerReinstallRequiresOfferID(t *testing.T) {
	restoreProvisioningFns()
	t.Cleanup(restoreProvisioningFns)

	cfg := newProvisionConfig(newNodePool("workers", config.NodeTypeWorker))

	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayGetServerFn = func(context.Context, *scaleway.Client, scw.Zone, string) (*baremetal.Server, error) {
		return &baremetal.Server{ID: "srv-worker", Name: "legacy-worker-node", OfferID: ""}, nil
	}

	_, _, err := provisionNodeServer(
		context.Background(),
		cfg,
		&cfg.NodePools[0],
		config.NodeTypeWorker,
		"production-workers-01",
		"srv-worker",
		"",
	)
	if err == nil {
		t.Fatal("expected missing offer ID error, got nil")
	}
	if !strings.Contains(err.Error(), "empty offer ID") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestPhaseOrderServerReinstallRequiresServerID(t *testing.T) {
	restoreProvisioningFns()
	t.Cleanup(restoreProvisioningFns)

	cfg := newProvisionConfig(newNodePool("control-plane", config.NodeTypeControlPlane))
	op := operation.New("op-test", operation.TypeCreateCluster, cfg.Environment, []string{"order-server"})
	op.SetContext("poolName", "control-plane")
	op.SetContext("role", config.NodeTypeControlPlane)
	op.SetContext(opContextReinstall, true)

	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}

	err := phaseOrderServer(context.Background(), op, cfg)
	if err == nil {
		t.Fatal("expected missing server ID error, got nil")
	}
	if !strings.Contains(err.Error(), "missing server ID") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestRunNodeAddValidationRunsBeforeConfigLoad(t *testing.T) {
	cmd := newNodeAddValidationCmd("srv-123", false)
	err := runNodeAdd(cmd, nil)
	if err == nil {
		t.Fatal("expected reinstall validation error, got nil")
	}
	if strings.Contains(err.Error(), "either --file or --cluster is required") {
		t.Fatalf("expected validation to run before config load, got %v", err)
	}
}

func TestRunClusterCreateValidationRunsBeforeConfigLoad(t *testing.T) {
	cmd := newClusterCreateValidationCmd("srv-123", false)
	err := runClusterCreate(cmd, nil)
	if err == nil {
		t.Fatal("expected reinstall validation error, got nil")
	}
	if strings.Contains(err.Error(), "either --file or --cluster is required") {
		t.Fatalf("expected validation to run before config load, got %v", err)
	}
}

func TestProvisionNodeServerSurfaceReinstallError(t *testing.T) {
	restoreProvisioningFns()
	t.Cleanup(restoreProvisioningFns)

	cfg := newProvisionConfig(newNodePool("workers", config.NodeTypeWorker))
	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayGetServerFn = func(context.Context, *scaleway.Client, scw.Zone, string) (*baremetal.Server, error) {
		return &baremetal.Server{ID: "srv-worker", Name: "legacy-worker-node", OfferID: "offer-existing"}, nil
	}
	scalewayResolveUbuntuOSIDFn = func(context.Context, *scaleway.Client, scw.Zone, string) (string, error) {
		return "ubuntu-os", nil
	}
	scalewayEnsureNetworkFoundationFn = func(context.Context, *scaleway.Client, scaleway.NetworkFoundationParams) (*scaleway.NetworkFoundation, error) {
		return &scaleway.NetworkFoundation{PrivateNetworkID: "pn-123"}, nil
	}
	scalewayEnsureServerPrivateNetworkFn = func(context.Context, *scaleway.Client, scw.Zone, string, string, string) error {
		return nil
	}
	scalewayReinstallServerFn = func(context.Context, *scaleway.Client, scaleway.ReinstallParams) (*baremetal.Server, error) {
		return nil, errors.New("reinstall failed")
	}

	_, _, err := provisionNodeServer(context.Background(), cfg, &cfg.NodePools[0], config.NodeTypeWorker, "production-workers-01", "srv-worker", "")
	if err == nil {
		t.Fatal("expected reinstall error, got nil")
	}
	if !strings.Contains(err.Error(), "reinstall server") {
		t.Fatalf("unexpected error: %v", err)
	}
}
