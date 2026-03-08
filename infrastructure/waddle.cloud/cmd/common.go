package cmd

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	baremetal "github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
	scw "github.com/scaleway/scaleway-sdk-go/scw"
	clusterstate "github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/cluster"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/scaleway"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/secrets"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/talos"
	"gopkg.in/yaml.v3"
)

const (
	defaultTalosNodeFQDNSuffix   = "rka.internal"
	defaultTalosAPINetbirdSubnet = "100.64.0.0/10"
	envTalosAllowedSubnets       = "TALOS_API_ALLOWED_SUBNETS"
	netbirdSetupKeyPrimary       = "NB_SETUP_KEY"
	netbirdSetupKeyCompatibility = "NETBIRD_SETUP_KEY"
	managedServerTagManaged      = "rawkode-cloud3:managed"
	managedServerTagEnvPrefix    = "rawkode-cloud3:env="
	managedServerTagPoolPrefix   = "rawkode-cloud3:pool="
	managedServerTagRolePrefix   = "rawkode-cloud3:role="
)

func loadConfigForClusterOrFile(clusterName, filePath string) (*config.Config, string, error) {
	resolved, err := resolveConfigPath(clusterName, filePath)
	if err != nil {
		return nil, "", err
	}

	cfg, err := config.Load(resolved)
	if err != nil {
		return nil, "", fmt.Errorf("load config %s: %w", resolved, err)
	}

	secretStore, err := getOrCreateSecretStore(context.Background(), cfg)
	if err != nil {
		return nil, "", fmt.Errorf("create secret store: %w", err)
	}
	if err := cfg.LoadRuntimeSecretsWithStore(context.Background(), secretStore); err != nil {
		return nil, "", fmt.Errorf("load runtime secrets: %w", err)
	}

	return cfg, resolved, nil
}

func resolveConfigPath(clusterName, filePath string) (string, error) {
	if p := strings.TrimSpace(filePath); p != "" {
		if _, err := os.Stat(p); err != nil {
			return "", fmt.Errorf("config file %s: %w", p, err)
		}
		return p, nil
	}

	clusterName = strings.TrimSpace(clusterName)
	if clusterName == "" {
		return "", fmt.Errorf("either --file or --cluster is required")
	}

	candidates := []string{
		clusterName + ".yaml",
		filepath.Join("clusters", clusterName+".yaml"),
	}

	for _, candidate := range candidates {
		if _, err := os.Stat(candidate); err == nil {
			return candidate, nil
		}
	}

	return "", fmt.Errorf("could not find config for cluster %q (checked %s)", clusterName, strings.Join(candidates, ", "))
}

func loadNodeState(ctx context.Context, cfg *config.Config) (*clusterstate.NodesState, error) {
	if cfg == nil {
		return nil, fmt.Errorf("config is required")
	}

	accessKey, secretKey := cfg.ScalewayCredentials()
	scwClient, err := scaleway.NewClient(accessKey, secretKey, cfg.Scaleway.ProjectID, cfg.Scaleway.OrganizationID)
	if err != nil {
		return nil, fmt.Errorf("create scaleway client: %w", err)
	}

	now := time.Now().UTC()
	seen := map[string]struct{}{}
	nodes := make([]clusterstate.NodeState, 0, len(cfg.NodePools))
	for i := range cfg.NodePools {
		pool := &cfg.NodePools[i]
		zoneValue := strings.TrimSpace(pool.EffectiveZone())
		if zoneValue == "" {
			return nil, fmt.Errorf("node pool %q must define zone", pool.Name)
		}

		req := &baremetal.ListServersRequest{
			Zone: scw.Zone(zoneValue),
		}
		if projectID := strings.TrimSpace(cfg.Scaleway.ProjectID); projectID != "" {
			req.ProjectID = &projectID
		}

		resp, err := scwClient.Baremetal.ListServers(req, scw.WithAllPages(), scw.WithContext(ctx))
		if err != nil {
			return nil, fmt.Errorf("list scaleway servers for pool %q in zone %q: %w", pool.Name, zoneValue, err)
		}

		for _, server := range resp.Servers {
			if server == nil {
				continue
			}
			if !serverBelongsToPool(cfg.Environment, pool, strings.TrimSpace(server.Name), server.Tags) {
				continue
			}
			if strings.TrimSpace(server.ID) == "" {
				continue
			}
			if _, exists := seen[server.ID]; exists {
				continue
			}
			seen[server.ID] = struct{}{}

			publicIP, privateIP := scaleway.ExtractServerIPs(server)
			role := nodeRoleForServer(pool, server.Tags)
			status := nodeStatusFromServerStatus(server.Status)
			nodes = append(nodes, clusterstate.NodeState{
				Name:      strings.TrimSpace(server.Name),
				Role:      role,
				Pool:      pool.Name,
				PublicIP:  publicIP,
				PrivateIP: privateIP,
				ServerID:  strings.TrimSpace(server.ID),
				Status:    status,
				CreatedAt: now,
				UpdatedAt: now,
			})
		}
	}

	return &clusterstate.NodesState{
		Environment: cfg.Environment,
		UpdatedAt:   now,
		Nodes:       nodes,
	}, nil
}

func serverBelongsToPool(environment string, pool *config.NodePoolConfig, nodeName string, serverTags []string) bool {
	if pool == nil {
		return false
	}

	if serverMatchesManagedPoolTags(serverTags, environment, pool.Name) {
		return true
	}

	return serverNameBelongsToPool(environment, pool, nodeName)
}

func serverNameBelongsToPool(environment string, pool *config.NodePoolConfig, nodeName string) bool {
	if pool == nil {
		return false
	}

	nodeName = strings.TrimSpace(nodeName)
	if nodeName == "" {
		return false
	}

	if _, ok := parsePooledNodeSlot(environment, pool.Name, nodeName); ok {
		return true
	}

	poolName := strings.TrimSpace(pool.Name)
	if poolName == "" {
		return false
	}

	prefixes := make([]string, 0, 2)
	if env := strings.TrimSpace(environment); env != "" {
		prefixes = append(prefixes, env+"-"+poolName+"-")
	}
	prefixes = append(prefixes, poolName+"-")

	for _, prefix := range prefixes {
		if strings.HasPrefix(nodeName, prefix) {
			return true
		}
	}

	return false
}

func nodeRoleForServer(pool *config.NodePoolConfig, serverTags []string) string {
	if role, ok := managedServerRoleTagValue(serverTags); ok {
		return role
	}
	if pool != nil {
		return pool.EffectiveType()
	}
	return ""
}

func managedServerTags(environment, poolName, role string) []string {
	tags := []string{
		managedServerTagManaged,
		managedServerTagEnvPrefix + strings.TrimSpace(environment),
		managedServerTagPoolPrefix + strings.TrimSpace(poolName),
	}
	if normalizedRole := config.NormalizeNodePoolType(role); normalizedRole != "" {
		tags = append(tags, managedServerTagRolePrefix+normalizedRole)
	}

	return normalizeNonEmptyStrings(tags...)
}

func serverMatchesManagedPoolTags(serverTags []string, environment, poolName string) bool {
	managed, env, pool, _ := parseManagedServerTags(serverTags)
	if !managed {
		return false
	}
	if strings.TrimSpace(environment) == "" || strings.TrimSpace(poolName) == "" {
		return false
	}
	return env == strings.TrimSpace(environment) && pool == strings.TrimSpace(poolName)
}

func managedServerRoleTagValue(serverTags []string) (string, bool) {
	managed, _, _, role := parseManagedServerTags(serverTags)
	if !managed {
		return "", false
	}
	normalizedRole := config.NormalizeNodePoolType(role)
	if normalizedRole == "" {
		return "", false
	}
	return normalizedRole, true
}

func parseManagedServerTags(serverTags []string) (managed bool, environment, pool, role string) {
	for _, tag := range serverTags {
		trimmed := strings.TrimSpace(tag)
		if trimmed == managedServerTagManaged {
			managed = true
			continue
		}
		if strings.HasPrefix(trimmed, managedServerTagEnvPrefix) {
			environment = strings.TrimSpace(strings.TrimPrefix(trimmed, managedServerTagEnvPrefix))
			continue
		}
		if strings.HasPrefix(trimmed, managedServerTagPoolPrefix) {
			pool = strings.TrimSpace(strings.TrimPrefix(trimmed, managedServerTagPoolPrefix))
			continue
		}
		if strings.HasPrefix(trimmed, managedServerTagRolePrefix) {
			role = strings.TrimSpace(strings.TrimPrefix(trimmed, managedServerTagRolePrefix))
			continue
		}
	}

	return managed, environment, pool, role
}

func mergeServerTags(existing, desired []string) []string {
	return normalizeNonEmptyStrings(append(existing, desired...)...)
}

func sameStringSet(valuesA, valuesB []string) bool {
	if len(valuesA) != len(valuesB) {
		return false
	}

	seen := make(map[string]struct{}, len(valuesA))
	for _, value := range valuesA {
		seen[value] = struct{}{}
	}
	for _, value := range valuesB {
		if _, ok := seen[value]; !ok {
			return false
		}
	}
	return true
}

func validateReinstallFlags(serverID string, confirmReinstall bool) error {
	serverID = strings.TrimSpace(serverID)
	switch {
	case serverID == "" && !confirmReinstall:
		return nil
	case serverID == "":
		return fmt.Errorf("--confirm-reinstall requires --server-id")
	case !confirmReinstall:
		return fmt.Errorf("--server-id requires --confirm-reinstall")
	default:
		return nil
	}
}

func nodeStatusFromServerStatus(status baremetal.ServerStatus) clusterstate.NodeStatus {
	switch status {
	case baremetal.ServerStatusReady:
		return clusterstate.NodeStatusReady
	case baremetal.ServerStatusDeleting:
		return clusterstate.NodeStatusDeleted
	case baremetal.ServerStatusError, baremetal.ServerStatusLocked, baremetal.ServerStatusOutOfStock:
		return clusterstate.NodeStatusFailed
	default:
		return clusterstate.NodeStatusProvisioning
	}
}

func findNodeByName(state *clusterstate.NodesState, name string) (*clusterstate.NodeState, bool) {
	for i := range state.Nodes {
		if state.Nodes[i].Name == name {
			return &state.Nodes[i], true
		}
	}
	return nil, false
}

func firstActiveNodeByRole(state *clusterstate.NodesState, role string) (*clusterstate.NodeState, error) {
	for i := range state.Nodes {
		node := &state.Nodes[i]
		if node.Role != role {
			continue
		}
		if node.Status == clusterstate.NodeStatusDeleted || node.Status == clusterstate.NodeStatusFailed {
			continue
		}
		if strings.TrimSpace(node.PublicIP) == "" && strings.TrimSpace(node.PrivateIP) == "" {
			continue
		}
		return node, nil
	}

	return nil, fmt.Errorf("no active %s node with reachable IP found in Scaleway inventory", role)
}

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

	suffix := strings.TrimSpace(os.Getenv("TALOS_NODE_FQDN_SUFFIX"))
	if suffix == "" {
		suffix = defaultTalosNodeFQDNSuffix
	}
	suffix = strings.Trim(strings.TrimSpace(suffix), ".")

	sans := []string{nodeName}
	if suffix != "" && !strings.Contains(nodeName, ".") {
		sans = append(sans, nodeName+"."+suffix)
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

func normalizeNonEmptyStrings(values ...string) []string {
	seen := make(map[string]struct{}, len(values))
	out := make([]string, 0, len(values))
	for _, value := range values {
		trimmed := strings.TrimSpace(value)
		if trimmed == "" {
			continue
		}
		if _, exists := seen[trimmed]; exists {
			continue
		}

		seen[trimmed] = struct{}{}
		out = append(out, trimmed)
	}

	return out
}

func pooledNodeName(environment, poolName string, slot int) string {
	namePrefix := strings.TrimSpace(poolName)
	if env := strings.TrimSpace(environment); env != "" {
		namePrefix = env + "-" + namePrefix
	}

	return fmt.Sprintf("%s-%02d", namePrefix, slot)
}

func parsePooledNodeSlot(environment, poolName, nodeName string) (int, bool) {
	nodeName = strings.TrimSpace(nodeName)
	poolName = strings.TrimSpace(poolName)

	prefixes := make([]string, 0, 2)
	if env := strings.TrimSpace(environment); env != "" {
		prefixes = append(prefixes, env+"-"+poolName+"-")
	}
	// Backward compatibility for existing nodes named as "<pool>-NN".
	prefixes = append(prefixes, poolName+"-")

	for _, prefix := range prefixes {
		if !strings.HasPrefix(nodeName, prefix) {
			continue
		}

		suffix := strings.TrimPrefix(nodeName, prefix)
		if len(suffix) != 2 {
			continue
		}

		slot, err := strconv.Atoi(suffix)
		if err == nil && slot > 0 {
			return slot, true
		}
	}

	return 0, false
}

func controlPlaneNodeName(environment, poolName string, slot int) string {
	return pooledNodeName(environment, poolName, slot)
}

func parseControlPlaneSlot(environment, poolName, nodeName string) (int, bool) {
	return parsePooledNodeSlot(environment, poolName, nodeName)
}

func controlPlaneReservedIPForSlot(pool *config.NodePoolConfig, slot int) (string, error) {
	if pool == nil {
		return "", fmt.Errorf("node pool is required")
	}

	if len(pool.ReservedPrivateIPs) == 0 {
		return "", nil
	}

	if slot <= 0 || slot > len(pool.ReservedPrivateIPs) {
		return "", fmt.Errorf(
			"control-plane slot %d exceeds reservedPrivateIPs for pool %q (defined=%d)",
			slot, pool.Name, len(pool.ReservedPrivateIPs),
		)
	}

	return strings.TrimSpace(pool.ReservedPrivateIPs[slot-1]), nil
}

func nextNodePoolSlot(state *clusterstate.NodesState, environment, poolName, role string) int {
	occupied := make(map[int]struct{})
	unknownNamedNodes := 0

	for _, node := range state.Nodes {
		if node.Pool != poolName {
			continue
		}
		if strings.TrimSpace(role) != "" && node.Role != role {
			continue
		}
		if node.Status == clusterstate.NodeStatusDeleted {
			continue
		}
		slot, ok := parsePooledNodeSlot(environment, poolName, node.Name)
		if !ok {
			unknownNamedNodes++
			continue
		}
		occupied[slot] = struct{}{}
	}

	for slot := 1; slot <= 99 && unknownNamedNodes > 0; slot++ {
		if _, used := occupied[slot]; used {
			continue
		}
		occupied[slot] = struct{}{}
		unknownNamedNodes--
	}

	for slot := 1; slot <= 99; slot++ {
		if _, used := occupied[slot]; !used {
			return slot
		}
	}

	return 100
}

func nextControlPlaneSlot(state *clusterstate.NodesState, environment, poolName string) int {
	return nextNodePoolSlot(state, environment, poolName, config.NodeTypeControlPlane)
}
