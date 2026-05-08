package cmd

import (
	"fmt"
	"strings"

	baremetal "github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
	clusterstate "github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/cluster"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
)

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
