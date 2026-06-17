package cmd

import (
	"encoding/binary"
	"net"
	"net/netip"
	"testing"
	"time"

	"github.com/waddle-social/waddle/infrastructure/waddle.cloud/internal/turnprobe"
)

// stunBindingSuccess mirrors a TURN/STUN server reply: a Binding success
// response echoing the request transaction id and carrying an
// XOR-MAPPED-ADDRESS for reflexive (RFC 8489 §14.2, IPv4).
func stunBindingSuccess(txID turnprobe.TxID, reflexive netip.AddrPort) []byte {
	const magicCookie uint32 = 0x2112A442
	ip := reflexive.Addr().As4()
	xport := reflexive.Port() ^ uint16(magicCookie>>16)

	attr := make([]byte, 12)
	binary.BigEndian.PutUint16(attr[0:2], 0x0020)
	binary.BigEndian.PutUint16(attr[2:4], 8)
	attr[5] = 0x01
	binary.BigEndian.PutUint16(attr[6:8], xport)
	var cookie [4]byte
	binary.BigEndian.PutUint32(cookie[:], magicCookie)
	for i := 0; i < 4; i++ {
		attr[8+i] = ip[i] ^ cookie[i]
	}

	msg := make([]byte, 20+len(attr))
	binary.BigEndian.PutUint16(msg[0:2], 0x0101)
	binary.BigEndian.PutUint16(msg[2:4], uint16(len(attr)))
	binary.BigEndian.PutUint32(msg[4:8], magicCookie)
	copy(msg[8:20], txID[:])
	copy(msg[20:], attr)
	return msg
}

// The UDP round tripper dials, sends the binding request, and reads the
// reply within the deadline. Against a loopback STUN responder it must
// drive a full Probe to the reflexive address — exercising the real
// socket path the operator probe uses against turn.waddle.social:30478.
func TestUDPRoundTripperReachesLoopbackResponder(t *testing.T) {
	conn, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatalf("listen udp: %v", err)
	}
	defer conn.Close()

	reflexive := netip.MustParseAddrPort("198.51.100.9:30478")
	go func() {
		buf := make([]byte, 1500)
		n, from, err := conn.ReadFromUDP(buf)
		if err != nil || n < 20 {
			return
		}
		_, _ = conn.WriteToUDP(stunBindingSuccess(turnprobe.TxID(buf[8:20]), reflexive), from)
	}()

	got, err := turnprobe.Probe(udpRoundTripper(conn.LocalAddr().String(), 2*time.Second))
	if err != nil {
		t.Fatalf("Probe over loopback round tripper: %v", err)
	}
	if got != reflexive {
		t.Fatalf("reflexive address = %v, want %v", got, reflexive)
	}
}

// An address with no responder must fail within the timeout rather than
// blocking forever.
func TestUDPRoundTripperTimesOutWhenUnreachable(t *testing.T) {
	// 203.0.113.0/24 (TEST-NET-3) is reserved and unrouted, so nothing replies.
	if _, err := turnprobe.Probe(udpRoundTripper("203.0.113.1:30478", 200*time.Millisecond)); err == nil {
		t.Fatal("expected timeout error for unreachable address, got nil")
	}
}
