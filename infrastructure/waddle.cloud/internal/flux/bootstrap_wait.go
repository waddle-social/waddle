package flux

import (
	"context"
	"fmt"
	"time"

	kustomizev1 "github.com/fluxcd/kustomize-controller/api/v1"
	sourcev1 "github.com/fluxcd/source-controller/api/v1"
	appsv1 "k8s.io/api/apps/v1"
	apierrors "k8s.io/apimachinery/pkg/api/errors"
	apimeta "k8s.io/apimachinery/pkg/api/meta"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/util/wait"
	ctrlclient "sigs.k8s.io/controller-runtime/pkg/client"
)

func waitForOCIRepositoryReady(ctx context.Context, kubeClient ctrlclient.Client, timeout time.Duration) error {
	key := ctrlclient.ObjectKey{Name: clusterConfigName, Namespace: fluxNamespace}
	return waitForReadyCondition(ctx, timeout, "OCIRepository/"+key.String(), func(ctx context.Context) ([]metav1.Condition, error) {
		var repo sourcev1.OCIRepository
		if err := kubeClient.Get(ctx, key, &repo); err != nil {
			return nil, err
		}
		return repo.Status.Conditions, nil
	})
}

func waitForKustomizationReady(ctx context.Context, kubeClient ctrlclient.Client, timeout time.Duration) error {
	key := ctrlclient.ObjectKey{Name: clusterConfigName, Namespace: fluxNamespace}
	return waitForReadyCondition(ctx, timeout, "Kustomization/"+key.String(), func(ctx context.Context) ([]metav1.Condition, error) {
		var ks kustomizev1.Kustomization
		if err := kubeClient.Get(ctx, key, &ks); err != nil {
			return nil, err
		}
		return ks.Status.Conditions, nil
	})
}

func waitForReadyCondition(
	ctx context.Context,
	timeout time.Duration,
	resource string,
	conditionsFn func(context.Context) ([]metav1.Condition, error),
) error {
	return wait.PollUntilContextTimeout(ctx, 2*time.Second, timeout, true, func(ctx context.Context) (bool, error) {
		conditions, err := conditionsFn(ctx)
		if err != nil {
			if apierrors.IsNotFound(err) {
				return false, nil
			}
			return false, err
		}

		readyCondition := apimeta.FindStatusCondition(conditions, readyConditionType)
		if readyCondition == nil {
			return false, nil
		}

		switch readyCondition.Status {
		case metav1.ConditionTrue:
			return true, nil
		case metav1.ConditionFalse:
			return false, fmt.Errorf("%s not ready: %s", resource, readyCondition.Message)
		default:
			return false, nil
		}
	})
}

func waitForFluxControllers(ctx context.Context, kubeClient ctrlclient.Client, timeout time.Duration) error {
	for _, controller := range fluxControllers {
		key := ctrlclient.ObjectKey{Name: controller, Namespace: fluxNamespace}
		err := wait.PollUntilContextTimeout(ctx, 2*time.Second, timeout, true, func(ctx context.Context) (bool, error) {
			var deployment appsv1.Deployment
			if err := kubeClient.Get(ctx, key, &deployment); err != nil {
				if apierrors.IsNotFound(err) {
					return false, nil
				}
				return false, err
			}
			return deploymentReady(&deployment), nil
		})
		if err != nil {
			return fmt.Errorf("controller %s not ready: %w", key.String(), err)
		}
	}

	return nil
}

func deploymentReady(deployment *appsv1.Deployment) bool {
	if deployment == nil || deployment.Spec.Replicas == nil {
		return false
	}

	desired := *deployment.Spec.Replicas
	if desired == 0 {
		return true
	}

	return deployment.Status.UpdatedReplicas >= desired &&
		deployment.Status.AvailableReplicas >= desired &&
		deployment.Status.ObservedGeneration >= deployment.Generation
}
