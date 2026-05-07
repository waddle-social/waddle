package cmd

import (
	"context"
	"fmt"
	"os"
	"strconv"
	"strings"

	"github.com/scaleway/scaleway-sdk-go/scw"
	clusterstate "github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/cluster"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/scaleway"
)

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

func talosNodeFQDNSuffix() string {
	suffix := strings.Trim(strings.TrimSpace(os.Getenv("TALOS_NODE_FQDN_SUFFIX")), ".")
	if suffix == "" {
		suffix = defaultTalosNodeFQDNSuffix
	}

	return suffix
}

func nodeFQDN(nodeName string) string {
	nodeName = strings.TrimSpace(nodeName)
	if nodeName == "" {
		return ""
	}
	if strings.Contains(nodeName, ".") {
		return nodeName
	}

	suffix := talosNodeFQDNSuffix()
	if suffix == "" {
		return nodeName
	}

	return nodeName + "." + suffix
}

func canonicalKubernetesAPIEndpoint(cfg *config.Config) (string, error) {
	if cfg == nil {
		return "", fmt.Errorf("config is required")
	}

	controlPlanePool, err := cfg.FirstNodePoolByType(config.NodeTypeControlPlane)
	if err != nil {
		return "", fmt.Errorf("select default control-plane pool: %w", err)
	}

	endpoint := nodeFQDN(controlPlaneNodeName(cfg.Environment, controlPlanePool.Name, 1))
	if endpoint == "" {
		return "", fmt.Errorf("canonical kubernetes API endpoint is empty")
	}

	return endpoint, nil
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

func reinstallReservedPrivateIPs(pool *config.NodePoolConfig, requestedPrivateIP string) []string {
	values := make([]string, 0, 1)
	if pool != nil {
		values = append(values, pool.ReservedPrivateIPs...)
	}
	if trimmed := strings.TrimSpace(requestedPrivateIP); trimmed != "" {
		values = append(values, trimmed)
	}
	return normalizeNonEmptyStrings(values...)
}

func ensureReservedPrivateIPsForReinstall(
	ctx context.Context,
	client *scaleway.Client,
	zone scw.Zone,
	pool *config.NodePoolConfig,
	serverID,
	privateNetworkID,
	requestedPrivateIP string,
) error {
	for _, reservedIP := range reinstallReservedPrivateIPs(pool, requestedPrivateIP) {
		if err := scalewayEnsureReservedPrivateNetworkIPFn(
			ctx,
			client,
			zone,
			serverID,
			privateNetworkID,
			reservedIP,
		); err != nil {
			return err
		}
	}

	return nil
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
