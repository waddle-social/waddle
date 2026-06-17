package turnprobe

import (
	"encoding/binary"
	"errors"
	"net/netip"
	"testing"
)

// bindingSuccess builds a STUN Binding success response (type 0x0101)
// carrying a single XOR-MAPPED-ADDRESS for an IPv4 reflexive transport
// address, encoded per RFC 8489 §14.2.
func bindingSuccess(txID TxID, addr netip.AddrPort) []byte {
	ip := addr.Addr().As4()
	xport := addr.Port() ^ uint16(magicCookie>>16)

	attr := make([]byte, 12)
	binary.BigEndian.PutUint16(attr[0:2], 0x0020) // XOR-MAPPED-ADDRESS
	binary.BigEndian.PutUint16(attr[2:4], 8)      // value length
	attr[4] = 0x00
	attr[5] = 0x01 // IPv4 family
	binary.BigEndian.PutUint16(attr[6:8], xport)
	cookie := make([]byte, 4)
	binary.BigEndian.PutUint32(cookie, magicCookie)
	for i := 0; i < 4; i++ {
		attr[8+i] = ip[i] ^ cookie[i]
	}

	msg := make([]byte, 20+len(attr))
	binary.BigEndian.PutUint16(msg[0:2], 0x0101) // Binding success response
	binary.BigEndian.PutUint16(msg[2:4], uint16(len(attr)))
	binary.BigEndian.PutUint32(msg[4:8], magicCookie)
	copy(msg[8:20], txID[:])
	copy(msg[20:], attr)
	return msg
}

// A STUN Binding request (RFC 8489 §5) is a 20-byte header: a Binding
// request method/class (0x0001), a zero attribute length, the fixed magic
// cookie (0x2112A442), and a 96-bit transaction id. The TURN/UDP relay
// reachability probe sends exactly this to the embedded TURN listener.
func TestBuildBindingRequestHasValidStunHeader(t *testing.T) {
	req, txID := BuildBindingRequest()

	if len(req) != 20 {
		t.Fatalf("binding request length = %d, want 20", len(req))
	}
	if got := binary.BigEndian.Uint16(req[0:2]); got != 0x0001 {
		t.Fatalf("message type = %#04x, want 0x0001 (Binding request)", got)
	}
	if got := binary.BigEndian.Uint16(req[2:4]); got != 0 {
		t.Fatalf("attribute length = %d, want 0 for an attribute-less request", got)
	}
	if got := binary.BigEndian.Uint32(req[4:8]); got != magicCookie {
		t.Fatalf("magic cookie = %#08x, want %#08x", got, magicCookie)
	}
	if [12]byte(req[8:20]) != txID {
		t.Fatalf("transaction id in header %x does not match returned id %x", req[8:20], txID)
	}
}

// A Binding success response that matches the request's transaction id
// yields the server-reflexive transport address it carried, proving the
// UDP round trip reached the TURN listener and a reply returned.
func TestParseBindingResponseDecodesReflexiveAddress(t *testing.T) {
	_, txID := BuildBindingRequest()
	want := netip.MustParseAddrPort("203.0.113.7:51820")

	got, err := ParseBindingResponse(txID, bindingSuccess(txID, want))
	if err != nil {
		t.Fatalf("ParseBindingResponse returned error: %v", err)
	}
	if got != want {
		t.Fatalf("reflexive address = %v, want %v", got, want)
	}
}

// A datagram whose transaction id differs from the request must be
// rejected — otherwise a stray or spoofed packet could be mistaken for a
// successful round trip and mask an unreachable relay.
func TestParseBindingResponseRejectsTransactionIDMismatch(t *testing.T) {
	_, sent := BuildBindingRequest()
	_, other := BuildBindingRequest()

	if _, err := ParseBindingResponse(sent, bindingSuccess(other, netip.MustParseAddrPort("203.0.113.7:3478"))); err == nil {
		t.Fatal("expected error for transaction id mismatch, got nil")
	}
}

// Truncated or non-STUN bytes must be rejected rather than panicking or
// reporting a bogus reachable result.
func TestParseBindingResponseRejectsMalformed(t *testing.T) {
	_, txID := BuildBindingRequest()
	for name, resp := range map[string][]byte{
		"empty":         {},
		"short header":  make([]byte, 8),
		"no attributes": bindingSuccessHeaderOnly(txID),
	} {
		if _, err := ParseBindingResponse(txID, resp); err == nil {
			t.Fatalf("%s: expected error, got nil", name)
		}
	}
}

// Probe composes a build → send → parse round trip over an injected
// transport, so the reachability check is exercised without a real socket.
func TestProbeReturnsReflexiveAddressFromRoundTrip(t *testing.T) {
	want := netip.MustParseAddrPort("198.51.100.9:30478")
	rt := func(request []byte) ([]byte, error) {
		return bindingSuccess(TxID(request[8:20]), want), nil
	}

	got, err := Probe(rt)
	if err != nil {
		t.Fatalf("Probe returned error: %v", err)
	}
	if got != want {
		t.Fatalf("Probe address = %v, want %v", got, want)
	}
}

// A transport failure (no datagram returned) must surface as an error so
// an unreachable relay is reported, not silently swallowed.
func TestProbePropagatesTransportError(t *testing.T) {
	rt := func([]byte) ([]byte, error) { return nil, errors.New("i/o timeout") }

	if _, err := Probe(rt); err == nil {
		t.Fatal("expected Probe to propagate transport error, got nil")
	}
}

// bindingSuccessHeaderOnly is a success response with a valid header but
// no XOR-MAPPED-ADDRESS attribute.
func bindingSuccessHeaderOnly(txID TxID) []byte {
	msg := make([]byte, 20)
	binary.BigEndian.PutUint16(msg[0:2], 0x0101)
	binary.BigEndian.PutUint32(msg[4:8], magicCookie)
	copy(msg[8:20], txID[:])
	return msg
}
