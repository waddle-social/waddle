package turnprobe

import (
	"bytes"
	"context"
	"crypto/hmac"
	"crypto/md5"
	"crypto/rand"
	"crypto/sha1"
	"crypto/tls"
	"encoding/binary"
	"errors"
	"fmt"
	"hash/crc32"
	"io"
	"net"
	"net/netip"
	"time"
)

// Transport is the client-to-TURN transport. The relayed peer traffic is UDP.
type Transport string

const (
	UDP Transport = "udp"
	TLS Transport = "tls"
)

// RelayConfig qualifies a TURN server against an explicitly controlled UDP
// echo peer. Credentials are never included in errors. TLS always verifies
// the server certificate; TLSConfig may supply a private qualification CA.
type RelayConfig struct {
	ServerAddr string
	Transport  Transport
	Username   string
	Password   string
	Peer       netip.AddrPort
	TLSConfig  *tls.Config
}

// RelayResult is returned only after both directions of the allocation carry
// an unpredictable echoed payload, not merely after a STUN Binding reply.
type RelayResult struct {
	RelayedAddr netip.AddrPort
}

const (
	allocate         uint16 = 0x0003
	refresh          uint16 = 0x0004
	createPermission uint16 = 0x0008
	channelBind      uint16 = 0x0009
	attrUsername     uint16 = 0x0006
	attrIntegrity    uint16 = 0x0008
	attrError        uint16 = 0x0009
	attrChannel      uint16 = 0x000c
	attrLifetime     uint16 = 0x000d
	attrPeer         uint16 = 0x0012
	attrRealm        uint16 = 0x0014
	attrNonce        uint16 = 0x0015
	attrRelay        uint16 = 0x0016
	attrTransport    uint16 = 0x0019
	probeChannel     uint16 = 0x4000
)

type attribute struct {
	kind  uint16
	value []byte
}

type stunMessage struct {
	kind  uint16
	txID  TxID
	attrs []attribute
	raw   []byte
}

func (m stunMessage) value(kind uint16) []byte {
	for _, a := range m.attrs {
		if a.kind == kind {
			return a.value
		}
	}
	return nil
}

func (m stunMessage) errorCode() int {
	v := m.value(attrError)
	if len(v) < 4 {
		return 0
	}
	return int(v[2]&7)*100 + int(v[3])
}

type relaySession struct {
	conn     net.Conn
	stream   bool
	deadline time.Time
	username string
	realm    []byte
	nonce    []byte
	key      []byte
}

// VerifyRelay performs RFC 8656 Allocate, CreatePermission and ChannelBind,
// followed by a bidirectional ChannelData exchange. A ten-second ceiling
// bounds even callers that omit a context deadline. It never weakens the
// target server's peer restrictions or starts a public echo service.
func VerifyRelay(ctx context.Context, config RelayConfig) (RelayResult, error) {
	if config.Username == "" || config.Password == "" {
		return RelayResult{}, errors.New("TURN_USERNAME and TURN_PASSWORD are required")
	}
	if !config.Peer.IsValid() || config.Peer.Port() == 0 || config.Peer.Addr().IsUnspecified() || config.Peer.Addr().IsMulticast() {
		return RelayResult{}, errors.New("an explicit unicast controlled echo peer is required")
	}
	if config.Transport != UDP && config.Transport != TLS {
		return RelayResult{}, errors.New("TURN transport must be udp or tls")
	}
	ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	deadline, _ := ctx.Deadline()
	conn, err := dialRelay(ctx, config)
	if err != nil {
		return RelayResult{}, fmt.Errorf("connect TURN/%s: %w", config.Transport, err)
	}
	defer conn.Close()
	stop := context.AfterFunc(ctx, func() { _ = conn.Close() })
	defer stop()
	if err := conn.SetDeadline(deadline); err != nil {
		return RelayResult{}, err
	}
	session := relaySession{conn: conn, stream: config.Transport == TLS, deadline: deadline, username: config.Username}
	requestAttrs := []attribute{{attrTransport, []byte{17, 0, 0, 0}}}
	challenge, err := session.exchange(allocate, requestAttrs, nil)
	if err != nil {
		return RelayResult{}, fmt.Errorf("TURN Allocate challenge: %w", err)
	}
	if challenge.kind != allocate|0x0110 || challenge.errorCode() != 401 || len(challenge.value(attrRealm)) == 0 || len(challenge.value(attrNonce)) == 0 {
		return RelayResult{}, errors.New("TURN server did not require long-term credential authentication")
	}
	session.realm = challenge.value(attrRealm)
	session.nonce = challenge.value(attrNonce)
	// Classic coturn REST authentication uses the RFC 8489 §9.2.2 MD5
	// long-term key and §14.5 HMAC-SHA1 integrity formula, not password
	// storage or file signatures. Negotiated SHA-256 is a separate STUN mode.
	key := md5.Sum([]byte(config.Username + ":" + string(session.realm) + ":" + config.Password)) // NOSONAR: reviewed classic coturn REST interoperability.
	session.key = key[:]
	allocated, err := session.authenticated(allocate, requestAttrs)
	if err != nil {
		return RelayResult{}, fmt.Errorf("authenticated TURN Allocate: %w", err)
	}
	relayed, err := decodeXorAddress(allocated.value(attrRelay), allocated.txID)
	if err != nil || !relayed.IsValid() || relayed.Port() == 0 || relayed.Addr().IsUnspecified() {
		return RelayResult{}, errors.New("TURN Allocate returned an invalid relayed address")
	}
	// Release the short-lived diagnostic allocation. Cleanup is best-effort
	// and cannot extend the probe's existing deadline or turn failure to success.
	defer func() {
		_, _ = session.authenticated(refresh, []attribute{{attrLifetime, []byte{0, 0, 0, 0}}})
	}()
	if _, err := session.authenticated(createPermission, []attribute{{attrPeer, encodeXorAddress(config.Peer)}}); err != nil {
		return RelayResult{}, fmt.Errorf("TURN CreatePermission: %w", err)
	}
	if _, err := session.authenticated(channelBind, []attribute{{attrChannel, []byte{0x40, 0, 0, 0}}, {attrPeer, encodeXorAddress(config.Peer)}}); err != nil {
		return RelayResult{}, fmt.Errorf("TURN ChannelBind: %w", err)
	}
	if err := session.verifyChannelEcho(); err != nil {
		return RelayResult{}, err
	}
	return RelayResult{RelayedAddr: relayed}, nil
}

func (s *relaySession) verifyChannelEcho() error {
	payload := make([]byte, 32)
	if _, err := rand.Read(payload); err != nil {
		return fmt.Errorf("generate relay payload: %w", err)
	}
	frame := make([]byte, 4+len(payload))
	binary.BigEndian.PutUint16(frame, probeChannel)
	binary.BigEndian.PutUint16(frame[2:], uint16(len(payload)))
	copy(frame[4:], payload)
	if err := s.writeFrame(frame); err != nil {
		return fmt.Errorf("send relayed payload: %w", err)
	}
	for {
		response, err := s.readFrame()
		if err != nil {
			return fmt.Errorf("receive echoed relay payload: %w", err)
		}
		if len(response) < 4 || binary.BigEndian.Uint16(response) != probeChannel {
			continue
		}
		length := int(binary.BigEndian.Uint16(response[2:]))
		if length != len(payload) || len(response) < 4+length || !bytes.Equal(response[4:4+length], payload) {
			return errors.New("relayed echo did not match the unpredictable probe payload")
		}
		return nil
	}
}

func dialRelay(ctx context.Context, config RelayConfig) (net.Conn, error) {
	dialer := &net.Dialer{}
	if config.Transport == UDP {
		return dialer.DialContext(ctx, "udp", config.ServerAddr)
	}
	tlsConfig := &tls.Config{MinVersion: tls.VersionTLS12}
	if config.TLSConfig != nil {
		tlsConfig = config.TLSConfig.Clone()
		if tlsConfig.InsecureSkipVerify {
			return nil, errors.New("TURN TLS verification cannot be disabled")
		}
		if tlsConfig.MinVersion < tls.VersionTLS12 {
			tlsConfig.MinVersion = tls.VersionTLS12
		}
	}
	return (&tls.Dialer{NetDialer: dialer, Config: tlsConfig}).DialContext(ctx, "tcp", config.ServerAddr)
}

func (s *relaySession) authenticated(method uint16, attrs []attribute) (stunMessage, error) {
	for attempt := 0; attempt < 2; attempt++ {
		authAttrs := append(append([]attribute{}, attrs...), attribute{attrUsername, []byte(s.username)}, attribute{attrRealm, s.realm}, attribute{attrNonce, s.nonce})
		response, err := s.exchange(method, authAttrs, s.key)
		if err != nil {
			return stunMessage{}, err
		}
		if response.kind == method|0x0110 && response.errorCode() == 438 {
			if err := s.acceptStaleNonce(response, attempt); err != nil {
				return stunMessage{}, err
			}
			continue
		}
		if response.kind != method|0x0100 {
			return stunMessage{}, fmt.Errorf("TURN request rejected (code %d)", response.errorCode())
		}
		if err := verifyIntegrity(response.raw, s.key); err != nil {
			return stunMessage{}, err
		}
		return response, nil
	}
	return stunMessage{}, errors.New("TURN nonce retry exhausted")
}

func (s *relaySession) acceptStaleNonce(response stunMessage, attempt int) error {
	// RFC 8489 §9.2.4 permits unsigned438 challenges. Authenticate them
	// when signed; authenticated still requires a signed success afterward.
	if response.value(attrIntegrity) != nil {
		if err := verifyIntegrity(response.raw, s.key); err != nil {
			return err
		}
	}
	if attempt != 0 {
		return errors.New("TURN nonce retry exhausted")
	}
	if len(response.value(attrNonce)) == 0 || bytes.Equal(response.value(attrNonce), s.nonce) || !bytes.Equal(response.value(attrRealm), s.realm) {
		return errors.New("invalid TURN stale-nonce challenge")
	}
	s.nonce = response.value(attrNonce)
	return nil
}

func buildRequest(method uint16, attrs []attribute, key []byte) ([]byte, TxID, error) {
	var id TxID
	if _, err := rand.Read(id[:]); err != nil {
		return nil, TxID{}, err
	}
	// IPv6 XOR-PEER-ADDRESS includes this request's transaction ID.
	for i := range attrs {
		if attrs[i].kind == attrPeer && len(attrs[i].value) == 20 {
			attrs[i].value = bytes.Clone(attrs[i].value)
			for j := range id {
				attrs[i].value[8+j] ^= id[j]
			}
		}
	}
	return buildMessage(method, id, attrs, key), id, nil
}

func (s *relaySession) exchange(method uint16, attrs []attribute, key []byte) (stunMessage, error) {
	request, id, err := buildRequest(method, attrs, key)
	if err != nil {
		return stunMessage{}, err
	}
	for attempt := 0; ; attempt++ {
		if err := s.writeFrame(request); err != nil {
			return stunMessage{}, err
		}
		readDeadline := s.deadline
		if !s.stream && attempt < 3 {
			if retryAt := time.Now().Add((250 * time.Millisecond) << attempt); retryAt.Before(readDeadline) {
				readDeadline = retryAt
			}
		}
		if err := s.conn.SetReadDeadline(readDeadline); err != nil {
			return stunMessage{}, err
		}
		response, err := s.readResponse(id)
		if err == nil {
			return response, nil
		}
		var netErr net.Error
		if !s.stream && attempt < 3 && time.Now().Before(s.deadline) && errors.As(err, &netErr) && netErr.Timeout() {
			continue
		}
		return stunMessage{}, err
	}
}

func (s *relaySession) readResponse(id TxID) (stunMessage, error) {
	for {
		frame, err := s.readFrame()
		if err != nil {
			return stunMessage{}, err
		}
		response, err := parseMessage(frame)
		if err != nil || response.txID != id {
			continue
		}
		_ = s.conn.SetReadDeadline(s.deadline)
		return response, nil
	}
}

func (s *relaySession) writeFrame(frame []byte) error {
	for len(frame) > 0 {
		n, err := s.conn.Write(frame)
		if err != nil {
			return err
		}
		if n == 0 || (!s.stream && n != len(frame)) {
			return io.ErrShortWrite
		}
		frame = frame[n:]
	}
	return nil
}

func (s *relaySession) readFrame() ([]byte, error) {
	if !s.stream {
		packet := make([]byte, 65535)
		n, err := s.conn.Read(packet)
		return packet[:n], err
	}
	header := make([]byte, 4)
	if _, err := io.ReadFull(s.conn, header); err != nil {
		return nil, err
	}
	length := int(binary.BigEndian.Uint16(header[2:]))
	if header[0]&0xc0 == 0 {
		length += 16 // Remaining STUN header.
	} else if header[0]&0xc0 == 0x40 {
		length = (length + 3) &^ 3 // Stream ChannelData padding, RFC 8656 §11.5.
	} else {
		return nil, errors.New("invalid TURN stream frame")
	}
	frame := make([]byte, 4+length)
	copy(frame, header)
	_, err := io.ReadFull(s.conn, frame[4:])
	return frame, err
}

func buildMessage(kind uint16, id TxID, attrs []attribute, key []byte) []byte {
	packet := make([]byte, headerLen)
	binary.BigEndian.PutUint16(packet, kind)
	binary.BigEndian.PutUint32(packet[4:], magicCookie)
	copy(packet[8:], id[:])
	for _, attr := range attrs {
		start := len(packet)
		packet = append(packet, make([]byte, 4+((len(attr.value)+3)&^3))...)
		binary.BigEndian.PutUint16(packet[start:], attr.kind)
		binary.BigEndian.PutUint16(packet[start+2:], uint16(len(attr.value)))
		copy(packet[start+4:], attr.value)
	}
	if len(key) > 0 {
		binary.BigEndian.PutUint16(packet[2:], uint16(len(packet)-headerLen+24))
		mac := hmac.New(sha1.New, key)
		_, _ = mac.Write(packet)
		packet = append(packet, 0, byte(attrIntegrity), 0, 20)
		packet = append(packet, mac.Sum(nil)...)
	}
	binary.BigEndian.PutUint16(packet[2:], uint16(len(packet)-headerLen))
	return packet
}

func parseMessage(packet []byte) (stunMessage, error) {
	if len(packet) < headerLen || packet[0]&0xc0 != 0 || binary.BigEndian.Uint32(packet[4:]) != magicCookie {
		return stunMessage{}, errors.New("invalid STUN header")
	}
	length := int(binary.BigEndian.Uint16(packet[2:]))
	if length%4 != 0 || length+headerLen != len(packet) {
		return stunMessage{}, errors.New("invalid STUN message length")
	}
	message := stunMessage{kind: binary.BigEndian.Uint16(packet), txID: TxID(packet[8:20]), raw: packet}
	for body := packet[headerLen:]; len(body) > 0; {
		if len(body) < 4 {
			return stunMessage{}, errors.New("truncated STUN attribute")
		}
		n := int(binary.BigEndian.Uint16(body[2:]))
		advance := 4 + ((n + 3) &^ 3)
		if advance > len(body) {
			return stunMessage{}, errors.New("truncated STUN attribute value")
		}
		message.attrs = append(message.attrs, attribute{binary.BigEndian.Uint16(body), body[4 : 4+n]})
		body = body[advance:]
	}
	return message, nil
}

func verifyIntegrity(packet, key []byte) error {
	message, err := parseMessage(packet)
	if err != nil {
		return err
	}
	offset := headerLen
	for index, attr := range message.attrs {
		if attr.kind == attrIntegrity {
			if len(attr.value) != sha1.Size {
				return errors.New("invalid TURN MESSAGE-INTEGRITY length")
			}
			input := bytes.Clone(packet[:offset])
			binary.BigEndian.PutUint16(input[2:], uint16(offset+24-headerLen))
			mac := hmac.New(sha1.New, key)
			_, _ = mac.Write(input)
			if !hmac.Equal(mac.Sum(nil), attr.value) {
				return errors.New("TURN response MESSAGE-INTEGRITY mismatch")
			}
			return verifyIntegritySuffix(packet, offset+24, message.attrs[index+1:])
		}
		offset += 4 + ((len(attr.value) + 3) &^ 3)
	}
	return errors.New("authenticated TURN response lacks MESSAGE-INTEGRITY")
}

func verifyIntegritySuffix(packet []byte, signedEnd int, trailing []attribute) error {
	if len(trailing) == 0 {
		return nil
	}
	if len(trailing) != 1 || trailing[0].kind != 0x8028 || len(trailing[0].value) != 4 {
		return errors.New("unauthenticated attributes after TURN MESSAGE-INTEGRITY")
	}
	fingerprint := crc32.ChecksumIEEE(packet[:signedEnd]) ^ 0x5354554e
	if binary.BigEndian.Uint32(trailing[0].value) != fingerprint {
		return errors.New("TURN response FINGERPRINT mismatch")
	}
	return nil
}

func encodeXorAddress(address netip.AddrPort) []byte {
	ip := address.Addr().Unmap()
	value := make([]byte, 4+ip.BitLen()/8)
	value[1] = familyIPv4
	if ip.Is6() {
		value[1] = familyIPv6
	}
	binary.BigEndian.PutUint16(value[2:], address.Port()^uint16(magicCookie>>16))
	copy(value[4:], ip.AsSlice())
	var cookie [4]byte
	binary.BigEndian.PutUint32(cookie[:], magicCookie)
	for i := range cookie {
		value[4+i] ^= cookie[i]
	}
	return value
}

func decodeXorAddress(value []byte, id TxID) (netip.AddrPort, error) {
	if len(value) == 8 && value[1] == familyIPv4 {
		return decodeXorMapped(value)
	}
	if len(value) != 20 || value[1] != familyIPv6 {
		return netip.AddrPort{}, errors.New("malformed XOR address")
	}
	var address [16]byte
	copy(address[:], value[4:])
	var cookie [4]byte
	binary.BigEndian.PutUint32(cookie[:], magicCookie)
	for i := range cookie {
		address[i] ^= cookie[i]
	}
	for i := range id {
		address[4+i] ^= id[i]
	}
	return netip.AddrPortFrom(netip.AddrFrom16(address), binary.BigEndian.Uint16(value[2:])^uint16(magicCookie>>16)), nil
}
