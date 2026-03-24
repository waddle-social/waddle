package scaleway

import (
	"net"

	baremetal "github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
)

// ExtractServerIPs returns the first public IPv4 and first private IP attached to a bare metal server.
func ExtractServerIPs(server *baremetal.Server) (publicIP, privateIP string) {
	if server == nil {
		return "", ""
	}

	for _, ip := range server.IPs {
		if ip == nil {
			continue
		}

		addr := ip.Address.String()
		parsed := net.ParseIP(addr)
		if parsed != nil && parsed.IsPrivate() {
			if privateIP == "" {
				privateIP = addr
			}
			continue
		}

		if ip.Version == baremetal.IPVersionIPv4 && publicIP == "" {
			publicIP = addr
		}
	}

	return publicIP, privateIP
}

// ServerDisplayIP returns the preferred operator-facing IP for a server.
func ServerDisplayIP(server *baremetal.Server) string {
	publicIP, privateIP := ExtractServerIPs(server)
	if publicIP != "" {
		return publicIP
	}

	return privateIP
}
