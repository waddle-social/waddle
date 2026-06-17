package cmd

import (
	"fmt"
	"net"
	"time"

	"github.com/spf13/cobra"
	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/turnprobe"
)

// defaultTurnUDPAddr is the production LiveKit embedded-TURN UDP listener
// advertised to clients (turn.waddle.social, NodePort 30478/UDP). It is
// kept in lockstep with gitops/livekit-sfu/helmrelease.yaml.
const defaultTurnUDPAddr = "turn.waddle.social:30478"

var verifyTurnUDPCmd = &cobra.Command{
	Use:   "verify-turn-udp",
	Short: "Probe the LiveKit TURN/UDP relay for external reachability",
	Long: "Send a STUN Binding request to the LiveKit embedded-TURN UDP " +
		"listener and report whether a response returns. A success proves a " +
		"UDP datagram traversed the NodePort and node firewall to the TURN " +
		"server, so LiveKit can hand clients a usable udp relay candidate " +
		"instead of silently forcing calls onto the TCP/443 relay. " +
		"This relies on the embedded TURN server answering an unauthenticated " +
		"STUN Binding request (the base STUN method, which pion/turn serves on " +
		"the TURN port); a NOT-reachable result against a known-good data plane " +
		"may indicate that assumption changed (e.g. after a LiveKit upgrade).",
	RunE: runVerifyTurnUDP,
}

func runVerifyTurnUDP(cmd *cobra.Command, _ []string) error {
	addr, _ := cmd.Flags().GetString("addr")
	timeout, _ := cmd.Flags().GetDuration("timeout")

	reflexive, err := turnprobe.Probe(udpRoundTripper(addr, timeout))
	if err != nil {
		return fmt.Errorf("TURN/UDP relay at %s is NOT reachable: %w", addr, err)
	}

	fmt.Fprintf(cmd.OutOrStdout(),
		"TURN/UDP relay at %s is reachable; server-reflexive address %s\n", addr, reflexive)
	return nil
}

// udpRoundTripper returns a turnprobe.RoundTripper that performs one
// request/response exchange against addr over UDP, bounded by timeout.
func udpRoundTripper(addr string, timeout time.Duration) turnprobe.RoundTripper {
	return func(request []byte) ([]byte, error) {
		conn, err := net.DialTimeout("udp", addr, timeout)
		if err != nil {
			return nil, fmt.Errorf("dial udp %s: %w", addr, err)
		}
		defer conn.Close()

		if err := conn.SetDeadline(time.Now().Add(timeout)); err != nil {
			return nil, err
		}
		if _, err := conn.Write(request); err != nil {
			return nil, fmt.Errorf("write request: %w", err)
		}

		// Correlate the reply to the request: a connected UDP socket still
		// admits stray/stale datagrams from the same peer, so discard any
		// whose transaction id does not match and keep reading until the
		// deadline rather than reporting a false "unreachable".
		var want [12]byte
		copy(want[:], request[8:20])
		buf := make([]byte, 1500)
		for {
			n, err := conn.Read(buf)
			if err != nil {
				return nil, fmt.Errorf("read response: %w", err)
			}
			if n >= 20 && [12]byte(buf[8:20]) == want {
				return append([]byte(nil), buf[:n]...), nil
			}
		}
	}
}

func init() {
	verifyTurnUDPCmd.Flags().String("addr", defaultTurnUDPAddr, "host:port of the TURN/UDP listener to probe")
	verifyTurnUDPCmd.Flags().Duration("timeout", 3*time.Second, "round-trip timeout")
	rootCmd.AddCommand(verifyTurnUDPCmd)
}
