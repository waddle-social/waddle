package turnprobe

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"net"
	"net/netip"
	"os"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func TestVerifyRelayRequiresCredentialsAndControlledPeer(t *testing.T) {
	for _, config := range []RelayConfig{
		{ServerAddr: "127.0.0.1:3478", Transport: UDP},
		{ServerAddr: "127.0.0.1:3478", Transport: UDP, Username: "user", Password: "never-log-this"},
	} {
		_, err := VerifyRelay(context.Background(), config)
		if err == nil {
			t.Fatal("incomplete relay qualification must not report success")
		}
		if strings.Contains(err.Error(), "never-log-this") {
			t.Fatal("credential leaked in probe error")
		}
	}
}

func TestVerifyRelayBoundsUnresponsiveUDPServer(t *testing.T) {
	server, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer server.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()
	started := time.Now()
	_, err = VerifyRelay(ctx, RelayConfig{ServerAddr: server.LocalAddr().String(), Transport: UDP, Username: "user", Password: "password", Peer: netip.MustParseAddrPort("192.0.2.10:9999")})
	if err == nil {
		t.Fatal("an unresponsive listener must not pass authenticated relay verification")
	}
	if time.Since(started) > time.Second {
		t.Fatal("probe exceeded its context deadline")
	}
}

func TestVerifyRelayHonorsCancellationBeforeDeadline(t *testing.T) {
	server, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer server.Close()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go func() {
		buffer := make([]byte, 1500)
		_, _, _ = server.ReadFrom(buffer)
		cancel()
	}()
	started := time.Now()
	_, err = VerifyRelay(ctx, RelayConfig{ServerAddr: server.LocalAddr().String(), Transport: UDP, Username: "user", Password: "password", Peer: netip.MustParseAddrPort("192.0.2.10:9999")})
	if err == nil || time.Since(started) > time.Second {
		t.Fatalf("cancelled qualification must fail immediately, got %v after %s", err, time.Since(started))
	}
}

func TestTURNResponseIntegrityUsesRFC5769Vector(t *testing.T) {
	// RFC 5769 §2.2: independent HMAC and XOR-MAPPED-ADDRESS test vector.
	packet, err := hex.DecodeString("0101003c2112a442b7e7a701bc34d686fa87dfae8022000b7465737420766563746f7220002000080001a147e112a643000800142b91f599fd9e90c38c7489f92af9ba53f06be7d780280004c07d4c96")
	if err != nil {
		t.Fatal(err)
	}
	if err := verifyIntegrity(packet, []byte("VOkJxbRl1RmTxUk/WvJxBt")); err != nil {
		t.Fatal(err)
	}
	packet[35] ^= 1
	if err := verifyIntegrity(packet, []byte("VOkJxbRl1RmTxUk/WvJxBt")); err == nil {
		t.Fatal("tampered authenticated response must be rejected")
	}
}

func TestTURNResponseRejectsUnauthenticatedTrailingAttributes(t *testing.T) {
	packet := buildMessage(allocate|0x0100, TxID{}, nil, []byte("key"))
	// The injected peer attribute is outside the HMAC-covered prefix.
	packet = append(packet, 0, byte(attrRelay), 0, 8, 0, 1, 0, 1, 0, 0, 0, 0)
	binary.BigEndian.PutUint16(packet[2:], uint16(len(packet)-headerLen))
	if err := verifyIntegrity(packet, []byte("key")); err == nil {
		t.Fatal("protocol attributes after MESSAGE-INTEGRITY must not become trusted")
	}
}

func TestAuthenticatedTURNStaleNonceChallenges(t *testing.T) {
	for _, scenario := range []struct {
		name      string
		signed    bool
		tampered  bool
		repeat    bool
		wantError bool
	}{
		{name: "unsigned438-retries"},
		{name: "signed438-retries", signed: true},
		{name: "tampered-signed438-refused", signed: true, tampered: true, wantError: true},
		{name: "repeated438-is-bounded", repeat: true, wantError: true},
	} {
		t.Run(scenario.name, func(t *testing.T) {
			client, server := net.Pipe()
			defer client.Close()
			defer server.Close()
			deadline := time.Now().Add(time.Second)
			_ = client.SetDeadline(deadline)
			_ = server.SetDeadline(deadline)
			key := []byte("qualification-key")
			realm := []byte("turn.example.test")
			session := relaySession{conn: client, stream: true, deadline: deadline, username: "user", realm: realm, nonce: []byte("old-nonce"), key: key}
			serverResult := make(chan error, 1)
			go func() {
				wire := relaySession{conn: server, stream: true, deadline: deadline}
				for attempt := 0; attempt < 2; attempt++ {
					packet, err := wire.readFrame()
					if err != nil {
						serverResult <- err
						return
					}
					request, err := parseMessage(packet)
					if err != nil {
						serverResult <- err
						return
					}
					if attempt == 1 && string(request.value(attrNonce)) != "fresh-nonce" {
						serverResult <- fmt.Errorf("nonce retry did not use the replacement challenge")
						return
					}
					attrs := []attribute{{attrError, []byte{0, 0, 4, 38}}, {attrRealm, realm}, {attrNonce, []byte("fresh-nonce")}}
					kind := createPermission | 0x0110
					var signingKey []byte
					if scenario.signed {
						signingKey = key
					}
					if attempt == 1 && !scenario.repeat {
						kind, attrs, signingKey = createPermission|0x0100, nil, key
					}
					response := buildMessage(kind, request.txID, attrs, signingKey)
					if scenario.tampered {
						response[len(response)-1] ^= 1
					}
					if err := wire.writeFrame(response); err != nil {
						serverResult <- err
						return
					}
					if scenario.tampered {
						break
					}
				}
				serverResult <- nil
			}()
			_, err := session.authenticated(createPermission, nil)
			if (err != nil) != scenario.wantError {
				t.Fatalf("stale nonce result: %v, want error=%v", err, scenario.wantError)
			}
			if serverErr := <-serverResult; serverErr != nil {
				t.Fatal(serverErr)
			}
		})
	}
}

// These are real coturn tests, not a synthetic TURN implementation. The
// qualification fixture supplies an isolated, explicitly allowed UDP echo
// peer: the production SFU media port is intentionally not a raw echo service.
func TestVerifyRelayAgainstCoturn(t *testing.T) {
	peerText := os.Getenv("TURNPROBE_TEST_PEER")
	if peerText == "" {
		t.Skip("set TURNPROBE_TEST_PEER and TURNPROBE_TEST_UDP_ADDR/TLS_ADDR for real coturn qualification")
	}
	peer, err := netip.ParseAddrPort(peerText)
	if err != nil {
		t.Fatal(err)
	}
	for _, transport := range []Transport{UDP, TLS} {
		t.Run(string(transport), func(t *testing.T) {
			address := os.Getenv("TURNPROBE_TEST_" + strings.ToUpper(string(transport)) + "_ADDR")
			if address == "" {
				t.Fatal("missing real coturn address for " + string(transport))
			}
			config := RelayConfig{ServerAddr: address, Transport: transport, Username: os.Getenv("TURNPROBE_TEST_USERNAME"), Password: os.Getenv("TURNPROBE_TEST_PASSWORD"), Peer: peer}
			if transport == TLS {
				pem, err := os.ReadFile(os.Getenv("TURNPROBE_TEST_CA"))
				if err != nil {
					t.Fatal(err)
				}
				roots := x509.NewCertPool()
				if !roots.AppendCertsFromPEM(pem) {
					t.Fatal("fixture CA is invalid")
				}
				config.TLSConfig = &tls.Config{RootCAs: roots, MinVersion: tls.VersionTLS12}
			}
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			result, err := VerifyRelay(ctx, config)
			if err != nil {
				t.Fatalf("authenticated bidirectional relay: %v", err)
			}
			if !result.RelayedAddr.IsValid() || result.RelayedAddr.Port() == 0 {
				t.Fatal("relay result lacks the actual allocated address")
			}
			t.Run("wrong-password", func(t *testing.T) {
				wrong := config
				wrong.Password = "incorrect-password"
				ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
				defer cancel()
				if _, err := VerifyRelay(ctx, wrong); err == nil {
					t.Fatal("wrong credentials must not pass relay verification")
				}
			})
			t.Run("denied-peer", func(t *testing.T) {
				denied := config
				denied.Peer = netip.MustParseAddrPort("172.30.171.10:3479")
				ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
				defer cancel()
				if _, err := VerifyRelay(ctx, denied); err == nil || !strings.Contains(err.Error(), "CreatePermission") {
					t.Fatalf("coturn must reject a peer outside its allowlist, got %v", err)
				}
			})
			if transport == TLS {
				t.Run("untrusted-certificate", func(t *testing.T) {
					config.TLSConfig = nil
					ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
					defer cancel()
					if _, err := VerifyRelay(ctx, config); err == nil {
						t.Fatal("untrusted TURN certificate must not be accepted")
					}
				})
			}
			if transport == UDP && os.Getenv("TURNPROBE_TEST_STALE_NONCE") == "1" {
				t.Run("stale-nonce-recovery", func(t *testing.T) {
					proxyAddress, staleReplies, closeProxy := staleNonceProxy(t, config.ServerAddr)
					defer closeProxy()
					config.ServerAddr = proxyAddress
					ctx, cancel := context.WithTimeout(context.Background(), 8*time.Second)
					defer cancel()
					if _, err := VerifyRelay(ctx, config); err != nil {
						t.Fatalf("real coturn stale nonce must be refreshed: %v", err)
					}
					if staleReplies.Load() == 0 {
						t.Fatal("fixture did not exercise a real coturn438 response")
					}
				})
			}
		})
	}
}

// Delay only the first real server challenge until its one-second nonce
// expires. All replies, including438 and their HMACs, still come from coturn.
func staleNonceProxy(t *testing.T, serverAddress string) (string, *atomic.Int32, func()) {
	t.Helper()
	client, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	server, err := net.Dial("udp", serverAddress)
	if err != nil {
		client.Close()
		t.Fatal(err)
	}
	var staleReplies atomic.Int32
	_ = client.SetDeadline(time.Now().Add(9 * time.Second))
	_ = server.SetDeadline(time.Now().Add(9 * time.Second))
	go func() {
		request, response := make([]byte, 65535), make([]byte, 65535)
		first := true
		for {
			n, peer, err := client.ReadFrom(request)
			if err != nil {
				return
			}
			if _, err := server.Write(request[:n]); err != nil {
				return
			}
			n, err = server.Read(response)
			if err != nil {
				return
			}
			if first {
				first = false
				time.Sleep(2200 * time.Millisecond)
			}
			if message, err := parseMessage(response[:n]); err == nil && message.errorCode() == 438 {
				staleReplies.Add(1)
			}
			if _, err := client.WriteTo(response[:n], peer); err != nil {
				return
			}
		}
	}()
	return client.LocalAddr().String(), &staleReplies, func() { client.Close(); server.Close() }
}
