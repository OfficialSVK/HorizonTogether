// Joins a Bedrock server as a bot and logs every packet in both directions, to record the
// official server's join sequence. Usage: go run . <host:port> [seconds]
package main

import (
	"encoding/hex"
	"fmt"
	"net"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/sandertv/gophertunnel/minecraft"
	"github.com/sandertv/gophertunnel/minecraft/protocol/login"
	"github.com/sandertv/gophertunnel/minecraft/protocol/packet"
)

func main() {
	addr := os.Args[1]
	secs := 15
	if len(os.Args) > 2 {
		secs, _ = strconv.Atoi(os.Args[2])
	}
	start := time.Now()
	server, client := packet.NewServerPool(), packet.NewClientPool()
	// DUMP=<file>: record every server->client packet after ResourcePackStack as [u32 id][u32 len][payload] (LE).
	var dump *os.File
	recording := false
	if p := os.Getenv("DUMP"); p != "" {
		dump, _ = os.Create(p)
		defer dump.Close()
	}

	d := minecraft.Dialer{
		IdentityData: login.IdentityData{DisplayName: "LogBot"},
		PacketFunc: func(h packet.Header, payload []byte, src, dst net.Addr) {
			dir, pool := "C->S", client
			if strings.HasSuffix(src.String(), addr[strings.LastIndex(addr, ":"):]) {
				dir, pool = "S->C", server
			}
			name := "?"
			if f, ok := pool[h.PacketID]; ok {
				name = fmt.Sprintf("%T", f())[len("*packet."):]
			}
			n := len(payload)
			bulky := h.PacketID == 58 || h.PacketID == 172 || h.PacketID == 144 // chunks, block updates, input
			if n > 64 && (bulky || os.Getenv("FULL") == "") {
				n = 64
			}
			fmt.Printf("%6dms %s %3d %-32s %7d  %s\n", time.Since(start).Milliseconds(), dir, h.PacketID, name, len(payload), hex.EncodeToString(payload[:n]))
			if dump != nil && dir == "S->C" {
				if recording {
					hdr := []byte{byte(h.PacketID), byte(h.PacketID >> 8), byte(h.PacketID >> 16), byte(h.PacketID >> 24),
						byte(len(payload)), byte(len(payload) >> 8), byte(len(payload) >> 16), byte(len(payload) >> 24)}
					_, _ = dump.Write(hdr)
					_, _ = dump.Write(payload)
				}
				if h.PacketID == 7 { // ResourcePackStack: everything after it is the join stream
					recording = true
				}
			}
		},
	}
	conn, err := d.Dial("raknet", addr)
	if err != nil {
		fmt.Println("dial:", err)
		os.Exit(1)
	}
	defer conn.Close()
	if err := conn.DoSpawn(); err != nil {
		fmt.Println("spawn:", err)
		os.Exit(1)
	}
	fmt.Printf("%6dms ---- spawned ----\n", time.Since(start).Milliseconds())
	_ = conn.SetDeadline(time.Now().Add(time.Duration(secs) * time.Second))
	for {
		if _, err := conn.ReadPacket(); err != nil {
			return
		}
	}
}
