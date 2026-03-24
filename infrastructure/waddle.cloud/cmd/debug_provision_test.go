package cmd

import (
	"context"
	"strings"
	"testing"

	"github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
	scw "github.com/scaleway/scaleway-sdk-go/scw"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/operation"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/scaleway"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/talos"
)

func TestProvisionCommandsExposeDebugProvisionFlag(t *testing.T) {
	if clusterCreateCmd.Flags().Lookup("debug-provision") == nil {
		t.Fatal("cluster create is missing --debug-provision")
	}
	if nodeAddCmd.Flags().Lookup("debug-provision") == nil {
		t.Fatal("node add is missing --debug-provision")
	}
}

func TestEnhanceTalosMaintenanceWaitErrorIncludesInspectHints(t *testing.T) {
	err := enhanceTalosMaintenanceWaitError(&talos.MaintenanceTimeoutError{
		Target:  "203.0.113.50:50000",
		Timeout: 30,
	}, true)
	if err == nil {
		t.Fatal("expected wrapped timeout error, got nil")
	}
	for _, expected := range []string{
		"Scaleway remote console",
		"/var/log/cloud-init-output.log",
		"/var/log/waddle-cloud-pivot.log",
	} {
		if !strings.Contains(err.Error(), expected) {
			t.Fatalf("expected %q in error, got %v", expected, err)
		}
	}
}

func TestPhaseOrderServerDebugProvisionBuildsVerboseCloudInit(t *testing.T) {
	restoreProvisioningFns()
	t.Cleanup(restoreProvisioningFns)

	cfg := newProvisionConfig(newNodePool("control-plane", config.NodeTypeControlPlane))
	op, _ := operation.New("op-debug", operation.TypeCreateCluster, cfg.Environment, []string{"order-server"})
	op.SetContext("poolName", "control-plane")
	op.SetContext("role", config.NodeTypeControlPlane)
	op.SetContext("serverId", "srv-existing")
	op.SetContext(opContextReinstall, true)
	op.SetContext(opContextDebugProvision, true)
	op.SetContext("privateIP", "172.16.16.16")

	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayEnsureNetworkFoundationFn = func(context.Context, *scaleway.Client, scaleway.NetworkFoundationParams) (*scaleway.NetworkFoundation, error) {
		return &scaleway.NetworkFoundation{PrivateNetworkID: "pn-123"}, nil
	}
	scalewayGetServerFn = func(context.Context, *scaleway.Client, scw.Zone, string) (*baremetal.Server, error) {
		return &baremetal.Server{ID: "srv-existing", OfferID: "offer-existing"}, nil
	}
	scalewayResolveUbuntuOSIDFn = func(context.Context, *scaleway.Client, scw.Zone, string) (string, error) {
		return "ubuntu-os", nil
	}
	scalewayEnsureReservedPrivateNetworkIPFn = func(context.Context, *scaleway.Client, scw.Zone, string, string, string) error {
		return nil
	}
	scalewayEnsureServerPrivateNetworkFn = func(context.Context, *scaleway.Client, scw.Zone, string, string, string) error {
		return nil
	}
	scalewayEnsureServerNameFn = func(_ context.Context, _ *scaleway.Client, _ scw.Zone, server *baremetal.Server, desiredName string) (*baremetal.Server, error) {
		server.Name = desiredName
		return server, nil
	}
	scalewayEnsureManagedServerTagsFn = func(_ context.Context, _ *scaleway.Client, _ scw.Zone, server *baremetal.Server, _, _, _ string) (*baremetal.Server, error) {
		return server, nil
	}

	var gotCloudInit string
	scalewayReinstallServerFn = func(_ context.Context, _ *scaleway.Client, params scaleway.ReinstallParams) (*baremetal.Server, error) {
		gotCloudInit = params.CloudInitScript
		return &baremetal.Server{ID: "srv-existing"}, nil
	}

	if err := phaseOrderServer(context.Background(), op, cfg); err != nil {
		t.Fatalf("phaseOrderServer returned error: %v", err)
	}
	for _, expected := range []string{
		"/var/log/cloud-init-output.log",
		"/var/log/waddle-cloud-pivot.log",
		"/dev/console",
		"set -x",
	} {
		if !strings.Contains(gotCloudInit, expected) {
			t.Fatalf("expected %q in cloud-init, got:\n%s", expected, gotCloudInit)
		}
	}
}

func TestProvisionNodeServerDebugProvisionBuildsVerboseCloudInit(t *testing.T) {
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
	scalewayEnsureManagedServerTagsFn = func(_ context.Context, _ *scaleway.Client, _ scw.Zone, server *baremetal.Server, _, _, _ string) (*baremetal.Server, error) {
		return server, nil
	}
	scalewayWaitForReadyFn = func(context.Context, *scaleway.Client, string, scw.Zone) (*baremetal.Server, error) {
		return &baremetal.Server{ID: "srv-new"}, nil
	}

	var gotCloudInit string
	scalewayOrderServerFn = func(_ context.Context, _ *scaleway.Client, params scaleway.ProvisionParams) (*baremetal.Server, error) {
		gotCloudInit = params.CloudInitScript
		return &baremetal.Server{ID: "srv-new", Name: "production-workers-01"}, nil
	}

	if _, _, err := provisionNodeServer(
		context.Background(),
		cfg,
		&cfg.NodePools[0],
		config.NodeTypeWorker,
		"production-workers-01",
		"",
		"",
		true,
	); err != nil {
		t.Fatalf("provisionNodeServer returned error: %v", err)
	}

	for _, expected := range []string{
		"/var/log/cloud-init-output.log",
		"/var/log/waddle-cloud-pivot.log",
		"/dev/console",
		"pivot_failed",
	} {
		if !strings.Contains(gotCloudInit, expected) {
			t.Fatalf("expected %q in cloud-init, got:\n%s", expected, gotCloudInit)
		}
	}
}
