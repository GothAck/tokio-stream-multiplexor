use tokio_stream_multiplexor::{Config, DuplexStream, StreamMultiplexor};

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    runtime::Runtime,
    sync::mpsc,
};

const PAYLOAD_SIZE: usize = 1024 * 1024;
const SEND_ROUND: usize = 256;
const HANDSHAKE_ROUND: usize = 16 * 1024;

async fn get_tcp_stream_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = listener.local_addr().unwrap();

    let (tx, mut rx) = mpsc::channel(1);
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        tx.send(stream).await.unwrap();
    });

    let client_stream = TcpStream::connect(local_addr).await.unwrap();
    let server_stream = rx.recv().await.unwrap();

    (client_stream, server_stream)
}

async fn get_mux_stream_pair() -> (
    StreamMultiplexor<TcpStream>,
    StreamMultiplexor<TcpStream>,
    DuplexStream,
    DuplexStream,
) {
    let (stream0, stream1) = get_tcp_stream_pair().await;

    let mux0 = StreamMultiplexor::new(stream0, Config::default());
    let mux1 = StreamMultiplexor::new(stream1, Config::default());

    let (tx, mut rx) = mpsc::channel(1);
    let mux1 = Arc::from(mux1);
    let listener1 = mux1.bind(22).await.unwrap();
    tokio::spawn(async move {
        let stream = listener1.accept().await.unwrap();
        tx.send(stream).await.unwrap();
    });

    let stream0 = mux0.connect(22).await.unwrap();
    let stream1 = rx.recv().await.unwrap();

    (mux0, Arc::try_unwrap(mux1).unwrap(), stream0, stream1)
}

async fn tcp_throughput() {
    let (mut stream0, mut stream1) = get_tcp_stream_pair().await;

    tokio::spawn(async move {
        let mut buf: Vec<u8> = vec![];
        buf.resize(PAYLOAD_SIZE, 0);
        for _ in 0..SEND_ROUND {
            stream0.write_all(&buf).await.unwrap();
        }
    });

    let mut buf: Vec<u8> = vec![];
    buf.resize(PAYLOAD_SIZE, 0);
    for _ in 0..SEND_ROUND {
        stream1.read_exact(&mut buf).await.unwrap();
    }
}

async fn mux_throughput() {
    let (_mux0, _mux1, mut stream0, mut stream1) = get_mux_stream_pair().await;

    tokio::spawn(async move {
        let mut buf: Vec<u8> = vec![];
        buf.resize(PAYLOAD_SIZE, 0);
        for _ in 0..SEND_ROUND {
            stream0.write_all(&buf).await.unwrap();
        }
    });

    let mut buf: Vec<u8> = vec![];
    buf.resize(PAYLOAD_SIZE, 0);
    for _ in 0..SEND_ROUND {
        stream1.read_exact(&mut buf).await.unwrap();
    }
}

async fn mux_handshake() {
    let (stream0, stream1) = get_tcp_stream_pair().await;
    let mux0 = StreamMultiplexor::new(stream0, Config::default());
    let mux1 = StreamMultiplexor::new(stream1, Config::default());

    for i in 0..HANDSHAKE_ROUND {
        let listener = mux0.bind(i as u16 + 1).await.unwrap();
        tokio::spawn(async move {
            listener.accept().await.unwrap();
        });
    }

    for i in 0..HANDSHAKE_ROUND {
        mux1.connect(i as u16 + 1).await.unwrap();
    }
}

pub fn throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput");
    group.throughput(Throughput::Bytes((PAYLOAD_SIZE * SEND_ROUND) as u64));
    group.bench_function("tcp", |b| {
        b.to_async(Runtime::new().unwrap())
            .iter(|| tcp_throughput())
    });
    group.bench_function("mux", |b| {
        b.to_async(Runtime::new().unwrap())
            .iter(|| mux_throughput())
    });
    group.finish();
}

pub fn handshake(c: &mut Criterion) {
    let mut group = c.benchmark_group("handshake");
    group.throughput(Throughput::Elements(HANDSHAKE_ROUND as u64));
    group.bench_function("mux", |b| {
        b.to_async(Runtime::new().unwrap()).iter(|| mux_handshake())
    });
    group.finish();
}

criterion_group! {
    name = throughput_benches;
    config = Criterion::default().sample_size(10);
    targets = throughput
}

criterion_group! {
    name = handshake_benches;
    config = Criterion::default().sample_size(10);
    targets = handshake
}

criterion_main!(handshake_benches, throughput_benches);
