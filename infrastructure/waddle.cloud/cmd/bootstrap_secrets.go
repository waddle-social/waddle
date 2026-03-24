package cmd

import (
	"context"
	"fmt"
	"strings"

	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/secrets"
	corev1 "k8s.io/api/core/v1"
	apierrors "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/client-go/tools/clientcmd"
	ctrlclient "sigs.k8s.io/controller-runtime/pkg/client"
)

const (
	onePasswordConnectCredentialsSecretKey = "ONEPASSWORD_CONNECT_CREDENTIALS_JSON"
	onePasswordConnectTokenSecretKey       = "ONEPASSWORD_CONNECT_TOKEN"
	teleportGitHubClientIDSecretKey        = "TELEPORT_GITHUB_CLIENT_ID"
	teleportGitHubClientSecretSecretKey    = "TELEPORT_GITHUB_CLIENT_SECRET"

	onePasswordNamespace             = "1password"
	teleportNamespace                = "teleport"
	onePasswordCredentialsName       = "onepassword-credentials"
	onePasswordCredentialsDataKey    = "1password-credentials.json"
	onePasswordTokenName             = "onepassword-token"
	onePasswordTokenDataKey          = "token"
	teleportGitHubConnectorSecret    = "teleport-github-connector"
	teleportGitHubConnectorDataKey   = "clientSecret"
	teleportGitHubConnectorIDDataKey = "clientID"
	teleportGitHubLookupAnnotation   = "resources.teleport.dev/allow-lookup-from-cr"
)

func prepareBootstrapSecrets(ctx context.Context, kubeconfigPath string, cfg *config.Config) (map[string]string, error) {
	if cfg == nil || strings.TrimSpace(cfg.Flux.OCIRepo) == "" {
		return nil, nil
	}
	if strings.TrimSpace(kubeconfigPath) == "" {
		return nil, fmt.Errorf("bootstrap kubeconfig path is required")
	}

	store, err := getOrCreateSecretStore(ctx, cfg)
	if err != nil {
		return nil, err
	}

	secretPath := strings.TrimSpace(cfg.Secrets.SecretPath)
	if secretPath == "" {
		return nil, fmt.Errorf("secrets.secretPath is required")
	}

	onePasswordCredentials, err := requiredBootstrapSecret(ctx, store, secretPath, onePasswordConnectCredentialsSecretKey)
	if err != nil {
		return nil, err
	}
	onePasswordToken, err := requiredBootstrapSecret(ctx, store, secretPath, onePasswordConnectTokenSecretKey)
	if err != nil {
		return nil, err
	}
	teleportGitHubClientID, err := requiredBootstrapSecret(ctx, store, secretPath, teleportGitHubClientIDSecretKey)
	if err != nil {
		return nil, err
	}
	teleportGitHubClientSecret, err := requiredBootstrapSecret(ctx, store, secretPath, teleportGitHubClientSecretSecretKey)
	if err != nil {
		return nil, err
	}

	kubeClient, err := newBootstrapSecretClient(kubeconfigPath)
	if err != nil {
		return nil, err
	}

	if err := ensureNamespace(ctx, kubeClient, onePasswordNamespace); err != nil {
		return nil, err
	}
	if err := ensureNamespace(ctx, kubeClient, teleportNamespace); err != nil {
		return nil, err
	}

	if err := ensureOpaqueSecret(ctx, kubeClient, onePasswordNamespace, onePasswordCredentialsName, nil, map[string][]byte{
		onePasswordCredentialsDataKey: []byte(onePasswordCredentials),
	}); err != nil {
		return nil, fmt.Errorf("ensure %s/%s: %w", onePasswordNamespace, onePasswordCredentialsName, err)
	}
	if err := ensureOpaqueSecret(ctx, kubeClient, onePasswordNamespace, onePasswordTokenName, nil, map[string][]byte{
		onePasswordTokenDataKey: []byte(onePasswordToken),
	}); err != nil {
		return nil, fmt.Errorf("ensure %s/%s: %w", onePasswordNamespace, onePasswordTokenName, err)
	}
	if err := ensureOpaqueSecret(ctx, kubeClient, teleportNamespace, teleportGitHubConnectorSecret, map[string]string{
		teleportGitHubLookupAnnotation: "github",
	}, map[string][]byte{
		teleportGitHubConnectorDataKey:   []byte(teleportGitHubClientSecret),
		teleportGitHubConnectorIDDataKey: []byte(teleportGitHubClientID),
	}); err != nil {
		return nil, fmt.Errorf("ensure %s/%s: %w", teleportNamespace, teleportGitHubConnectorSecret, err)
	}

	return map[string]string{
		fluxSubstituteTeleportGitHubClientID: teleportGitHubClientID,
	}, nil
}

func requiredBootstrapSecret(ctx context.Context, store secrets.Store, secretPath, key string) (string, error) {
	value, err := store.GetSecret(ctx, secretPath, key)
	if err != nil {
		return "", fmt.Errorf("load %s from secret path %s: %w", key, secretPath, err)
	}
	if strings.TrimSpace(value) == "" {
		return "", fmt.Errorf("%s is empty in secret path %s", key, secretPath)
	}

	return value, nil
}

func newBootstrapSecretClient(kubeconfigPath string) (ctrlclient.Client, error) {
	restConfig, err := clientcmd.BuildConfigFromFlags("", kubeconfigPath)
	if err != nil {
		return nil, fmt.Errorf("load kubeconfig %s: %w", kubeconfigPath, err)
	}

	scheme := runtime.NewScheme()
	if err := corev1.AddToScheme(scheme); err != nil {
		return nil, fmt.Errorf("register corev1 scheme: %w", err)
	}

	kubeClient, err := ctrlclient.New(restConfig, ctrlclient.Options{Scheme: scheme})
	if err != nil {
		return nil, fmt.Errorf("create kubernetes client: %w", err)
	}

	return kubeClient, nil
}

func ensureNamespace(ctx context.Context, kubeClient ctrlclient.Client, namespace string) error {
	if strings.TrimSpace(namespace) == "" {
		return fmt.Errorf("namespace is required")
	}

	key := ctrlclient.ObjectKey{Name: namespace}
	existing := &corev1.Namespace{}
	if err := kubeClient.Get(ctx, key, existing); err != nil {
		if apierrors.IsNotFound(err) {
			return kubeClient.Create(ctx, &corev1.Namespace{
				ObjectMeta: metav1.ObjectMeta{Name: namespace},
			})
		}
		return fmt.Errorf("get namespace %s: %w", namespace, err)
	}

	return nil
}

func ensureOpaqueSecret(
	ctx context.Context,
	kubeClient ctrlclient.Client,
	namespace string,
	name string,
	annotations map[string]string,
	data map[string][]byte,
) error {
	key := ctrlclient.ObjectKey{Namespace: namespace, Name: name}
	existing := &corev1.Secret{}
	if err := kubeClient.Get(ctx, key, existing); err != nil {
		if apierrors.IsNotFound(err) {
			secret := &corev1.Secret{
				ObjectMeta: metav1.ObjectMeta{
					Namespace:   namespace,
					Name:        name,
					Annotations: copyStringMap(annotations),
				},
				Type: corev1.SecretTypeOpaque,
				Data: copySecretData(data),
			}
			return kubeClient.Create(ctx, secret)
		}
		return fmt.Errorf("get secret %s/%s: %w", namespace, name, err)
	}

	existing.Type = corev1.SecretTypeOpaque
	existing.Annotations = copyStringMap(annotations)
	existing.Data = copySecretData(data)

	if err := kubeClient.Update(ctx, existing); err != nil {
		return fmt.Errorf("update secret %s/%s: %w", namespace, name, err)
	}

	return nil
}

func copySecretData(data map[string][]byte) map[string][]byte {
	if len(data) == 0 {
		return nil
	}

	out := make(map[string][]byte, len(data))
	for key, value := range data {
		out[key] = append([]byte(nil), value...)
	}

	return out
}

func copyStringMap(values map[string]string) map[string]string {
	if len(values) == 0 {
		return nil
	}

	out := make(map[string]string, len(values))
	for key, value := range values {
		out[key] = value
	}

	return out
}
