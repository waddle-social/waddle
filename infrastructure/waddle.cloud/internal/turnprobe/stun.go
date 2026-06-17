// Package turnprobe builds and interprets the minimal STUN Binding
// exchange (RFC 8489) used to confirm that the self-hosted LiveKit
// embedded-TURN UDP listener is reachable end-to-end from outside the
// cluster. A Binding success response proves a UDP datagram traversed the
// NodePort and node firewall to the TURN server and a reply came back, so
// LiveKit can hand clients a usable `udp` relay candidate instead of
// silently forcing calls onto the slow TCP/443 relay.
package turnprobe

import (
	"crypto/rand"
	"encoding/binary"
	"errors"
	"fmt"
	"net/netip"
)

// magicCookie is the fixed STUN magic cookie (RFC 8489 §5).
const magicCookie uint32 = 0x2112A442

// TxID is a STUN 96-bit transaction id.
type TxID [12]byte

const (
	headerLen       = 20
	methodClassBind = 0x0001 // Binding request method+class.
	classBindingOK  = 0x0101 // Binding success response method+class.
	attrXorMapped   = 0x0020 // XOR-MAPPED-ADDRESS attribute type.
	familyIPv4      = 0x01
	familyIPv6      = 0x02
)

// BuildBindingRequest returns an attribute-less STUN Binding request and
// the transaction id stamped into it, so the caller can match the
// response against the request it sent. It errors if a cryptographically
// random transaction id cannot be generated — proceeding with a
// predictable id would weaken response correlation and spoofing resistance.
func BuildBindingRequest() ([]byte, TxID, error) {
	var txID TxID
	if _, err := rand.Read(txID[:]); err != nil {
		return nil, TxID{}, fmt.Errorf("generate transaction id: %w", err)
	}

	msg := make([]byte, headerLen)
	binary.BigEndian.PutUint16(msg[0:2], methodClassBind)
	binary.BigEndian.PutUint16(msg[2:4], 0)
	binary.BigEndian.PutUint32(msg[4:8], magicCookie)
	copy(msg[8:20], txID[:])
	return msg, txID, nil
}

// RoundTripper sends a STUN request and returns the response datagram that
// correlates to it — implementations must discard unrelated datagrams
// (e.g. by matching the transaction id in request[8:20]) and surface a
// timeout as an error. The CLI supplies a UDP-backed implementation; tests
// supply a fake.
type RoundTripper func(request []byte) ([]byte, error)

// Probe sends a Binding request through rt and returns the
// server-reflexive address from a matching success response. A non-nil
// error means the relay was not confirmed reachable.
func Probe(rt RoundTripper) (netip.AddrPort, error) {
	request, txID, err := BuildBindingRequest()
	if err != nil {
		return netip.AddrPort{}, err
	}
	response, err := rt(request)
	if err != nil {
		return netip.AddrPort{}, fmt.Errorf("STUN round trip: %w", err)
	}
	return ParseBindingResponse(txID, response)
}

// ParseBindingResponse validates that resp is a STUN Binding success
// response matching txID and returns the server-reflexive transport
// address from its XOR-MAPPED-ADDRESS attribute.
func ParseBindingResponse(txID TxID, resp []byte) (netip.AddrPort, error) {
	if len(resp) < headerLen {
		return netip.AddrPort{}, fmt.Errorf("response too short: %d bytes", len(resp))
	}
	if got := binary.BigEndian.Uint32(resp[4:8]); got != magicCookie {
		return netip.AddrPort{}, fmt.Errorf("wrong magic cookie %#08x", got)
	}
	if TxID(resp[8:20]) != txID {
		return netip.AddrPort{}, errors.New("transaction id mismatch")
	}
	if got := binary.BigEndian.Uint16(resp[0:2]); got != classBindingOK {
		return netip.AddrPort{}, fmt.Errorf("not a Binding success response: type %#04x", got)
	}

	// The header's message length is authoritative (RFC 8489 §5): the
	// attribute section is exactly that many bytes, 4-byte aligned. Clamp
	// to it so trailing bytes past the declared boundary are never mined.
	msgLen := int(binary.BigEndian.Uint16(resp[2:4]))
	if msgLen%4 != 0 {
		return netip.AddrPort{}, fmt.Errorf("STUN message length %d is not 4-byte aligned", msgLen)
	}
	if headerLen+msgLen > len(resp) {
		return netip.AddrPort{}, errors.New("STUN message length exceeds datagram")
	}

	addr, err := xorMappedAddress(resp[headerLen : headerLen+msgLen])
	if err != nil {
		return netip.AddrPort{}, err
	}
	return addr, nil
}

// xorMappedAddress walks the attribute section and decodes the first
// XOR-MAPPED-ADDRESS attribute (RFC 8489 §14.2).
func xorMappedAddress(body []byte) (netip.AddrPort, error) {
	for len(body) >= 4 {
		attrType := binary.BigEndian.Uint16(body[0:2])
		attrLen := int(binary.BigEndian.Uint16(body[2:4]))
		if 4+attrLen > len(body) {
			return netip.AddrPort{}, errors.New("truncated STUN attribute")
		}
		value := body[4 : 4+attrLen]
		if attrType == attrXorMapped {
			return decodeXorMapped(value)
		}
		// Attributes are padded to a 4-byte boundary; advancing must stay
		// within the buffer even when a trailing attribute omits its pad.
		advance := 4 + ((attrLen + 3) &^ 3)
		if advance > len(body) {
			return netip.AddrPort{}, errors.New("truncated STUN attribute padding")
		}
		body = body[advance:]
	}
	return netip.AddrPort{}, errors.New("no XOR-MAPPED-ADDRESS attribute")
}

func decodeXorMapped(value []byte) (netip.AddrPort, error) {
	if len(value) < 8 {
		return netip.AddrPort{}, errors.New("malformed XOR-MAPPED-ADDRESS")
	}
	if value[1] == familyIPv6 {
		return netip.AddrPort{}, errors.New("IPv6 XOR-MAPPED-ADDRESS is unsupported; probe targets the IPv4 NodePort")
	}
	if value[1] != familyIPv4 {
		return netip.AddrPort{}, fmt.Errorf("unknown XOR-MAPPED-ADDRESS family %#02x", value[1])
	}
	port := binary.BigEndian.Uint16(value[2:4]) ^ uint16(magicCookie>>16)
	var cookie [4]byte
	binary.BigEndian.PutUint32(cookie[:], magicCookie)
	var ip [4]byte
	for i := 0; i < 4; i++ {
		ip[i] = value[4+i] ^ cookie[i]
	}
	return netip.AddrPortFrom(netip.AddrFrom4(ip), port), nil
}
