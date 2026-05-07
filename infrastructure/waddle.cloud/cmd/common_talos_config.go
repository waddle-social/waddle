package cmd

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"strings"

	clusterstate "github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/cluster"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/secrets"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/talos"
	"gopkg.in/yaml.v3"
)

func loadTalosconfigFromSecretStore(ctx context.Context, cfg *config.Config, store secrets.Store) ([]byte, error) {
	secretPath := secretPathForCluster(cfg)
	value, err := store.GetSecret(ctx, secretPath, talosConfigSecretKey)
	if err != nil {
		return nil, fmt.Errorf("load %s from secret backend: %w", talosConfigSecretKey, err)
	}
	if strings.TrimSpace(value) == "" {
		return nil, fmt.Errorf("%s is empty in secret path %s", talosConfigSecretKey, secretPath)
	}

	return []byte(value), nil
}

func controlPlaneEndpointFromState(state *clusterstate.NodesState) (string, error) {
	node, err := firstActiveNodeByRole(state, config.NodeTypeControlPlane)
	if err != nil {
		return "", err
	}
	if privateIP := strings.TrimSpace(node.PrivateIP); privateIP != "" {
		return privateIP, nil
	}
	return node.PublicIP, nil
}

func secretPathForCluster(cfg *config.Config) string {
	if cfg == nil {
		return ""
	}

	base := strings.TrimSpace(cfg.Secrets.SecretPath)
	cluster := strings.TrimSpace(cfg.Environment)

	if cluster == "" {
		return base
	}
	if base == "" || base == "/" {
		return "/" + cluster
	}
	return strings.TrimRight(base, "/") + "/" + cluster
}

func renderNodeTalosConfig(machineConfig []byte, nodeName string) ([]byte, error) {
	rendered, err := talos.WithNodeName(machineConfig, nodeName)
	if err != nil {
		return nil, err
	}

	certSANs := nodeTalosCertSANs(nodeName)
	if len(certSANs) == 0 {
		return rendered, nil
	}

	rendered, err = talos.WithCertSANs(rendered, certSANs...)
	if err != nil {
		return nil, err
	}

	return rendered, nil
}

func nodeTalosCertSANs(nodeName string) []string {
	nodeName = strings.TrimSpace(nodeName)
	if nodeName == "" {
		return nil
	}

	sans := []string{nodeName}
	if fqdn := nodeFQDN(nodeName); fqdn != "" && fqdn != nodeName {
		sans = append(sans, fqdn)
	}

	return sans
}

func netbirdSetupKeyLookupTargets(cfg *config.Config, secretPathOverride, secretKeyOverride string) (string, []string) {
	secretPath := strings.TrimSpace(secretPathOverride)
	if secretPath == "" && cfg != nil {
		secretPath = strings.TrimSpace(cfg.Secrets.NetbirdSecretPath)
	}
	if secretPath == "" && cfg != nil {
		secretPath = strings.TrimSpace(cfg.Secrets.SecretPath)
	}
	if secretPath == "" {
		secretPath = secretPathForCluster(cfg)
	}

	secretKey := strings.TrimSpace(secretKeyOverride)
	if secretKey == "" && cfg != nil {
		secretKey = strings.TrimSpace(cfg.Secrets.NetbirdSecretKey)
	}
	if secretKey != "" {
		return secretPath, []string{secretKey}
	}

	return secretPath, []string{netbirdSetupKeyPrimary, netbirdSetupKeyCompatibility}
}

func loadOptionalNetbirdSetupKeyFromSecretStoreWithOverrides(
	ctx context.Context,
	cfg *config.Config,
	store secrets.Store,
	secretPathOverride,
	secretKeyOverride string,
) (string, error) {
	secretPath, keyCandidates := netbirdSetupKeyLookupTargets(cfg, secretPathOverride, secretKeyOverride)
	all, err := store.GetSecrets(ctx, secretPath)
	if err != nil {
		return "", fmt.Errorf("load secrets from backend: %w", err)
	}

	for _, key := range keyCandidates {
		if value := strings.TrimSpace(all[key]); value != "" {
			return value, nil
		}
	}

	return "", nil
}

func loadOptionalNetbirdSetupKeyFromSecretStore(ctx context.Context, cfg *config.Config, store secrets.Store) (string, error) {
	return loadOptionalNetbirdSetupKeyFromSecretStoreWithOverrides(ctx, cfg, store, "", "")
}

func appendNetbirdExtensionServiceConfig(machineConfig []byte, setupKey string) ([]byte, error) {
	setupKey = strings.TrimSpace(setupKey)
	if setupKey == "" {
		return machineConfig, nil
	}

	doc := map[string]any{
		"apiVersion": "v1alpha1",
		"kind":       "ExtensionServiceConfig",
		"name":       "netbird",
		"environment": []string{
			"NB_SETUP_KEY=" + setupKey,
		},
	}
	rawDoc, err := yaml.Marshal(doc)
	if err != nil {
		return nil, fmt.Errorf("encode netbird extension service config: %w", err)
	}

	return appendTalosConfigDocuments(machineConfig, rawDoc)
}

func appendTalosAPIIngressRestriction(machineConfig []byte, allowedSubnets []string) ([]byte, error) {
	allowedSubnets = normalizeNonEmptyStrings(allowedSubnets...)
	if len(allowedSubnets) == 0 {
		return machineConfig, nil
	}

	ingress := make([]map[string]string, 0, len(allowedSubnets))
	for _, subnet := range allowedSubnets {
		ingress = append(ingress, map[string]string{"subnet": subnet})
	}

	doc := map[string]any{
		"apiVersion": "v1alpha1",
		"kind":       "NetworkRuleConfig",
		"name":       "ingress-apid",
		"portSelector": map[string]any{
			"ports":    []int{50000},
			"protocol": "tcp",
		},
		"ingress": ingress,
	}
	rawDoc, err := yaml.Marshal(doc)
	if err != nil {
		return nil, fmt.Errorf("encode Talos API network rule config: %w", err)
	}

	return appendTalosConfigDocuments(machineConfig, rawDoc)
}

func talosAPIAllowedSubnets() []string {
	if configured := strings.TrimSpace(os.Getenv(envTalosAllowedSubnets)); configured != "" {
		parts := strings.Split(configured, ",")
		return normalizeNonEmptyStrings(parts...)
	}

	return []string{defaultTalosAPINetbirdSubnet}
}

func appendTalosConfigDocuments(machineConfig []byte, docs ...[]byte) ([]byte, error) {
	base := bytes.TrimSpace(machineConfig)
	if len(base) == 0 {
		return nil, fmt.Errorf("machine config is required")
	}

	var out bytes.Buffer
	out.Write(base)
	for _, doc := range docs {
		trimmed := bytes.TrimSpace(doc)
		if len(trimmed) == 0 {
			continue
		}

		out.WriteString("\n---\n")
		out.Write(trimmed)
	}
	out.WriteByte('\n')

	return out.Bytes(), nil
}
