# libp2p-playground

Repository of [libp2p](https://libp2p.io/) experiments.

## DCUtR Bandwith Benchmark

The DCUtR bandwidth benchmark evaluates the bandwidth between two peers
that established a connection via libp2p'2 hole punching mechanism: DCUtR.

Sources:
- The relay server implementation is modified from
  [libp2p/examples/relay-server](https://github.com/libp2p/rust-libp2p/tree/7d1d67cad3847a845ad50d9e56b3b68ca53f5e22/examples/relay-server)
- The DCUtR code is based on the [example](https://github.com/libp2p/rust-libp2p/tree/7d1d67cad3847a845ad50d9e56b3b68ca53f5e22/examples/dcutr).
- The [libp2p-perf](https://docs.rs/libp2p-perf/latest/libp2p_perf/)
  benchmarking code is based on the [perf binary](https://github.com/libp2p/rust-libp2p/blob/master/protocols/perf/src/bin/perf.rs).

### Host a Relay Server

The relay server needs to be on a public host.

```bash
$ cargo run --bin relay-server --port 4001 --secret-key-seed 0
```

The relay server's multiaddr will look as follows if you used the same seed and port.
```
/ip4/$RELAY_SERVER_IP/tcp/4001/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN
```
Replace `$RELAY_SERVER_IP` with the public IP of the host.

### Start the Receiver and Sender

The receiver will wait for a sender to request to connect to it. Once the
receiver and sender are connected, the sender will start the performace test
by sending `payload-bytes` to the receiver. The receiver sends the same
amount back to the sender afterwards.

**Receiver**
```bash
$ cargo run --bin benchmark-receive -- --seed 1 --relay-multiaddr /ip4/$RELAY_SERVER_IP/tcp/4001/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN
```

**Sender**
```bash
$ cargo run --bin benchmark-send -- --seed 2 --relay-multiaddr /ip4/$RELAY_SERVER_IP/tcp/4001/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN --receiver-peer-id 12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X --payload-bytes 10000000
```
