package cmd

import (
	"bytes"
	"context"
	"strings"
	"testing"

	"github.com/rawkode-academy/rawkode-cloud3/internal/config"
	"github.com/rawkode-academy/rawkode-cloud3/internal/scaleway"
	"github.com/spf13/cobra"
)

func restoreServerListFns() {
	loadConfigForServerListFn = loadConfigForClusterOrFile
	loadServerInventoryFn = func(ctx context.Context, cfg *config.Config) ([]scaleway.BareMetalServerInventoryItem, error) {
		accessKey, secretKey := cfg.ScalewayCredentials()
		scwClient, err := scaleway.NewClient(accessKey, secretKey, cfg.Scaleway.ProjectID, cfg.Scaleway.OrganizationID)
		if err != nil {
			return nil, err
		}
		return scaleway.ListBareMetalServerInventory(ctx, scwClient, cfg.Scaleway.OrganizationID, cfg.Scaleway.ProjectID)
	}
}

func newServerListTestCmd(clusterName, filePath, output string) *cobra.Command {
	cmd := &cobra.Command{}
	cmd.SetOut(&bytes.Buffer{})
	cmd.Flags().String("cluster", "", "")
	cmd.Flags().StringP("file", "f", "", "")
	cmd.Flags().String("output", serverListOutputTable, "")
	_ = cmd.Flags().Set("cluster", clusterName)
	_ = cmd.Flags().Set("file", filePath)
	if output != "" {
		_ = cmd.Flags().Set("output", output)
	}
	return cmd
}

func TestRootCommandRegistersServerList(t *testing.T) {
	cmd, _, err := rootCmd.Find([]string{"server", "list"})
	if err != nil {
		t.Fatalf("rootCmd.Find(server list) returned error: %v", err)
	}
	if cmd != serverListCmd {
		t.Fatalf("rootCmd.Find(server list) returned %p, want %p", cmd, serverListCmd)
	}
}

func TestRunServerListPassesClusterFlagToConfigLoader(t *testing.T) {
	restoreServerListFns()
	t.Cleanup(restoreServerListFns)

	loadConfigForServerListFn = func(clusterName, filePath string) (*config.Config, string, error) {
		if clusterName != "production" {
			t.Fatalf("clusterName = %q, want %q", clusterName, "production")
		}
		if filePath != "" {
			t.Fatalf("filePath = %q, want empty", filePath)
		}
		return &config.Config{
			Scaleway: config.ScalewayConfig{
				OrganizationID: "org-123",
				ProjectID:      "proj-456",
			},
		}, "./clusters/production.yaml", nil
	}
	loadServerInventoryFn = func(ctx context.Context, cfg *config.Config) ([]scaleway.BareMetalServerInventoryItem, error) {
		return []scaleway.BareMetalServerInventoryItem{}, nil
	}

	cmd := newServerListTestCmd("production", "", serverListOutputJSON)
	var out bytes.Buffer
	cmd.SetOut(&out)

	if err := runServerList(cmd, nil); err != nil {
		t.Fatalf("runServerList returned error: %v", err)
	}
	if strings.TrimSpace(out.String()) != "[]" {
		t.Fatalf("json output = %q, want []", out.String())
	}
}

func TestRunServerListPassesFileFlagToConfigLoader(t *testing.T) {
	restoreServerListFns()
	t.Cleanup(restoreServerListFns)

	loadConfigForServerListFn = func(clusterName, filePath string) (*config.Config, string, error) {
		if clusterName != "" {
			t.Fatalf("clusterName = %q, want empty", clusterName)
		}
		if filePath != "./clusters/production.yaml" {
			t.Fatalf("filePath = %q, want %q", filePath, "./clusters/production.yaml")
		}
		return &config.Config{
			Scaleway: config.ScalewayConfig{
				OrganizationID: "org-123",
				ProjectID:      "proj-456",
			},
		}, filePath, nil
	}
	loadServerInventoryFn = func(ctx context.Context, cfg *config.Config) ([]scaleway.BareMetalServerInventoryItem, error) {
		return []scaleway.BareMetalServerInventoryItem{}, nil
	}

	cmd := newServerListTestCmd("", "./clusters/production.yaml", serverListOutputJSON)
	if err := runServerList(cmd, nil); err != nil {
		t.Fatalf("runServerList returned error: %v", err)
	}
}

func TestRunServerListRequiresScalewayOrganizationAndProject(t *testing.T) {
	restoreServerListFns()
	t.Cleanup(restoreServerListFns)

	loadConfigForServerListFn = func(clusterName, filePath string) (*config.Config, string, error) {
		return &config.Config{}, "./clusters/production.yaml", nil
	}
	loadServerInventoryFn = func(ctx context.Context, cfg *config.Config) ([]scaleway.BareMetalServerInventoryItem, error) {
		t.Fatal("loadServerInventoryFn should not be called when scope validation fails")
		return nil, nil
	}

	cmd := newServerListTestCmd("production", "", "")
	err := runServerList(cmd, nil)
	if err == nil || !strings.Contains(err.Error(), "scaleway.organizationId is required") {
		t.Fatalf("expected organization validation error, got %v", err)
	}

	loadConfigForServerListFn = func(clusterName, filePath string) (*config.Config, string, error) {
		return &config.Config{
			Scaleway: config.ScalewayConfig{
				OrganizationID: "org-123",
			},
		}, "./clusters/production.yaml", nil
	}

	err = runServerList(cmd, nil)
	if err == nil || !strings.Contains(err.Error(), "scaleway.projectId is required") {
		t.Fatalf("expected project validation error, got %v", err)
	}
}

func TestRunServerListRendersTable(t *testing.T) {
	restoreServerListFns()
	t.Cleanup(restoreServerListFns)

	loadConfigForServerListFn = func(clusterName, filePath string) (*config.Config, string, error) {
		return &config.Config{
			Scaleway: config.ScalewayConfig{
				OrganizationID: "org-123",
				ProjectID:      "proj-456",
			},
		}, "./clusters/production.yaml", nil
	}
	loadServerInventoryFn = func(ctx context.Context, cfg *config.Config) ([]scaleway.BareMetalServerInventoryItem, error) {
		return []scaleway.BareMetalServerInventoryItem{
			{
				Name:      "alpha",
				Type:      "EM-A610R-NVMe",
				ServerID:  "srv-1",
				Zone:      "fr-par-2",
				IPAddress: "203.0.113.10",
			},
		}, nil
	}

	cmd := newServerListTestCmd("production", "", serverListOutputTable)
	var out bytes.Buffer
	cmd.SetOut(&out)

	if err := runServerList(cmd, nil); err != nil {
		t.Fatalf("runServerList returned error: %v", err)
	}

	rendered := out.String()
	if !strings.Contains(rendered, "NAME") || !strings.Contains(rendered, "SERVER ID") {
		t.Fatalf("expected table header, got %q", rendered)
	}
	if !strings.Contains(rendered, "alpha") || !strings.Contains(rendered, "srv-1") || !strings.Contains(rendered, "203.0.113.10") {
		t.Fatalf("expected inventory row, got %q", rendered)
	}
}

func TestRunServerListRendersEmptyTableMessage(t *testing.T) {
	restoreServerListFns()
	t.Cleanup(restoreServerListFns)

	loadConfigForServerListFn = func(clusterName, filePath string) (*config.Config, string, error) {
		return &config.Config{
			Scaleway: config.ScalewayConfig{
				OrganizationID: "org-123",
				ProjectID:      "proj-456",
			},
		}, "./clusters/production.yaml", nil
	}
	loadServerInventoryFn = func(ctx context.Context, cfg *config.Config) ([]scaleway.BareMetalServerInventoryItem, error) {
		return nil, nil
	}

	cmd := newServerListTestCmd("production", "", serverListOutputTable)
	var out bytes.Buffer
	cmd.SetOut(&out)

	if err := runServerList(cmd, nil); err != nil {
		t.Fatalf("runServerList returned error: %v", err)
	}
	if !strings.Contains(out.String(), "No Scaleway bare metal servers found") {
		t.Fatalf("expected empty-success message, got %q", out.String())
	}
}

func TestRunServerListRejectsUnsupportedOutput(t *testing.T) {
	cmd := newServerListTestCmd("production", "", "yaml")
	err := runServerList(cmd, nil)
	if err == nil || !strings.Contains(err.Error(), "unsupported --output") {
		t.Fatalf("expected unsupported output error, got %v", err)
	}
}
