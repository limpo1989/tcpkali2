# TcpKali2 - High Performance Load Testing Tool

## Overview

TcpKali2 is a high-performance load testing tool designed for benchmarking TCP and WebSocket servers. Built with Rust
and Tokio, it delivers exceptional performance with minimal overhead, making it ideal for stress testing and performance
analysis.

## Features

* Multi-protocol support: Test both TCP and WebSocket (RFC6455) server
* Precise rate control: Accurate message/connection rate limiting
* Comprehensive metrics: Latency distribution, throughput, error rates
* Resource efficient: Low memory footprint, high connection density
* Flexible configuration: Custom messages, bandwidth limiting, variable duration

## Installation

```bash
cargo install tcpkali2
```

## Usage

Basic TCP echo test.

```bash
tcpkali2 -c 1000 127.0.0.1:9527
```

Basic Websocket echo test.

```bash
tcpkali2 -c 1000 --ws ws://127.0.0.1:8000
```

```
$ tcpkali2 --help
A load testing tool for WebSocket and TCP server

Usage: tcpkali2 [OPTIONS] <host:port>

Arguments:
  <host:port>  Target server in host:port format

Options:
      --websocket                    Use RFC6455 WebSocket transport
  -c, --connections <N>              Connections to keep open to the destinations [default: 1]
      --connect-rate <R>             Limit number of new connections per second [default: 100]
      --connect-timeout <T>          Limit time spent in a connection attempt [default: 1s]
      --channel-lifetime <T>         Shut down each connection after T seconds
  -w, --workers <N>                  Number of Tokio worker threads to use [default: 8]
      --nagle                        Control Nagle algorithm (set TCP_NODELAY)
  -p, --pipeline                     Use pipeline client to send messages
  -T, --duration <T>                 Load test for the specified amount of time [default: 15s]
  -e, --unescape-message-args        Unescape the following {-m|-f|--first-*} arguments
      --first-message <string>       Send this message first, once
      --first-message-file <name>    Read the first message from a file
  -m, --message <string>             Message to repeatedly send to the remote
  -s, --message-size <message-size>  Random message to repeatedly send to the remote [default: 128]
  -f, --message-file <name>          Read message to send from a file
  -r, --message-rate <R>             Messages per second to send in a connection
  -q                                 Suppress real-time output
  -h, --help                         Print help
  -V, --version                      Print version
```

## Example

Written your echo test server

```go
package main

import (
	"fmt"
	"github.com/urpc/uio"
)

func main() {
	var events uio.Events
	
	events.OnData = func(c uio.Conn) error {
		_, err := c.WriteTo(c)
		return err
	}

	if err := events.Serve(":9527"); nil != err {
		fmt.Println("server exited with error:", err)
	}
}
```

Run your benchmark

````
# run echo server
$ go run main.go

# start benchmark
$ tcpkali2 -c 200 -p 127.0.0.1:9527
Warming up for 5 seconds...
Warmup completed. Starting benchmark for 15 seconds...
[Live] QPS: inf | Req: 787 | Latency(us): P50=3153 P95=44159 P99=44159
[Live] QPS: 1564783 | Req: 1567135 | Latency(us): P50=4435 P95=19087 P99=32255
[Live] QPS: 1551506 | Req: 3124851 | Latency(us): P50=4639 P95=21087 P99=35263
[Live] QPS: 1555870 | Req: 4674494 | Latency(us): P50=4687 P95=21791 P99=36031
[Live] QPS: 1579619 | Req: 6250954 | Latency(us): P50=4627 P95=21343 P99=35775
[Live] QPS: 1580863 | Req: 7842883 | Latency(us): P50=4651 P95=21487 P99=35583
[Live] QPS: 1575235 | Req: 9410242 | Latency(us): P50=4659 P95=21967 P99=36639
[Live] QPS: 1558136 | Req: 10963704 | Latency(us): P50=4671 P95=22015 P99=36479
[Live] QPS: 1555887 | Req: 12527370 | Latency(us): P50=4679 P95=22143 P99=36767
[Live] QPS: 1565435 | Req: 14091240 | Latency(us): P50=4667 P95=21871 P99=36383
[Live] QPS: 1544243 | Req: 15641660 | Latency(us): P50=4663 P95=21807 P99=35935
[Live] QPS: 1557999 | Req: 17193427 | Latency(us): P50=4671 P95=21791 P99=35903
[Live] QPS: 1562958 | Req: 18757948 | Latency(us): P50=4691 P95=22015 P99=36287
[Live] QPS: 1563452 | Req: 20318273 | Latency(us): P50=4687 P95=21967 P99=36383
[Live] QPS: 1571461 | Req: 21891306 | Latency(us): P50=4719 P95=22143 P99=36671
[Live] QPS: 1565013 | Req: 23468838 | Latency(us): P50=4719 P95=22143 P99=36543

=== Final Results ===
Duration:          15.01s
Total Connections: 200
Success Rate:      100.0%
Total Requests:    23478241
Error Rate:        0.00%
Requests Rate:     1563978.19 req/s
Throughput:        6010.43 MB
Bandwidth:         400.38 MB/s
Traffic:           24041↓, 24041↑ Mbps
Latency Distribution (us):
  Avg:   6687.3  Min:       53
  P50:     4719  P90:    13287
  P95:    22143  P99:    36543
  Max:    97663
````

## License

The repository released under version 2.0 of the Apache License.