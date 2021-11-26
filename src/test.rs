use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use tokio::{
    io::{duplex, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    time::{sleep, Duration},
};
use tracing::{info, trace};
use tracing_subscriber::filter::EnvFilter;

use crate::{StreamMultiplexor, StreamMultiplexorConfig};

#[ctor::ctor]
fn init_tests() {
    let mut filter = {
        if let Ok(filter) = EnvFilter::try_from_default_env() {
            filter
        } else {
            EnvFilter::default()
        }
    };
    filter = filter.add_directive("tokio_stream_multiplexor=trace".parse().unwrap());

    tracing_subscriber::fmt::Subscriber::builder()
        .without_time()
        .with_test_writer()
        .with_env_filter(filter)
        .init();
}

#[test]
fn test_readme_deps() {
    version_sync::assert_markdown_deps_updated!("src/lib.rs");
}

#[test]
fn test_html_root_url() {
    version_sync::assert_html_root_url_updated!("src/lib.rs");
}

#[tokio::test]
#[tracing::instrument]
async fn connect_no_listen_fails() {
    let (a, b) = duplex(10);

    let sm_a = StreamMultiplexor::new(
        a,
        StreamMultiplexorConfig::default().with_identifier("sm_a"),
    );
    let _sm_b = StreamMultiplexor::new(
        b,
        StreamMultiplexorConfig::default().with_identifier("sm_b"),
    );

    assert!(matches!(sm_a.connect(22).await, Err(_)));
}

#[tokio::test]
#[tracing::instrument]
async fn connect_listen_succeeds() {
    let (a, b) = duplex(10);

    let sm_a = StreamMultiplexor::new(
        a,
        StreamMultiplexorConfig::default().with_identifier("sm_a"),
    );
    let sm_b = StreamMultiplexor::new(
        b,
        StreamMultiplexorConfig::default().with_identifier("sm_b"),
    );

    tokio::spawn(async move {
        let _connection = sm_b.bind(22).await.unwrap().accept().await;
        sleep(Duration::from_millis(50)).await;
    });

    assert!(matches!(sm_a.connect(22).await, Ok(_)));
}

#[tokio::test]
#[tracing::instrument]
async fn dropped_connection_rsts() {
    let (a, b) = duplex(10);

    let sm_a = StreamMultiplexor::new(
        a,
        StreamMultiplexorConfig::default().with_identifier("sm_a"),
    );
    let sm_b = StreamMultiplexor::new(
        b,
        StreamMultiplexorConfig::default().with_identifier("sm_b"),
    );

    let listener = sm_b.bind(22).await.unwrap();
    tokio::spawn(async move {
        listener.accept().await.unwrap();
    });

    let res = sm_a.connect(22).await;
    assert!(matches!(res, Ok(_)));
    let res = res.unwrap().write_all(&[0u8; 1024]).await;
    println!("{:?}", res);
    assert!(matches!(res, Err(_)));
}

#[tokio::test]
#[tracing::instrument]
async fn connected_stream_passes_data() {
    let (a, b) = duplex(10);

    let input_bytes: Vec<u8> = (0..(1024 * 1024)).map(|_| rand::random::<u8>()).collect();
    let len = input_bytes.len();

    let sm_a = StreamMultiplexor::new(
        a,
        StreamMultiplexorConfig::default().with_identifier("sm_a"),
    );
    let sm_b = StreamMultiplexor::new(
        b,
        StreamMultiplexorConfig::default().with_identifier("sm_b"),
    );

    let input_bytes_clone = input_bytes.clone();
    tokio::spawn(async move {
        let mut conn = sm_b.bind(22).await.unwrap().accept().await.unwrap();
        let mut i = 0;
        while i < input_bytes_clone.len() {
            let res = conn.write_all(&input_bytes_clone[i..i + 1024]).await;
            assert!(matches!(res, Ok(..)));
            i += 1024;
        }
        sleep(Duration::from_millis(1)).await;
        info!("Done send");
    });

    let mut output_bytes: Vec<u8> = vec![];

    let mut conn = sm_a.connect(22).await.unwrap();
    while output_bytes.len() < len {
        let mut buf = [0u8; 2048];
        let bytes = conn.read(&mut buf).await.unwrap();
        if bytes == 0 {
            break;
        }
        output_bytes.extend_from_slice(&buf[..bytes]);
    }

    assert!(input_bytes == output_bytes);
}

#[tokio::test]
#[tracing::instrument]
async fn wrapped_stream_disconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        listener.accept().await.unwrap();
    });

    let stream = TcpStream::connect(local_addr).await.unwrap();

    let sm_a = StreamMultiplexor::new(
        stream,
        StreamMultiplexorConfig::default().with_identifier("sm_a"),
    );
    sleep(Duration::from_millis(100)).await;

    assert!(matches!(sm_a.connect(1024).await, Err(..)));
    assert!(matches!(sm_a.bind(1024).await, Err(..)));
}

#[tokio::test]
#[tracing::instrument]
async fn wrapped_stream_disconnect_listener() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let _ = listener.accept().await.unwrap();
        sleep(Duration::from_millis(100)).await;
    });

    let stream = TcpStream::connect(local_addr).await.unwrap();
    let sm_a = StreamMultiplexor::new(
        stream,
        StreamMultiplexorConfig::default().with_identifier("sm_a"),
    );
    let listener = sm_a.bind(1024).await.unwrap();

    sleep(Duration::from_millis(100)).await;

    assert!(matches!(listener.accept().await, Err(..)));
}

#[tokio::test]
#[tracing::instrument]
async fn wrapped_stream_disconnect_after_bind_connect_accept() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        info!("spawn 0");
        let (stream, _) = listener.accept().await.unwrap();
        let sm_b = StreamMultiplexor::new_paused(
            stream,
            StreamMultiplexorConfig::default().with_identifier("sm_b"),
        );
        let listener22 = sm_b.bind(22).await.unwrap();
        let listener23 = sm_b.bind(23).await.unwrap();

        // sleep(Duration::from_millis(50)).await;

        sm_b.start();

        let (exit_tx, mut exit_rx) = mpsc::channel(3);

        let exit_tx_clone = exit_tx.clone();
        tokio::spawn(async move {
            info!("spawn 1");
            while let Ok(mut stream) = listener22.accept().await {
                info!("accept 1");
                stream
                    .write_all(b"Hello, ")
                    .await
                    .expect("stream.write_all succeeds");
                info!("write_all 1");
                break;
            }
            exit_tx_clone.send(()).await.expect("exit_tx.send succeeds");
        });

        tokio::spawn(async move {
            info!("spawn 2");
            while let Ok(mut stream) = listener23.accept().await {
                info!("accept 2");
                stream
                    .write_all(b"world!\n")
                    .await
                    .expect("stream.write_all succeeds");
                info!("write_all 2");
                break;
            }
            exit_tx.send(()).await.expect("exit_tx.send succeeds");
        });

        exit_rx.recv().await;
        exit_rx.recv().await;
    });

    let stream = TcpStream::connect(local_addr).await.unwrap();
    let sm_a = StreamMultiplexor::new(
        stream,
        StreamMultiplexorConfig::default().with_identifier("sm_a"),
    );

    let watch_connected = sm_a.watch_connected();

    let mut buf = [0u8; 16];

    let bytes = sm_a
        .connect(22)
        .await
        .expect("connect succeeds")
        .read(&mut buf)
        .await
        .expect("stream.recv succeeds");
    print!(
        "{}",
        std::str::from_utf8(&buf[0..bytes]).expect("utf8 decodes")
    );
    let bytes = sm_a
        .connect(23)
        .await
        .expect("connect succeeds")
        .read(&mut buf)
        .await
        .expect("stream.recv succeeds");
    print!(
        "{}",
        std::str::from_utf8(&buf[0..bytes]).expect("utf8 decodes")
    );

    sleep(Duration::from_millis(100)).await;

    assert_eq!(*watch_connected.borrow(), false);
}

#[tokio::test]
#[tracing::instrument]
async fn wrapped_stream_disconnect_subscribe_before() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let _ = listener.accept().await.unwrap();
        sleep(Duration::from_millis(100)).await;
    });

    let stream = TcpStream::connect(local_addr).await.unwrap();
    let sm_a = StreamMultiplexor::new(
        stream,
        StreamMultiplexorConfig::default().with_identifier("sm_a"),
    );
    let mut connected = sm_a.watch_connected();
    assert_eq!(*connected.borrow(), true);

    let listener = sm_a.bind(1024).await.unwrap();

    sleep(Duration::from_millis(100)).await;

    assert!(matches!(connected.changed().await, Ok(())));
    assert_eq!(*connected.borrow(), false);
    assert!(matches!(listener.accept().await, Err(..)));
}

#[tokio::test]
#[tracing::instrument]
async fn wrapped_stream_disconnect_subscribe_after() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let _ = listener.accept().await.unwrap();
        sleep(Duration::from_millis(100)).await;
    });

    let stream = TcpStream::connect(local_addr).await.unwrap();
    let sm_a = StreamMultiplexor::new(
        stream,
        StreamMultiplexorConfig::default().with_identifier("sm_a"),
    );

    let listener = sm_a.bind(1024).await.unwrap();

    sleep(Duration::from_millis(100)).await;

    let connected = sm_a.watch_connected();

    assert_eq!(*connected.borrow(), false);

    assert!(matches!(listener.accept().await, Err(..)));
}

#[tokio::test]
#[tracing::instrument]
async fn listen_accept_multiple() {
    let (a, b) = duplex(10);

    let input_bytes: Vec<u8> = (0..(1024 * 1024)).map(|_| rand::random::<u8>()).collect();
    let len = input_bytes.len();

    let sm_a = StreamMultiplexor::new(
        a,
        StreamMultiplexorConfig::default().with_identifier("sm_a"),
    );
    let sm_b = StreamMultiplexor::new(
        b,
        StreamMultiplexorConfig::default().with_identifier("sm_b"),
    );

    let input_bytes_clone = input_bytes.clone();
    tokio::spawn(async move {
        let listener = sm_b.bind(22).await.unwrap();
        loop {
            let input_bytes_clone = input_bytes_clone.clone();
            let mut conn = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut i = 0;
                while i < input_bytes_clone.len() {
                    let res = conn.write_all(&input_bytes_clone[i..i + 1024]).await;
                    assert!(matches!(res, Ok(..)));
                    i += 1024;
                }
            });
        }
    });

    let mut output_bytes0: Vec<u8> = vec![];
    let mut output_bytes1: Vec<u8> = vec![];

    let mut conn0 = sm_a.connect(22).await.unwrap();
    let mut conn1 = sm_a.connect(22).await.unwrap();
    while output_bytes0.len() < len {
        let mut buf = [0u8; 2048];
        let bytes = conn0.read(&mut buf).await.unwrap();
        output_bytes0.extend_from_slice(&buf[..bytes]);
        let bytes = conn1.read(&mut buf).await.unwrap();
        output_bytes1.extend_from_slice(&buf[..bytes]);
    }

    assert_eq!(input_bytes, output_bytes0);
    assert_eq!(input_bytes, output_bytes1);
}

#[tokio::test]
#[tracing::instrument]
async fn listen_multiple_accept() {
    let (a, b) = duplex(10);

    let input_bytes: Vec<u8> = (0..(1024 * 1024)).map(|_| rand::random::<u8>()).collect();
    let len = input_bytes.len();

    let sm_a = StreamMultiplexor::new(
        a,
        StreamMultiplexorConfig::default().with_identifier("sm_a"),
    );
    let sm_b = StreamMultiplexor::new(
        b,
        StreamMultiplexorConfig::default().with_identifier("sm_b"),
    );

    let input_bytes_clone = input_bytes.clone();
    let listener = sm_b.bind(22).await.unwrap();
    tokio::spawn(async move {
        let mut conn = listener.accept().await.unwrap();
        tokio::spawn(async move {
            let mut i = 0;
            while i < input_bytes_clone.len() {
                let res = conn.write_all(&input_bytes_clone[i..i + 1024]).await;
                assert!(matches!(res, Ok(..)));
                i += 1024;
            }
        });
    });

    let input_bytes_clone = input_bytes.clone();
    let listener = sm_b.bind(23).await.unwrap();
    tokio::spawn(async move {
        let mut conn = listener.accept().await.unwrap();
        tokio::spawn(async move {
            let mut i = 0;
            while i < input_bytes_clone.len() {
                let res = conn.write_all(&input_bytes_clone[i..i + 1024]).await;
                assert!(matches!(res, Ok(..)));
                i += 1024;
            }
        });
    });

    let mut output_bytes0: Vec<u8> = vec![];
    let mut output_bytes1: Vec<u8> = vec![];

    let mut conn0 = sm_a.connect(22).await.unwrap();
    let mut conn1 = sm_a.connect(23).await.unwrap();
    while output_bytes0.len() < len {
        let mut buf = [0u8; 2048];
        let bytes = conn0.read(&mut buf).await.unwrap();
        output_bytes0.extend_from_slice(&buf[..bytes]);
        let bytes = conn1.read(&mut buf).await.unwrap();
        output_bytes1.extend_from_slice(&buf[..bytes]);
    }

    assert_eq!(input_bytes, output_bytes0);
    assert_eq!(input_bytes, output_bytes1);
}

#[tokio::test]
#[tracing::instrument]
async fn test_start_paused() {
    let (a, b) = duplex(10);

    let sm_a = StreamMultiplexor::new(
        a,
        StreamMultiplexorConfig::default().with_identifier("sm_a"),
    );
    let sm_b = Arc::from(StreamMultiplexor::new_paused(
        b,
        StreamMultiplexorConfig::default().with_identifier("sm_b"),
    ));

    let bound = Arc::from(AtomicBool::new(false));
    let accepted = Arc::from(AtomicBool::new(false));
    let connected = Arc::from(AtomicBool::new(false));

    let sm_b_clone = sm_b.clone();
    let bound_clone = bound.clone();
    let accepted_clone = accepted.clone();
    tokio::spawn(async move {
        trace!("bind");
        let listener = sm_b_clone.bind(22).await.unwrap();
        bound_clone.store(true, Ordering::Relaxed);
        trace!("accept");
        let _ = listener.accept().await;
        accepted_clone.store(true, Ordering::Relaxed);
        trace!("accepted")
    });

    let connected_clone = connected.clone();
    tokio::spawn(async move {
        trace!("connect");
        assert!(matches!(sm_a.connect(22).await, Ok(_)));
        connected_clone.store(true, Ordering::Relaxed);
        trace!("connected");
    });

    sleep(Duration::from_millis(250)).await;

    assert!(bound.load(Ordering::Relaxed));
    assert!(!accepted.load(Ordering::Relaxed));
    assert!(!connected.load(Ordering::Relaxed));

    sm_b.start();

    sleep(Duration::from_millis(5)).await;

    assert!(bound.load(Ordering::Relaxed));
    assert!(accepted.load(Ordering::Relaxed));
    assert!(connected.load(Ordering::Relaxed));
}
