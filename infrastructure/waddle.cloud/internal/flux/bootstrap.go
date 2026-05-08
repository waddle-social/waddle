package flux

import (
	"context"
	"fmt"
	"log/slog"
	"strings"
	"time"

	fluxinstall "github.com/fluxcd/flux2/v2/pkg/manifestgen/install"
	kustomizev1 "github.com/fluxcd/kustomize-controller/api/v1"
	sourcev1 "github.com/fluxcd/source-controller/api/v1"
	apierrors "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/rest"
	ctrlclient "sigs.k8s.io/controller-runtime/pkg/client"
)

type BootstrapParams struct {
	Kubeconfig string
	OCIRepo    string // OCI repository URL (e.g. oci://ghcr.io/waddle-social/waddle/gitops)
	Substitute map[string]string
	Version    string // Optional Flux version for install manifest generation.
}

const (
	fluxNamespace        = "flux-system"
	clusterConfigName    = "bootstrap"
	fluxFieldManager     = "waddle-cloud"
	readyConditionType   = "Ready"
	rootReconcileTimeout = 20 * time.Minute
)

var fluxControllers = []string{
	"source-controller",
	"kustomize-controller",
	"helm-controller",
	"notification-controller",
}

// Bootstrap installs FluxCD into the cluster and configures an OCI source/Kustomization when provided.
// This implementation uses Flux and Kubernetes Go modules directly (no external flux CLI binary).
func Bootstrap(ctx context.Context, params BootstrapParams) error {
	if ctx == nil {
		ctx = context.Background()
	}

	cfg, err := kubeConfig(strings.TrimSpace(params.Kubeconfig))
	if err != nil {
		return fmt.Errorf("load kube config: %w", err)
	}

	if err := installComponents(ctx, cfg, strings.TrimSpace(params.Version)); err != nil {
		return err
	}

	if strings.TrimSpace(params.OCIRepo) == "" {
		slog.Info("fluxcd install complete (no OCI source configured)")
		return nil
	}

	if err := upsertOCIResources(ctx, cfg, strings.TrimSpace(params.OCIRepo), params.Substitute); err != nil {
		return err
	}

	slog.Info("fluxcd bootstrap complete", "oci_repo", strings.TrimSpace(params.OCIRepo))
	return nil
}

func installComponents(ctx context.Context, cfg *rest.Config, version string) error {
	opts := fluxinstall.MakeDefaultOptions()
	opts.Namespace = fluxNamespace
	if version != "" {
		opts.Version = version
	}

	slog.Info("installing flux components", "version", opts.Version, "namespace", opts.Namespace)

	manifest, err := fluxinstall.Generate(opts, "")
	if err != nil {
		return fmt.Errorf("generate flux install manifests: %w", err)
	}

	if err := applyManifest(ctx, cfg, manifest.Content); err != nil {
		return fmt.Errorf("apply flux install manifests: %w", err)
	}

	kubeClient, err := newFluxClient(cfg)
	if err != nil {
		return fmt.Errorf("create kubernetes client: %w", err)
	}

	if err := waitForFluxControllers(ctx, kubeClient, 5*time.Minute); err != nil {
		return fmt.Errorf("wait for flux controllers: %w", err)
	}

	slog.Info("flux components installed")
	return nil
}

func upsertOCIResources(ctx context.Context, cfg *rest.Config, ociRepo string, substitute map[string]string) error {
	kubeClient, err := newFluxClient(cfg)
	if err != nil {
		return fmt.Errorf("create kubernetes client: %w", err)
	}

	if err := upsertOCIRepository(ctx, kubeClient, ociRepo); err != nil {
		return err
	}

	if err := upsertKustomization(ctx, kubeClient, substitute); err != nil {
		return err
	}

	if err := waitForOCIRepositoryReady(ctx, kubeClient, 5*time.Minute); err != nil {
		return err
	}

	if err := waitForKustomizationReady(ctx, kubeClient, rootReconcileTimeout); err != nil {
		return err
	}

	return nil
}

func upsertOCIRepository(ctx context.Context, kubeClient ctrlclient.Client, ociRepo string) error {
	desired := &sourcev1.OCIRepository{
		ObjectMeta: metav1.ObjectMeta{
			Name:      clusterConfigName,
			Namespace: fluxNamespace,
		},
		Spec: sourcev1.OCIRepositorySpec{
			URL: ociRepo,
			Interval: metav1.Duration{
				Duration: 5 * time.Minute,
			},
			Reference: &sourcev1.OCIRepositoryRef{
				Tag: "latest",
			},
		},
	}

	var existing sourcev1.OCIRepository
	key := ctrlclient.ObjectKeyFromObject(desired)
	err := kubeClient.Get(ctx, key, &existing)
	if apierrors.IsNotFound(err) {
		slog.Info("creating flux OCI source", "repo", ociRepo)
		if err := kubeClient.Create(ctx, desired); err != nil {
			return fmt.Errorf("create OCIRepository %s: %w", key.String(), err)
		}
		return nil
	}
	if err != nil {
		return fmt.Errorf("get OCIRepository %s: %w", key.String(), err)
	}

	existing.Spec = desired.Spec
	if err := kubeClient.Update(ctx, &existing); err != nil {
		return fmt.Errorf("update OCIRepository %s: %w", key.String(), err)
	}

	slog.Info("updated flux OCI source", "repo", ociRepo)
	return nil
}

func upsertKustomization(ctx context.Context, kubeClient ctrlclient.Client, substitute map[string]string) error {
	desired := &kustomizev1.Kustomization{
		ObjectMeta: metav1.ObjectMeta{
			Name:      clusterConfigName,
			Namespace: fluxNamespace,
		},
		Spec: kustomizev1.KustomizationSpec{
			Interval: metav1.Duration{
				Duration: 5 * time.Minute,
			},
			Timeout: &metav1.Duration{Duration: rootReconcileTimeout},
			Path:    "./",
			Prune:   true,
			Wait:    true,
			SourceRef: kustomizev1.CrossNamespaceSourceReference{
				Kind: sourcev1.OCIRepositoryKind,
				Name: clusterConfigName,
			},
		},
	}
	if len(substitute) > 0 {
		desired.Spec.PostBuild = &kustomizev1.PostBuild{
			Substitute: copySubstituteMap(substitute),
		}
	}

	var existing kustomizev1.Kustomization
	key := ctrlclient.ObjectKeyFromObject(desired)
	err := kubeClient.Get(ctx, key, &existing)
	if apierrors.IsNotFound(err) {
		slog.Info("creating flux kustomization")
		if err := kubeClient.Create(ctx, desired); err != nil {
			return fmt.Errorf("create Kustomization %s: %w", key.String(), err)
		}
		return nil
	}
	if err != nil {
		return fmt.Errorf("get Kustomization %s: %w", key.String(), err)
	}

	existing.Spec = desired.Spec
	if err := kubeClient.Update(ctx, &existing); err != nil {
		return fmt.Errorf("update Kustomization %s: %w", key.String(), err)
	}

	slog.Info("updated flux kustomization")
	return nil
}

func copySubstituteMap(values map[string]string) map[string]string {
	if len(values) == 0 {
		return nil
	}

	out := make(map[string]string, len(values))
	for key, value := range values {
		out[key] = value
	}

	return out
}
