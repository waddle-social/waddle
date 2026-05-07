package talos

import (
	"context"
	"crypto/tls"
	"fmt"
	"log/slog"
	"strings"

	machineapi "github.com/siderolabs/talos/pkg/machinery/api/machine"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
)

const talosAPIDefaultPort = "50000"

type Client struct {
	targetNode string
	insecure   bool

	conn    *grpc.ClientConn
	machine machineapi.MachineServiceClient
}

func NewClient(endpoint string, talosconfig []byte) (*Client, error) {
	dialEndpoint, targetNode, err := normalizeTalosEndpoint(endpoint)
	if err != nil {
		return nil, err
	}
	if len(talosconfig) == 0 {
		return nil, fmt.Errorf("talosconfig is required")
	}

	ctxCfg, err := parseTalosconfigContext(talosconfig)
	if err != nil {
		return nil, err
	}

	tlsConfig, err := tlsConfigFromContext(ctxCfg, false)
	if err != nil {
		return nil, err
	}

	conn, err := grpc.NewClient(
		"dns:///"+dialEndpoint,
		grpc.WithTransportCredentials(credentials.NewTLS(tlsConfig)),
		grpc.WithDefaultCallOptions(grpc.MaxCallRecvMsgSize(32*1024*1024)),
	)
	if err != nil {
		return nil, fmt.Errorf("connect to Talos API %q: %w", dialEndpoint, err)
	}

	return &Client{
		targetNode: targetNode,
		conn:       conn,
		machine:    machineapi.NewMachineServiceClient(conn),
	}, nil
}

// NewInsecureClient creates a Talos client for maintenance mode.
func NewInsecureClient(endpoint string) (*Client, error) {
	dialEndpoint, targetNode, err := normalizeTalosEndpoint(endpoint)
	if err != nil {
		return nil, err
	}

	conn, err := grpc.NewClient(
		"dns:///"+dialEndpoint,
		grpc.WithTransportCredentials(credentials.NewTLS(&tls.Config{InsecureSkipVerify: true})), //nolint:gosec
		grpc.WithDefaultCallOptions(grpc.MaxCallRecvMsgSize(32*1024*1024)),
	)
	if err != nil {
		return nil, fmt.Errorf("connect to Talos API %q: %w", dialEndpoint, err)
	}

	return &Client{
		targetNode: targetNode,
		insecure:   true,
		conn:       conn,
		machine:    machineapi.NewMachineServiceClient(conn),
	}, nil
}

// Close closes the underlying Talos gRPC connection.
func (c *Client) Close() error {
	if c == nil || c.conn == nil {
		return nil
	}

	return c.conn.Close()
}

// ApplyConfig sends a machine configuration to a Talos node.
func (c *Client) ApplyConfig(ctx context.Context, configYAML []byte) error {
	if c.machine == nil {
		return fmt.Errorf("talos client is not initialized")
	}
	if len(strings.TrimSpace(string(configYAML))) == 0 {
		return fmt.Errorf("machine config is required")
	}

	slog.Info("applying Talos machine config", "target", c.targetNode)

	_, err := c.machine.ApplyConfiguration(ctx, &machineapi.ApplyConfigurationRequest{
		Data: configYAML,
		Mode: machineapi.ApplyConfigurationRequest_AUTO,
	})
	if err != nil {
		return fmt.Errorf("apply talos machine config: %w", err)
	}

	return nil
}

// Bootstrap bootstraps etcd on the first control plane node.
func (c *Client) Bootstrap(ctx context.Context) error {
	if c.machine == nil {
		return fmt.Errorf("talos client is not initialized")
	}
	if c.insecure {
		return fmt.Errorf("bootstrap requires talosconfig")
	}

	slog.Info("bootstrapping etcd", "target", c.targetNode)

	if _, err := c.machine.Bootstrap(ctx, &machineapi.BootstrapRequest{}); err != nil {
		return fmt.Errorf("bootstrap etcd: %w", err)
	}

	return nil
}

// Kubeconfig retrieves the Kubernetes kubeconfig from a Talos control plane node.

func (c *Client) Upgrade(ctx context.Context, imageURL string) error {
	if c.machine == nil {
		return fmt.Errorf("talos client is not initialized")
	}
	if c.insecure {
		return fmt.Errorf("upgrade requires talosconfig")
	}
	if strings.TrimSpace(imageURL) == "" {
		return fmt.Errorf("image URL is required")
	}

	_, err := c.machine.Upgrade(ctx, &machineapi.UpgradeRequest{
		Image: imageURL,
	})
	if err != nil {
		return fmt.Errorf("upgrade failed: %w", err)
	}

	return nil
}

// UpgradeKubernetes triggers a cluster Kubernetes control-plane version upgrade.
func (c *Client) UpgradeKubernetes(ctx context.Context, version string) error {
	_ = ctx
	_ = version
	return fmt.Errorf("kubernetes upgrades are orchestrated by regenerating and applying machine configs")
}

// Reset resets a Talos node.
func (c *Client) Reset(ctx context.Context) error {
	if c.machine == nil {
		return fmt.Errorf("talos client is not initialized")
	}

	req := &machineapi.ResetRequest{
		Graceful: true,
		Reboot:   true,
	}

	if c.insecure {
		req.Graceful = false
		req.Reboot = false
		req.Mode = machineapi.ResetRequest_SYSTEM_DISK
		req.SystemPartitionsToWipe = []*machineapi.ResetPartitionSpec{
			{
				Label: "STATE",
				Wipe:  true,
			},
		}
	}

	if _, err := c.machine.Reset(ctx, req); err != nil {
		return fmt.Errorf("reset failed: %w", err)
	}

	return nil
}
