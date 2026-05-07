package config

import "strings"

func (p NodePoolConfig) EffectiveType() string {
	if normalized := NormalizeNodePoolType(p.Type); normalized != "" {
		return normalized
	}
	return NodeTypeControlPlane
}

// DesiredSize returns the configured pool size with a sane default.
func (p NodePoolConfig) DesiredSize() int {
	if p.Size <= 0 {
		return 1
	}

	return p.Size
}

// EffectiveZone returns the node pool zone value with surrounding whitespace removed.
func (p NodePoolConfig) EffectiveZone() string {
	return strings.TrimSpace(p.Zone)
}

// EffectiveCiliumVersion returns the Cilium version with a default fallback.
func (c ClusterConfig) EffectiveCiliumVersion() string {
	if version := strings.TrimSpace(c.CiliumVersion); version != "" {
		return version
	}

	return defaultCiliumVersion
}

// EffectiveFluxVersion returns the Flux version with a default fallback.
func (c ClusterConfig) EffectiveFluxVersion() string {
	if version := strings.TrimSpace(c.FluxVersion); version != "" {
		return version
	}

	return defaultFluxVersion
}

// EffectiveControlPlaneTaints returns whether control-plane NoSchedule taints should be kept.
// Defaults to true to preserve control-plane isolation when unset.
func (c ClusterConfig) EffectiveControlPlaneTaints() bool {
	if c.ControlPlaneTaints == nil {
		return true
	}

	return *c.ControlPlaneTaints
}

func NormalizeNodePoolType(value string) string {
	normalized := compactLower(value)
	switch normalized {
	case "", strings.ReplaceAll(NodeTypeControlPlane, "-", ""), "cp":
		return NodeTypeControlPlane
	case NodeTypeWorker:
		return NodeTypeWorker
	default:
		return ""
	}
}

func compactLower(value string) string {
	trimmed := strings.ToLower(strings.TrimSpace(value))
	return strings.NewReplacer("-", "", "_", "").Replace(trimmed)
}
