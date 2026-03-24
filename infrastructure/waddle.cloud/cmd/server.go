package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"strings"
	"text/tabwriter"

	"github.com/spf13/cobra"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/config"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/scaleway"
)

const (
	serverListOutputTable = "table"
	serverListOutputJSON  = "json"
)

var (
	loadConfigForServerListFn = loadConfigForClusterOrFile
	loadServerInventoryFn     = func(ctx context.Context, cfg *config.Config) ([]scaleway.BareMetalServerInventoryItem, error) {
		accessKey, secretKey := cfg.ScalewayCredentials()
		scwClient, err := scaleway.NewClient(accessKey, secretKey, cfg.Scaleway.ProjectID, cfg.Scaleway.OrganizationID)
		if err != nil {
			return nil, fmt.Errorf("create scaleway client: %w", err)
		}

		return scaleway.ListBareMetalServerInventory(ctx, scwClient, cfg.Scaleway.OrganizationID, cfg.Scaleway.ProjectID)
	}
)

var serverCmd = &cobra.Command{
	Use:   "server",
	Short: "Scaleway server inventory",
}

var serverListCmd = &cobra.Command{
	Use:   "list",
	Short: "List Scaleway bare metal servers",
	RunE:  runServerList,
}

func init() {
	serverCmd.AddCommand(serverListCmd)

	serverListCmd.Flags().String("cluster", "", "Cluster/environment name")
	serverListCmd.Flags().StringP("file", "f", "", "Path to cluster config YAML")
	serverListCmd.Flags().String("output", serverListOutputTable, "Output format: table or json")
}

func runServerList(cmd *cobra.Command, args []string) error {
	clusterName, _ := cmd.Flags().GetString("cluster")
	filePath, _ := cmd.Flags().GetString("file")
	output, err := normalizedServerListOutput(cmd)
	if err != nil {
		return err
	}

	cfg, cfgPath, err := loadConfigForServerListFn(clusterName, filePath)
	if err != nil {
		return err
	}
	if err := validateServerListScope(cfg); err != nil {
		return err
	}

	items, err := loadServerInventoryFn(cmd.Context(), cfg)
	if err != nil {
		return err
	}

	switch output {
	case serverListOutputJSON:
		return renderServerListJSON(cmd.OutOrStdout(), items)
	default:
		return renderServerListTable(cmd.OutOrStdout(), cfgPath, cfg, items)
	}
}

func normalizedServerListOutput(cmd *cobra.Command) (string, error) {
	output, _ := cmd.Flags().GetString("output")
	switch strings.ToLower(strings.TrimSpace(output)) {
	case "", serverListOutputTable:
		return serverListOutputTable, nil
	case serverListOutputJSON:
		return serverListOutputJSON, nil
	default:
		return "", fmt.Errorf("unsupported --output %q (expected %q or %q)", output, serverListOutputTable, serverListOutputJSON)
	}
}

func validateServerListScope(cfg *config.Config) error {
	if cfg == nil {
		return fmt.Errorf("config is required")
	}
	if strings.TrimSpace(cfg.Scaleway.OrganizationID) == "" {
		return fmt.Errorf("scaleway.organizationId is required for server list")
	}
	if strings.TrimSpace(cfg.Scaleway.ProjectID) == "" {
		return fmt.Errorf("scaleway.projectId is required for server list")
	}
	return nil
}

func renderServerListJSON(w io.Writer, items []scaleway.BareMetalServerInventoryItem) error {
	encoder := json.NewEncoder(w)
	encoder.SetIndent("", "  ")
	return encoder.Encode(items)
}

func renderServerListTable(w io.Writer, cfgPath string, cfg *config.Config, items []scaleway.BareMetalServerInventoryItem) error {
	if len(items) == 0 {
		_, err := fmt.Fprintf(
			w,
			"No Scaleway bare metal servers found for organization %q and project %q (config=%s)\n",
			strings.TrimSpace(cfg.Scaleway.OrganizationID),
			strings.TrimSpace(cfg.Scaleway.ProjectID),
			cfgPath,
		)
		return err
	}

	tw := tabwriter.NewWriter(w, 0, 0, 2, ' ', 0)
	if _, err := fmt.Fprintln(tw, "NAME\tTYPE\tSERVER ID\tZONE\tIP ADDRESS"); err != nil {
		return err
	}
	for _, item := range items {
		if _, err := fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\n", item.Name, item.Type, item.ServerID, item.Zone, item.IPAddress); err != nil {
			return err
		}
	}
	return tw.Flush()
}
