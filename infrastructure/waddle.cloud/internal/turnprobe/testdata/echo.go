//go:build ignore

// The isolated qualification peer echoes UDP only inside its Docker network.
package main

import (
	"log"
	"net"
)

func main() {
	conn, err := net.ListenPacket("udp4", ":3479")
	if err != nil {
		log.Fatal(err)
	}
	defer conn.Close()
	buffer := make([]byte, 65535)
	for {
		n, peer, err := conn.ReadFrom(buffer)
		if err != nil {
			log.Fatal(err)
		}
		if _, err := conn.WriteTo(buffer[:n], peer); err != nil {
			log.Fatal(err)
		}
	}
}
