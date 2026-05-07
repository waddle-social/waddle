package cmd

import (
	"fmt"
	"strings"

	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/operation"
)

func selectCreatePool(cfg *config.Config, poolName string) (*config.NodePoolConfig, error) {
	if strings.TrimSpace(poolName) != "" {
		pool, err := cfg.FindNodePool(poolName)
		if err != nil {
			return nil, fmt.Errorf("resolve --pool %q: %w", poolName, err)
		}
		if pool.EffectiveType() == "" {
			return nil, fmt.Errorf("pool %q has invalid type %q", pool.Name, pool.Type)
		}
		return pool, nil
	}

	pool, err := cfg.FirstNodePoolByType(config.NodeTypeControlPlane)
	if err != nil {
		return nil, fmt.Errorf("select default control-plane pool: %w", err)
	}
	return pool, nil
}

func poolForOperation(cfg *config.Config, op *operation.Operation) (*config.NodePoolConfig, error) {
	poolName := op.GetContextString("poolName")
	if strings.TrimSpace(poolName) != "" {
		return cfg.FindNodePool(poolName)
	}

	pool, err := cfg.FirstNodePoolByType(config.NodeTypeControlPlane)
	if err != nil {
		return nil, err
	}
	op.SetContext("poolName", pool.Name)
	return pool, nil
}

func nodeNameForOperation(op *operation.Operation, environment string) string {
	if nodeName := op.GetContextString("nodeName"); strings.TrimSpace(nodeName) != "" {
		return nodeName
	}
	poolName := strings.TrimSpace(op.GetContextString("poolName"))
	if poolName == "" {
		poolName = "control-plane"
	}
	return controlPlaneNodeName(environment, poolName, 1)
}

func controlPlaneEndpoint(privateIP, publicIP string) string {
	if ip := strings.TrimSpace(privateIP); ip != "" {
		return ip
	}
	return strings.TrimSpace(publicIP)
}
