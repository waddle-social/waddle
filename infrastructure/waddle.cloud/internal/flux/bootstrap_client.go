package flux

import (
	"context"
	"fmt"
	"log/slog"
	"strings"

	kustomizev1 "github.com/fluxcd/kustomize-controller/api/v1"
	sourcev1 "github.com/fluxcd/source-controller/api/v1"
	appsv1 "k8s.io/api/apps/v1"
	apimeta "k8s.io/apimachinery/pkg/api/meta"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/client-go/rest"
	"k8s.io/client-go/tools/clientcmd"
	ctrlclient "sigs.k8s.io/controller-runtime/pkg/client"
)

func kubeConfig(kubeconfig string) (*rest.Config, error) {
	if kubeconfig != "" {
		return clientcmd.BuildConfigFromFlags("", kubeconfig)
	}

	loadingRules := clientcmd.NewDefaultClientConfigLoadingRules()
	return clientcmd.NewNonInteractiveDeferredLoadingClientConfig(loadingRules, &clientcmd.ConfigOverrides{}).ClientConfig()
}

func newFluxClient(cfg *rest.Config) (ctrlclient.Client, error) {
	return ctrlclient.New(cfg, ctrlclient.Options{Scheme: newFluxScheme()})
}

func newFluxScheme() *runtime.Scheme {
	scheme := runtime.NewScheme()
	_ = appsv1.AddToScheme(scheme)
	_ = sourcev1.AddToScheme(scheme)
	_ = kustomizev1.AddToScheme(scheme)
	return scheme
}

// Status checks FluxCD reconciliation status.
func Status(ctx context.Context, kubeconfig string) error {
	if ctx == nil {
		ctx = context.Background()
	}

	cfg, err := kubeConfig(strings.TrimSpace(kubeconfig))
	if err != nil {
		return fmt.Errorf("load kube config: %w", err)
	}

	kubeClient, err := newFluxClient(cfg)
	if err != nil {
		return fmt.Errorf("create kubernetes client: %w", err)
	}

	for _, controller := range fluxControllers {
		var deployment appsv1.Deployment
		key := ctrlclient.ObjectKey{Name: controller, Namespace: fluxNamespace}
		if err := kubeClient.Get(ctx, key, &deployment); err != nil {
			return fmt.Errorf("get deployment %s: %w", key.String(), err)
		}
		if !deploymentReady(&deployment) {
			return fmt.Errorf("deployment %s is not ready", key.String())
		}
	}

	var repositories sourcev1.OCIRepositoryList
	if err := kubeClient.List(ctx, &repositories, ctrlclient.InNamespace(fluxNamespace)); err != nil {
		return fmt.Errorf("list OCIRepositories: %w", err)
	}
	for _, repo := range repositories.Items {
		if readyCondition := apimeta.FindStatusCondition(repo.Status.Conditions, readyConditionType); readyCondition != nil && readyCondition.Status == metav1.ConditionFalse {
			return fmt.Errorf("OCIRepository %s/%s not ready: %s", repo.Namespace, repo.Name, readyCondition.Message)
		}
	}

	var kustomizations kustomizev1.KustomizationList
	if err := kubeClient.List(ctx, &kustomizations, ctrlclient.InNamespace(fluxNamespace)); err != nil {
		return fmt.Errorf("list Kustomizations: %w", err)
	}
	for _, ks := range kustomizations.Items {
		if readyCondition := apimeta.FindStatusCondition(ks.Status.Conditions, readyConditionType); readyCondition != nil && readyCondition.Status == metav1.ConditionFalse {
			return fmt.Errorf("Kustomization %s/%s not ready: %s", ks.Namespace, ks.Name, readyCondition.Message)
		}
	}

	slog.Info("fluxcd status healthy")
	return nil
}

// IsInstalled is retained for compatibility and always returns true because Flux integration is in-process.
func IsInstalled() bool {
	return true
}
