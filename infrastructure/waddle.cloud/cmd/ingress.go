package cmd

import (
	"context"
	"errors"
	"fmt"
	"net"
	"strings"
	"time"

	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/operation"
	gatewayv1 "sigs.k8s.io/gateway-api/apis/v1"

	apierrors "k8s.io/apimachinery/pkg/api/errors"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/util/wait"
	"k8s.io/client-go/tools/clientcmd"
	ctrlclient "sigs.k8s.io/controller-runtime/pkg/client"
)

const (
	ingressGatewayNamespace     = "default"
	ingressGatewayName          = "waddle-gateway"
	externalDNSTargetAnnotation = "external-dns.alpha.kubernetes.io/target"
	opContextIngressPublicIPv4  = "ingressPublicIPv4"
)

var (
	postBootstrapIngressGatewaySyncFn       = syncIngressGatewayTarget
	postBootstrapIngressGatewaySyncInterval = 5 * time.Second
	postBootstrapIngressGatewaySyncTimeout  = 10 * time.Minute
	errIngressGatewayNotFound               = errors.New("ingress gateway not found")
)

func ingressPublicIPv4ForOperation(cfg *config.Config, op *operation.Operation) (string, error) {
	if op == nil {
		return "", fmt.Errorf("operation context is required")
	}

	publicIPv4 := strings.TrimSpace(op.GetContextString("publicIP"))
	if cfg != nil {
		publicIPv4 = cfg.EffectiveIngressPublicIPv4(publicIPv4)
	}
	if publicIPv4 == "" {
		return "", fmt.Errorf("missing ingress public IPv4 in operation context and ingress.publicIPv4 override")
	}

	parsed := net.ParseIP(publicIPv4)
	if parsed == nil || parsed.To4() == nil {
		return "", fmt.Errorf("ingress public IPv4 %q is not a valid IPv4 address", publicIPv4)
	}

	return publicIPv4, nil
}

func syncIngressGatewayTarget(ctx context.Context, kubeconfigPath, targetIPv4 string) error {
	kubeconfigPath = strings.TrimSpace(kubeconfigPath)
	if kubeconfigPath == "" {
		return fmt.Errorf("bootstrap kubeconfig path is required")
	}

	parsed := net.ParseIP(strings.TrimSpace(targetIPv4))
	if parsed == nil || parsed.To4() == nil {
		return fmt.Errorf("ingress target %q is not a valid IPv4 address", targetIPv4)
	}

	restCfg, err := clientcmd.BuildConfigFromFlags("", kubeconfigPath)
	if err != nil {
		return fmt.Errorf("load kube config: %w", err)
	}

	scheme := runtime.NewScheme()
	if err := gatewayv1.Install(scheme); err != nil {
		return fmt.Errorf("register gateway API types: %w", err)
	}

	kubeClient, err := ctrlclient.New(restCfg, ctrlclient.Options{Scheme: scheme})
	if err != nil {
		return fmt.Errorf("create kubernetes client: %w", err)
	}

	key := ctrlclient.ObjectKey{Name: ingressGatewayName, Namespace: ingressGatewayNamespace}
	timeout := postBootstrapIngressGatewaySyncTimeout
	if timeout <= 0 {
		timeout = time.Minute
	}

	interval := postBootstrapIngressGatewaySyncInterval
	if interval <= 0 {
		interval = time.Second
	}

	var gatewaySeen bool
	err = wait.PollUntilContextTimeout(ctx, interval, timeout, true, func(ctx context.Context) (bool, error) {
		var gateway gatewayv1.Gateway
		if err := kubeClient.Get(ctx, key, &gateway); err != nil {
			if apierrors.IsNotFound(err) {
				return false, nil
			}
			return false, fmt.Errorf("load gateway %s: %w", key.String(), err)
		}
		gatewaySeen = true

		if strings.TrimSpace(gateway.Annotations[externalDNSTargetAnnotation]) == targetIPv4 {
			return true, nil
		}

		base := gateway.DeepCopy()
		if gateway.Annotations == nil {
			gateway.Annotations = map[string]string{}
		}
		gateway.Annotations[externalDNSTargetAnnotation] = targetIPv4

		if err := kubeClient.Patch(ctx, &gateway, ctrlclient.MergeFrom(base)); err != nil {
			if apierrors.IsConflict(err) {
				return false, nil
			}
			return false, fmt.Errorf("patch gateway %s DNS target annotation: %w", key.String(), err)
		}

		return true, nil
	})
	if err != nil && !gatewaySeen {
		return fmt.Errorf("%w: %s", errIngressGatewayNotFound, key.String())
	}

	return err
}
