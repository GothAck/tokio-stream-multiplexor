[![Build Status](https://app.travis-ci.com/GothAck/tokio-stream-multiplexor.svg?branch=master)](https://app.travis-ci.com/GothAck/tokio-stream-multiplexor)
![Crates.io](https://img.shields.io/crates/v/tokio-stream-multiplexor)
![Crates.io](https://img.shields.io/crates/l/tokio-stream-multiplexor)
![docs.rs](https://img.shields.io/docsrs/tokio-stream-multiplexor)

<!-- cargo-sync-readme start -->

# Tokio Stream Multiplexor

TL;DR: Multiplex multiple streams over a single stream. Has a TcpListener / TcpSocket style interface, and uses u16 ports similar to TCP itself.

```toml
[dependencies]
foo = "1.2.3"
```

## Why?

Because sometimes you wanna cram as much shite down one TCP stream as you possibly can, rather than have your application connect with multiple ports.

## But Whyyyyyy?

Because I needed it. Inspired by [async-smux](https://github.com/black-binary/async-smux), written for [Tokio](https://tokio.rs/).

## What about performance?
Doesn't this whole protocol in a protocol thing hurt perf?

Sure, but take a look at the benches:

```rust
throughput/tcp          time:   [28.968 ms 30.460 ms 31.744 ms]
                        thrpt:  [7.8755 GiB/s 8.2076 GiB/s 8.6303 GiB/s]

throughput/mux          time:   [122.24 ms 135.96 ms 158.80 ms]
                        thrpt:  [1.5743 GiB/s 1.8388 GiB/s 2.0451 GiB/s]
```

Approximately 4.5 times slower than TCP, but still able to shovel 1.8 GiB/s of shite... Seems alright to me. (Numbers could possibly be improved with some tuning of the config params too.)

<!-- cargo-sync-readme end -->
