use tokio::{
    io::{duplex, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::{sleep, Duration},
};

use tracing_subscriber::filter::EnvFilter;

use super::{Config, StreamMultiplexor};

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

#[tokio::test]
#[tracing::instrument]
async fn connect_no_listen_fails() {
    let (a, b) = duplex(10);

    let sm_a = StreamMultiplexor::new(a, Config::default().with_identifier("sm_a"));
    let _sm_b = StreamMultiplexor::new(b, Config::default().with_identifier("sm_b"));

    assert!(matches!(sm_a.connect(22).await, Err(_)));
}

#[tokio::test]
#[tracing::instrument]
async fn connect_listen_succeeds() {
    let (a, b) = duplex(10);

    let sm_a = StreamMultiplexor::new(a, Config::default().with_identifier("sm_a"));
    let sm_b = StreamMultiplexor::new(b, Config::default().with_identifier("sm_b"));

    tokio::spawn(async move {
        let _connection = sm_b.bind(22).await.unwrap().accept().await;
        sleep(Duration::from_secs(1)).await;
    });

    assert!(matches!(sm_a.connect(22).await, Ok(_)));
}

#[tokio::test]
#[tracing::instrument]
async fn dropped_connection_rsts() {
    let (a, b) = duplex(10);

    let sm_a = StreamMultiplexor::new(a, Config::default().with_identifier("sm_a"));
    let sm_b = StreamMultiplexor::new(b, Config::default().with_identifier("sm_b"));

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

    let sm_a = StreamMultiplexor::new(a, Config::default().with_identifier("sm_a"));
    let sm_b = StreamMultiplexor::new(b, Config::default().with_identifier("sm_b"));

    let input_bytes_clone = input_bytes.clone();
    tokio::spawn(async move {
        let mut conn = sm_b.bind(22).await.unwrap().accept().await.unwrap();
        let mut i = 0;
        while i < input_bytes_clone.len() {
            let res = conn.write_all(&input_bytes_clone[i..i + 1024]).await;
            assert!(matches!(res, Ok(..)));
            i += 1024;
        }
    });

    let mut output_bytes: Vec<u8> = vec![];

    let mut conn = sm_a.connect(22).await.unwrap();
    while output_bytes.len() < len {
        let mut buf = [0u8; 2048];
        let bytes = conn.read(&mut buf).await.unwrap();
        output_bytes.extend_from_slice(&buf[..bytes]);
    }

    assert_eq!(input_bytes, output_bytes);
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

    let sm_a = StreamMultiplexor::new(stream, Config::default().with_identifier("sm_a"));
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
    let sm_a = StreamMultiplexor::new(stream, Config::default().with_identifier("sm_a"));
    let listener = sm_a.bind(1024).await.unwrap();

    sleep(Duration::from_millis(100)).await;

    assert!(matches!(listener.accept().await, Err(..)));
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
    let sm_a = StreamMultiplexor::new(stream, Config::default().with_identifier("sm_a"));
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
    let sm_a = StreamMultiplexor::new(stream, Config::default().with_identifier("sm_a"));

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

    let sm_a = StreamMultiplexor::new(a, Config::default().with_identifier("sm_a"));
    let sm_b = StreamMultiplexor::new(b, Config::default().with_identifier("sm_b"));

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

    let sm_a = StreamMultiplexor::new(a, Config::default().with_identifier("sm_a"));
    let sm_b = StreamMultiplexor::new(b, Config::default().with_identifier("sm_b"));

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
