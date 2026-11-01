package cmd

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"net/netip"
	"os"
	"time"

	"github.com/spf13/cobra"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/turnprobe"
)

const defaultTurnUDPAddr = "turn.waddle.social:30478"

var verifyTurnUDPCmd = newVerifyTurnUDPCmd()

func newVerifyTurnUDPCmd() *cobra.Command {
	command := &cobra.Command{
		Use:   "verify-turn-udp",
		Short: "Qualify authenticated TURN relay over UDP or TLS",
		Long: "Allocate an authenticated UDP relay, create a permission and channel, " +
			"and verify bidirectional payload echo through TURN. This requires an " +
			"explicit controlled UDP echo peer allowed by the qualification server. " +
			"The production SFU media port is not an echo service; use real calls " +
			"to qualify the production policy. Credentials come only from " +
			"TURN_USERNAME and TURN_PASSWORD. TLS certificates are verified.",
		Args: cobra.NoArgs,
		RunE: runVerifyTurnUDP,
	}
	command.Flags().String("addr", defaultTurnUDPAddr, "TURN host:port (TLS default: turn.waddle.social:443)")
	command.Flags().String("transport", "udp", "client-to-TURN transport: udp or tls")
	command.Flags().String("peer", "", "explicit controlled UDP echo peer IP:port in an isolated qualification environment")
	command.Flags().String("ca", "", "additional PEM CA certificate for an isolated TLS qualification server")
	command.Flags().Duration("timeout", 3*time.Second, "total probe deadline, greater than zero and at most 10s")
	return command
}

func runVerifyTurnUDP(cmd *cobra.Command, _ []string) error {
	timeout, _ := cmd.Flags().GetDuration("timeout")
	if timeout <= 0 || timeout > 10*time.Second {
		return errors.New("timeout must be greater than zero and at most 10s")
	}
	username, password := os.Getenv("TURN_USERNAME"), os.Getenv("TURN_PASSWORD")
	if username == "" || password == "" {
		return errors.New("TURN_USERNAME and TURN_PASSWORD environment variables are required")
	}
	peerText, _ := cmd.Flags().GetString("peer")
	peer, err := netip.ParseAddrPort(peerText)
	if err != nil {
		return errors.New("peer must name an explicit controlled UDP echo peer IP:port")
	}
	transportText, _ := cmd.Flags().GetString("transport")
	transport := turnprobe.Transport(transportText)
	if transport != turnprobe.UDP && transport != turnprobe.TLS {
		return errors.New("transport must be udp or tls")
	}
	address, _ := cmd.Flags().GetString("addr")
	if transport == turnprobe.TLS && !cmd.Flags().Changed("addr") {
		address = "turn.waddle.social:443"
	}
	config := turnprobe.RelayConfig{ServerAddr: address, Transport: transport, Username: username, Password: password, Peer: peer}
	caFile, _ := cmd.Flags().GetString("ca")
	if caFile != "" {
		if transport != turnprobe.TLS {
			return errors.New("ca is only valid for TLS transport")
		}
		pem, err := os.ReadFile(caFile)
		if err != nil {
			return fmt.Errorf("read TURN qualification CA: %w", err)
		}
		roots, err := x509.SystemCertPool()
		if err != nil {
			return fmt.Errorf("read system TLS roots: %w", err)
		}
		if !roots.AppendCertsFromPEM(pem) {
			return errors.New("TURN qualification CA has no valid PEM certificates")
		}
		config.TLSConfig = &tls.Config{RootCAs: roots, MinVersion: tls.VersionTLS12}
	}
	ctx, cancel := context.WithTimeout(cmd.Context(), timeout)
	defer cancel()
	result, err := turnprobe.VerifyRelay(ctx, config)
	if err != nil {
		return fmt.Errorf("TURN/%s authenticated bidirectional relay at %s was NOT verified: %w", transport, address, err)
	}
	_, err = fmt.Fprintf(cmd.OutOrStdout(), "TURN/%s authenticated bidirectional relay at %s verified through %s to controlled peer %s\n", transport, address, result.RelayedAddr, peer)
	return err
}

func init() {
	rootCmd.AddCommand(verifyTurnUDPCmd)
}
