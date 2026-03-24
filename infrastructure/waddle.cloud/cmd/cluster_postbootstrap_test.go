package cmd

import (
	"context"
	"errors"
	"net"
	"strings"
	"testing"
	"time"

	"github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
	scw "github.com/scaleway/scaleway-sdk-go/scw"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/cilium"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/flux"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/operation"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/scaleway"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/storage"
)

func restorePostBootstrapFns() {
	ciliumInstallFn = cilium.Install
	fluxBootstrapFn = flux.Bootstrap
	postBootstrapIngressGatewaySyncFn = syncIngressGatewayTarget
	postBootstrapIngressGatewaySyncInterval = 5 * time.Second
	postBootstrapIngressGatewaySyncTimeout = 10 * time.Minute
	postBootstrapKubeconfigPathFn = func(context.Context, *operation.Operation, *config.Config) (string, func(), error) {
		return "", func() {}, nil
	}
	postBootstrapKubeconfigRetryInterval = 15 * time.Second
	postBootstrapKubeconfigRetryTimeout = 30 * time.Minute
	postBootstrapKubernetesAPIProbeFn = postBootstrapKubernetesAPIReachable
	postBootstrapKubernetesAPIWaitFn = func(context.Context, string) error {
		return nil
	}
	postBootstrapKubernetesAPIRetryInterval = 5 * time.Second
	postBootstrapKubernetesAPIRetryTimeout = 10 * time.Minute
	storagePrepareOpenEBSMayastorFn = storage.PrepareOpenEBSMayastorRawDisk
	postBootstrapBootstrapSecretsFn = prepareBootstrapSecrets
	scalewayNewClientFn = scaleway.NewClient
	scalewayGetServerFn = func(ctx context.Context, client *scaleway.Client, zone scw.Zone, serverID string) (*baremetal.Server, error) {
		return client.Baremetal.GetServer(&baremetal.GetServerRequest{
			Zone:     zone,
			ServerID: serverID,
		}, scw.WithContext(ctx))
	}
	scalewayEnsureNetworkFoundationFn = scaleway.EnsureNetworkFoundation
	scalewayResolvePrivateNetworkIPv4CIDRFn = scaleway.ResolvePrivateNetworkIPv4CIDR
}

func newPostBootstrapOperation() *operation.Operation {
	op, _ := operation.New("op-test", operation.TypeCreateCluster, "production", []string{"post-bootstrap"})
	op.SetContext("zone", string(scw.ZoneFrPar1))
	op.SetContext("privateNetworkID", "pn-123")
	op.SetContext("publicIP", "51.159.1.2")
	op.SetContext("privateIP", "172.16.16.16")
	op.SetContext("nodeName", "production-control-plane-01")
	return op
}

func TestPhasePostBootstrapStopsBeforeFluxWhenCiliumFails(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	errCilium := errors.New("cilium failed")

	ciliumInstallFn = func(context.Context, cilium.InstallParams) error {
		return errCilium
	}
	fluxBootstrapFn = func(context.Context, flux.BootstrapParams) error {
		t.Fatal("did not expect flux bootstrap to run after cilium failure")
		return nil
	}
	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayResolvePrivateNetworkIPv4CIDRFn = func(context.Context, *scaleway.Client, scw.Region, string, string) (string, error) {
		return "172.16.16.0/22", nil
	}

	cfg := &config.Config{
		Flux: config.FluxConfig{
			OCIRepo: "oci://ghcr.io/waddle-social/waddle/gitops",
		},
	}

	err := phasePostBootstrap(context.Background(), newPostBootstrapOperation(), cfg)
	if err == nil {
		t.Fatalf("expected post-bootstrap failure, got nil")
	}
	if !errors.Is(err, errCilium) {
		t.Fatalf("expected cilium error to be included: %v", err)
	}
}

func TestPhasePostBootstrapFailureDoesNotMaskOtherFailures(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	errCilium := errors.New("cilium failed")

	ciliumInstallFn = func(context.Context, cilium.InstallParams) error {
		return errCilium
	}
	fluxBootstrapFn = func(context.Context, flux.BootstrapParams) error {
		return nil
	}
	postBootstrapIngressGatewaySyncFn = func(context.Context, string, string) error {
		return nil
	}
	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayResolvePrivateNetworkIPv4CIDRFn = func(context.Context, *scaleway.Client, scw.Region, string, string) (string, error) {
		return "172.16.16.0/22", nil
	}

	err := phasePostBootstrap(context.Background(), newPostBootstrapOperation(), &config.Config{
		Environment: "production",
	})
	if err == nil {
		t.Fatalf("expected post-bootstrap failure, got nil")
	}
	if !errors.Is(err, errCilium) {
		t.Fatalf("expected cilium error to be included: %v", err)
	}
}

func TestPhasePostBootstrapSucceedsWhenAllComponentsSucceed(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	ciliumInstallFn = func(context.Context, cilium.InstallParams) error {
		return nil
	}
	fluxBootstrapFn = func(context.Context, flux.BootstrapParams) error {
		return nil
	}
	postBootstrapIngressGatewaySyncFn = func(context.Context, string, string) error {
		return nil
	}
	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayResolvePrivateNetworkIPv4CIDRFn = func(context.Context, *scaleway.Client, scw.Region, string, string) (string, error) {
		return "172.16.16.0/22", nil
	}

	cfg := &config.Config{}

	if err := phasePostBootstrap(context.Background(), newPostBootstrapOperation(), cfg); err != nil {
		t.Fatalf("expected post-bootstrap success, got error: %v", err)
	}
}

func TestPhasePostBootstrapPassesDiscoveredCIDRToCilium(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	var gotInstallParams cilium.InstallParams
	ciliumInstallFn = func(_ context.Context, params cilium.InstallParams) error {
		gotInstallParams = params
		return nil
	}
	fluxBootstrapFn = func(context.Context, flux.BootstrapParams) error {
		return nil
	}
	postBootstrapIngressGatewaySyncFn = func(context.Context, string, string) error {
		return nil
	}
	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayResolvePrivateNetworkIPv4CIDRFn = func(context.Context, *scaleway.Client, scw.Region, string, string) (string, error) {
		return "172.16.16.0/22", nil
	}

	cfg := &config.Config{}

	if err := phasePostBootstrap(context.Background(), newPostBootstrapOperation(), cfg); err != nil {
		t.Fatalf("phasePostBootstrap returned error: %v", err)
	}
	if gotInstallParams.IPv4NativeRoutingCIDR != "172.16.16.0/22" {
		t.Fatalf(
			"cilium install native routing cidr = %q, want %q",
			gotInstallParams.IPv4NativeRoutingCIDR,
			"172.16.16.0/22",
		)
	}
}

func TestPhasePostBootstrapPassesPreparedKubeconfigToComponents(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	const kubeconfigPath = "/tmp/bootstrap-kubeconfig"
	const ociRepo = "oci://ghcr.io/waddle-social/waddle/gitops"

	postBootstrapKubeconfigPathFn = func(context.Context, *operation.Operation, *config.Config) (string, func(), error) {
		return kubeconfigPath, func() {}, nil
	}

	var gotCiliumParams cilium.InstallParams
	ciliumInstallFn = func(_ context.Context, params cilium.InstallParams) error {
		gotCiliumParams = params
		return nil
	}

	var gotFluxParams flux.BootstrapParams
	fluxBootstrapFn = func(_ context.Context, params flux.BootstrapParams) error {
		gotFluxParams = params
		return nil
	}
	postBootstrapBootstrapSecretsFn = func(_ context.Context, kubeconfigPath string, cfg *config.Config) (map[string]string, error) {
		if kubeconfigPath != "/tmp/bootstrap-kubeconfig" {
			t.Fatalf("bootstrap secret kubeconfig = %q, want %q", kubeconfigPath, "/tmp/bootstrap-kubeconfig")
		}
		if cfg == nil {
			t.Fatal("expected config to be passed to bootstrap secret helper")
		}
		return map[string]string{
			fluxSubstituteTeleportGitHubClientID: "client-id-123",
		}, nil
	}
	var gotIngressTarget string
	postBootstrapIngressGatewaySyncFn = func(_ context.Context, kubeconfigPath, targetIPv4 string) error {
		if kubeconfigPath != "/tmp/bootstrap-kubeconfig" {
			t.Fatalf("ingress gateway sync kubeconfig = %q, want %q", kubeconfigPath, "/tmp/bootstrap-kubeconfig")
		}
		gotIngressTarget = targetIPv4
		return nil
	}

	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayResolvePrivateNetworkIPv4CIDRFn = func(context.Context, *scaleway.Client, scw.Region, string, string) (string, error) {
		return "172.16.16.0/22", nil
	}

	cfg := &config.Config{
		Flux: config.FluxConfig{
			OCIRepo: ociRepo,
		},
	}

	if err := phasePostBootstrap(context.Background(), newPostBootstrapOperation(), cfg); err != nil {
		t.Fatalf("phasePostBootstrap returned error: %v", err)
	}

	if gotCiliumParams.Kubeconfig != kubeconfigPath {
		t.Fatalf("cilium install kubeconfig = %q, want %q", gotCiliumParams.Kubeconfig, kubeconfigPath)
	}
	if gotFluxParams.Kubeconfig != kubeconfigPath {
		t.Fatalf("flux bootstrap kubeconfig = %q, want %q", gotFluxParams.Kubeconfig, kubeconfigPath)
	}
	if gotFluxParams.OCIRepo != ociRepo {
		t.Fatalf("flux bootstrap OCI repo = %q, want %q", gotFluxParams.OCIRepo, ociRepo)
	}
	if gotFluxParams.Substitute[fluxSubstituteTeleportGitHubClientID] != "client-id-123" {
		t.Fatalf("flux bootstrap substitute %s = %q, want %q", fluxSubstituteTeleportGitHubClientID, gotFluxParams.Substitute[fluxSubstituteTeleportGitHubClientID], "client-id-123")
	}
	if gotIngressTarget != "51.159.1.2" {
		t.Fatalf("ingress gateway sync target = %q, want %q", gotIngressTarget, "51.159.1.2")
	}
}

func TestPhasePostBootstrapContinuesWhenFluxOCIRepoIsEmpty(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	var fluxCalled bool
	fluxBootstrapFn = func(_ context.Context, params flux.BootstrapParams) error {
		fluxCalled = true
		if params.OCIRepo != "" {
			t.Fatalf("flux bootstrap OCI repo = %q, want empty", params.OCIRepo)
		}
		return nil
	}
	postBootstrapIngressGatewaySyncFn = func(context.Context, string, string) error {
		return nil
	}
	ciliumInstallFn = func(context.Context, cilium.InstallParams) error {
		return nil
	}
	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayResolvePrivateNetworkIPv4CIDRFn = func(context.Context, *scaleway.Client, scw.Region, string, string) (string, error) {
		return "172.16.16.0/22", nil
	}

	cfg := &config.Config{
		Flux: config.FluxConfig{
			OCIRepo: "   ",
		},
	}

	if err := phasePostBootstrap(context.Background(), newPostBootstrapOperation(), cfg); err != nil {
		t.Fatalf("phasePostBootstrap returned error: %v", err)
	}
	if !fluxCalled {
		t.Fatal("expected flux bootstrap to run")
	}
}

func TestPhasePostBootstrapPreparesOpenEBSMayastorBeforeFlux(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	var gotStorageParams storage.PrepareOpenEBSMayastorParams
	var storageCalled bool
	storagePrepareOpenEBSMayastorFn = func(_ context.Context, params storage.PrepareOpenEBSMayastorParams) error {
		storageCalled = true
		gotStorageParams = params
		return nil
	}

	var gotFluxParams flux.BootstrapParams
	var fluxCalled bool
	fluxBootstrapFn = func(_ context.Context, params flux.BootstrapParams) error {
		if !storageCalled {
			t.Fatal("expected storage prep to run before flux bootstrap")
		}
		gotFluxParams = params
		fluxCalled = true
		return nil
	}
	postBootstrapBootstrapSecretsFn = func(context.Context, string, *config.Config) (map[string]string, error) {
		return map[string]string{
			fluxSubstituteTeleportGitHubClientID: "teleport-client-id",
		}, nil
	}
	ciliumInstallFn = func(context.Context, cilium.InstallParams) error { return nil }
	postBootstrapIngressGatewaySyncFn = func(context.Context, string, string) error { return nil }
	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) { return &scaleway.Client{}, nil }
	scalewayResolvePrivateNetworkIPv4CIDRFn = func(context.Context, *scaleway.Client, scw.Region, string, string) (string, error) {
		return "172.16.16.0/24", nil
	}

	cfg := &config.Config{
		Storage: config.StorageConfig{
			Provider:            config.StorageProviderOpenEBSMayastorLab,
			DiskPoolDiskByID:    "/dev/disk/by-id/nvme-eui.1234",
			StorageClassName:    "openebs-mayastor",
			DefaultStorageClass: true,
			ReplicaCount:        1,
		},
		NodePools: []config.NodePoolConfig{
			{
				Name: "control-plane",
				Type: config.NodeTypeControlPlane,
				Size: 1,
				Disks: config.DiskConfig{
					OS:   "/dev/nvme0n1",
					Data: "/dev/nvme1n1",
				},
			},
		},
		Flux: config.FluxConfig{
			OCIRepo: "oci://ghcr.io/waddle-social/waddle/gitops",
		},
	}

	if err := phasePostBootstrap(context.Background(), newPostBootstrapOperation(), cfg); err != nil {
		t.Fatalf("phasePostBootstrap returned error: %v", err)
	}
	if !storageCalled {
		t.Fatal("expected storage prep to run")
	}
	if !fluxCalled {
		t.Fatal("expected flux bootstrap to run")
	}
	if gotStorageParams.NodeName != "production-control-plane-01" {
		t.Fatalf("storage prep node name = %q, want %q", gotStorageParams.NodeName, "production-control-plane-01")
	}
	if gotStorageParams.Device != "/dev/nvme1n1" {
		t.Fatalf("storage prep device = %q, want %q", gotStorageParams.Device, "/dev/nvme1n1")
	}
	if gotFluxParams.Substitute[fluxSubstituteStorageClass] != "openebs-mayastor" {
		t.Fatalf("flux substitute %s = %q, want %q", fluxSubstituteStorageClass, gotFluxParams.Substitute[fluxSubstituteStorageClass], "openebs-mayastor")
	}
	if gotFluxParams.Substitute[fluxSubstituteStorageNode] != "production-control-plane-01" {
		t.Fatalf("flux substitute %s = %q, want %q", fluxSubstituteStorageNode, gotFluxParams.Substitute[fluxSubstituteStorageNode], "production-control-plane-01")
	}
	if gotFluxParams.Substitute[fluxSubstituteDiskPoolDisk] != "/dev/disk/by-id/nvme-eui.1234" {
		t.Fatalf("flux substitute %s = %q, want %q", fluxSubstituteDiskPoolDisk, gotFluxParams.Substitute[fluxSubstituteDiskPoolDisk], "/dev/disk/by-id/nvme-eui.1234")
	}
	if gotFluxParams.Substitute[fluxSubstituteTeleportGitHubClientID] != "teleport-client-id" {
		t.Fatalf("flux substitute %s = %q, want %q", fluxSubstituteTeleportGitHubClientID, gotFluxParams.Substitute[fluxSubstituteTeleportGitHubClientID], "teleport-client-id")
	}
	if _, ok := gotFluxParams.Substitute["WADDLE_STORAGE_CLASS_DEFAULT"]; ok {
		t.Fatal("did not expect default storage class substitution for mayastor bootstrap")
	}
	if _, ok := gotFluxParams.Substitute["WADDLE_STORAGE_REPLICA_COUNT"]; ok {
		t.Fatal("did not expect replica count substitution for mayastor bootstrap")
	}
}

func TestPhasePostBootstrapSkipsStoragePrepWhenStorageDisabled(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	storagePrepareOpenEBSMayastorFn = func(context.Context, storage.PrepareOpenEBSMayastorParams) error {
		t.Fatal("did not expect storage prep to run")
		return nil
	}
	ciliumInstallFn = func(context.Context, cilium.InstallParams) error { return nil }
	fluxBootstrapFn = func(context.Context, flux.BootstrapParams) error { return nil }
	postBootstrapIngressGatewaySyncFn = func(context.Context, string, string) error { return nil }
	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) { return &scaleway.Client{}, nil }
	scalewayResolvePrivateNetworkIPv4CIDRFn = func(context.Context, *scaleway.Client, scw.Region, string, string) (string, error) {
		return "172.16.16.0/24", nil
	}

	if err := phasePostBootstrap(context.Background(), newPostBootstrapOperation(), &config.Config{}); err != nil {
		t.Fatalf("phasePostBootstrap returned error: %v", err)
	}
}

func TestPhasePostBootstrapStopsBeforeFluxWhenStoragePrepFails(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	errStorage := errors.New("storage prep failed")
	storagePrepareOpenEBSMayastorFn = func(context.Context, storage.PrepareOpenEBSMayastorParams) error {
		return errStorage
	}
	ciliumInstallFn = func(context.Context, cilium.InstallParams) error { return nil }
	fluxBootstrapFn = func(context.Context, flux.BootstrapParams) error {
		t.Fatal("did not expect flux bootstrap to run after storage prep failure")
		return nil
	}
	postBootstrapIngressGatewaySyncFn = func(context.Context, string, string) error { return nil }
	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) { return &scaleway.Client{}, nil }
	scalewayResolvePrivateNetworkIPv4CIDRFn = func(context.Context, *scaleway.Client, scw.Region, string, string) (string, error) {
		return "172.16.16.0/24", nil
	}

	cfg := &config.Config{
		Storage: config.StorageConfig{
			Provider:         config.StorageProviderOpenEBSMayastorLab,
			DiskPoolDiskByID: "/dev/disk/by-id/nvme-eui.1234",
			StorageClassName: "openebs-mayastor",
			ReplicaCount:     1,
		},
		NodePools: []config.NodePoolConfig{
			{
				Name: "control-plane",
				Type: config.NodeTypeControlPlane,
				Size: 1,
				Disks: config.DiskConfig{
					OS:   "/dev/nvme0n1",
					Data: "/dev/nvme1n1",
				},
			},
		},
	}

	err := phasePostBootstrap(context.Background(), newPostBootstrapOperation(), cfg)
	if err == nil {
		t.Fatal("expected post-bootstrap failure")
	}
	if !errors.Is(err, errStorage) {
		t.Fatalf("expected storage prep error, got %v", err)
	}
}

func TestPrepareBootstrapKubeconfigWithRetryRetriesUntilSuccess(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	postBootstrapKubeconfigRetryInterval = time.Millisecond
	postBootstrapKubeconfigRetryTimeout = 100 * time.Millisecond

	attempts := 0
	postBootstrapKubeconfigPathFn = func(context.Context, *operation.Operation, *config.Config) (string, func(), error) {
		attempts++
		if attempts < 3 {
			return "", nil, errors.New("talos api not ready")
		}
		return "/tmp/bootstrap-kubeconfig", func() {}, nil
	}

	path, cleanup, err := prepareBootstrapKubeconfigWithRetry(context.Background(), newPostBootstrapOperation(), &config.Config{})
	if err != nil {
		t.Fatalf("prepareBootstrapKubeconfigWithRetry returned error: %v", err)
	}
	if path != "/tmp/bootstrap-kubeconfig" {
		t.Fatalf("kubeconfig path = %q, want %q", path, "/tmp/bootstrap-kubeconfig")
	}
	if cleanup == nil {
		t.Fatal("expected cleanup function")
	}
	if attempts != 3 {
		t.Fatalf("attempts = %d, want %d", attempts, 3)
	}
}

func TestPrepareBootstrapKubeconfigWithRetryTimesOut(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	postBootstrapKubeconfigRetryInterval = time.Millisecond
	postBootstrapKubeconfigRetryTimeout = 5 * time.Millisecond

	errFetch := errors.New("dial timeout")
	postBootstrapKubeconfigPathFn = func(context.Context, *operation.Operation, *config.Config) (string, func(), error) {
		return "", nil, errFetch
	}

	_, _, err := prepareBootstrapKubeconfigWithRetry(context.Background(), newPostBootstrapOperation(), &config.Config{})
	if err == nil {
		t.Fatal("expected timeout error, got nil")
	}
	if !strings.Contains(err.Error(), "bootstrap kubeconfig not ready") {
		t.Fatalf("expected timeout wrapper in error, got %q", err)
	}
	if !errors.Is(err, errFetch) {
		t.Fatalf("expected wrapped fetch error, got %v", err)
	}
}

func TestWaitForKubernetesAPIWithRetryRetriesUntilSuccess(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	postBootstrapKubernetesAPIRetryInterval = time.Millisecond
	postBootstrapKubernetesAPIRetryTimeout = 100 * time.Millisecond

	attempts := 0
	postBootstrapKubernetesAPIProbeFn = func(context.Context, string) error {
		attempts++
		if attempts < 3 {
			return errors.New("connection refused")
		}
		return nil
	}

	if err := waitForKubernetesAPIWithRetry(context.Background(), "/tmp/bootstrap-kubeconfig"); err != nil {
		t.Fatalf("waitForKubernetesAPIWithRetry returned error: %v", err)
	}
	if attempts != 3 {
		t.Fatalf("attempts = %d, want %d", attempts, 3)
	}
}

func TestWaitForKubernetesAPIWithRetryTimesOut(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	postBootstrapKubernetesAPIRetryInterval = time.Millisecond
	postBootstrapKubernetesAPIRetryTimeout = 5 * time.Millisecond

	errProbe := errors.New("connection refused")
	postBootstrapKubernetesAPIProbeFn = func(context.Context, string) error {
		return errProbe
	}

	err := waitForKubernetesAPIWithRetry(context.Background(), "/tmp/bootstrap-kubeconfig")
	if err == nil {
		t.Fatal("expected timeout error, got nil")
	}
	if !strings.Contains(err.Error(), "kubernetes API not ready") {
		t.Fatalf("expected timeout wrapper in error, got %q", err)
	}
	if !errors.Is(err, errProbe) {
		t.Fatalf("expected wrapped probe error, got %v", err)
	}
}

func TestPhasePostBootstrapFailsWhenKubernetesAPINotReachable(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	waitErr := errors.New("api not ready")
	postBootstrapKubernetesAPIWaitFn = func(context.Context, string) error {
		return waitErr
	}
	ciliumInstallFn = func(context.Context, cilium.InstallParams) error {
		t.Fatal("cilium install should not run when kubernetes API is not reachable")
		return nil
	}
	fluxBootstrapFn = func(context.Context, flux.BootstrapParams) error {
		t.Fatal("flux bootstrap should not run when kubernetes API is not reachable")
		return nil
	}
	postBootstrapIngressGatewaySyncFn = func(context.Context, string, string) error {
		t.Fatal("ingress gateway sync should not run when kubernetes API is not reachable")
		return nil
	}
	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayResolvePrivateNetworkIPv4CIDRFn = func(context.Context, *scaleway.Client, scw.Region, string, string) (string, error) {
		return "172.16.16.0/22", nil
	}

	cfg := &config.Config{}

	err := phasePostBootstrap(context.Background(), newPostBootstrapOperation(), cfg)
	if err == nil {
		t.Fatal("expected kubernetes API readiness failure, got nil")
	}
	if !strings.Contains(err.Error(), "wait for kubernetes API readiness") {
		t.Fatalf("expected wait wrapper in error, got %q", err)
	}
	if !errors.Is(err, waitErr) {
		t.Fatalf("expected wrapped wait error, got %v", err)
	}
}

func TestPhasePostBootstrapFailsWhenCIDRDiscoveryFails(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	ciliumInstallFn = func(context.Context, cilium.InstallParams) error {
		t.Fatal("cilium install should not run when CIDR discovery fails")
		return nil
	}
	fluxBootstrapFn = func(context.Context, flux.BootstrapParams) error {
		return nil
	}
	postBootstrapIngressGatewaySyncFn = func(context.Context, string, string) error {
		t.Fatal("ingress gateway sync should not run when CIDR discovery fails")
		return nil
	}
	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayResolvePrivateNetworkIPv4CIDRFn = func(context.Context, *scaleway.Client, scw.Region, string, string) (string, error) {
		return "", errors.New("no discovered IPv4 CIDR contains preferred private IP")
	}

	cfg := &config.Config{}

	err := phasePostBootstrap(context.Background(), newPostBootstrapOperation(), cfg)
	if err == nil {
		t.Fatal("expected cidr discovery failure, got nil")
	}
	if !strings.Contains(err.Error(), "resolve cilium ipv4 native routing cidr") {
		t.Fatalf("expected wrapped cidr discovery failure, got %q", err)
	}
}

func TestPhasePostBootstrapRecoversMissingPrivateNetworkIDFromScaleway(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	op := newPostBootstrapOperation()
	delete(op.Context, "privateNetworkID")

	var gotCIDRLookupPrivateNetworkID string
	ciliumInstallFn = func(context.Context, cilium.InstallParams) error {
		return nil
	}
	fluxBootstrapFn = func(context.Context, flux.BootstrapParams) error {
		return nil
	}
	postBootstrapIngressGatewaySyncFn = func(context.Context, string, string) error {
		return nil
	}
	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayEnsureNetworkFoundationFn = func(context.Context, *scaleway.Client, scaleway.NetworkFoundationParams) (*scaleway.NetworkFoundation, error) {
		return &scaleway.NetworkFoundation{
			PrivateNetworkID: "pn-recovered",
		}, nil
	}
	scalewayResolvePrivateNetworkIPv4CIDRFn = func(_ context.Context, _ *scaleway.Client, _ scw.Region, privateNetworkID string, _ string) (string, error) {
		gotCIDRLookupPrivateNetworkID = privateNetworkID
		return "172.16.16.0/22", nil
	}

	cfg := &config.Config{
		Environment: "production",
	}

	if err := phasePostBootstrap(context.Background(), op, cfg); err != nil {
		t.Fatalf("phasePostBootstrap returned error: %v", err)
	}
	if gotCIDRLookupPrivateNetworkID != "pn-recovered" {
		t.Fatalf("cidr lookup private network id = %q, want %q", gotCIDRLookupPrivateNetworkID, "pn-recovered")
	}
	if got := op.GetContextString("privateNetworkID"); got != "pn-recovered" {
		t.Fatalf("operation privateNetworkID context = %q, want %q", got, "pn-recovered")
	}
}

func TestPhasePostBootstrapUsesIngressPublicIPv4Override(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	var gotIngressTarget string
	ciliumInstallFn = func(context.Context, cilium.InstallParams) error {
		return nil
	}
	fluxBootstrapFn = func(context.Context, flux.BootstrapParams) error {
		return nil
	}
	postBootstrapIngressGatewaySyncFn = func(_ context.Context, _ string, targetIPv4 string) error {
		gotIngressTarget = targetIPv4
		return nil
	}
	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayResolvePrivateNetworkIPv4CIDRFn = func(context.Context, *scaleway.Client, scw.Region, string, string) (string, error) {
		return "172.16.16.0/22", nil
	}

	cfg := &config.Config{
		Ingress: config.IngressConfig{
			PublicIPv4: "203.0.113.10",
		},
	}

	if err := phasePostBootstrap(context.Background(), newPostBootstrapOperation(), cfg); err != nil {
		t.Fatalf("phasePostBootstrap returned error: %v", err)
	}
	if gotIngressTarget != "203.0.113.10" {
		t.Fatalf("ingress gateway sync target = %q, want %q", gotIngressTarget, "203.0.113.10")
	}
}

func TestPhasePostBootstrapAggregatesIngressGatewaySyncFailure(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	errSync := errors.New("gateway sync failed")
	ciliumInstallFn = func(context.Context, cilium.InstallParams) error {
		return nil
	}
	fluxBootstrapFn = func(context.Context, flux.BootstrapParams) error {
		return nil
	}
	postBootstrapIngressGatewaySyncFn = func(context.Context, string, string) error {
		return errSync
	}
	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayResolvePrivateNetworkIPv4CIDRFn = func(context.Context, *scaleway.Client, scw.Region, string, string) (string, error) {
		return "172.16.16.0/22", nil
	}

	err := phasePostBootstrap(context.Background(), newPostBootstrapOperation(), &config.Config{})
	if err == nil {
		t.Fatal("expected ingress gateway sync failure, got nil")
	}
	if !errors.Is(err, errSync) {
		t.Fatalf("expected wrapped ingress gateway sync error, got %v", err)
	}
}

func TestPhasePostBootstrapSkipsMissingIngressGateway(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	ciliumInstallFn = func(context.Context, cilium.InstallParams) error {
		return nil
	}
	fluxBootstrapFn = func(context.Context, flux.BootstrapParams) error {
		return nil
	}
	postBootstrapIngressGatewaySyncFn = func(context.Context, string, string) error {
		return errIngressGatewayNotFound
	}
	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayResolvePrivateNetworkIPv4CIDRFn = func(context.Context, *scaleway.Client, scw.Region, string, string) (string, error) {
		return "172.16.16.0/22", nil
	}

	if err := phasePostBootstrap(context.Background(), newPostBootstrapOperation(), &config.Config{}); err != nil {
		t.Fatalf("expected missing ingress gateway to be non-fatal, got %v", err)
	}
}

func TestMaybeRefreshOperationServerIPsUpdatesContext(t *testing.T) {
	restorePostBootstrapFns()
	t.Cleanup(restorePostBootstrapFns)

	op, _ := operation.New("op-test", operation.TypeCreateCluster, "production", []string{"post-bootstrap"})
	op.SetContext("serverId", "srv-123")
	op.SetContext("zone", string(scw.ZoneFrPar1))
	op.SetContext("publicIP", "51.15.0.10")
	op.SetContext("privateIP", "172.16.16.10")

	scalewayNewClientFn = func(string, string, string, string) (*scaleway.Client, error) {
		return &scaleway.Client{}, nil
	}
	scalewayGetServerFn = func(context.Context, *scaleway.Client, scw.Zone, string) (*baremetal.Server, error) {
		return &baremetal.Server{
			IPs: []*baremetal.IP{
				{Address: net.ParseIP("172.16.16.16"), Version: "IPv4"},
				{Address: net.ParseIP("51.159.1.2"), Version: "IPv4"},
			},
		}, nil
	}

	maybeRefreshOperationServerIPs(context.Background(), op, &config.Config{
		Scaleway: config.ScalewayConfig{
			ProjectID:      "project-id",
			OrganizationID: "org-id",
		},
	})

	if got := op.GetContextString("publicIP"); got != "51.159.1.2" {
		t.Fatalf("publicIP = %q, want %q", got, "51.159.1.2")
	}
	if got := op.GetContextString("privateIP"); got != "172.16.16.16" {
		t.Fatalf("privateIP = %q, want %q", got, "172.16.16.16")
	}
}
