use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};
use tokio_stream_multiplexor::{StreamMultiplexor, StreamMultiplexorConfig};

#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    let (stream_0, stream_1) = duplex(10);

    let mux_0 = StreamMultiplexor::new(stream_0, StreamMultiplexorConfig::default());
    let mux_1 = StreamMultiplexor::new(stream_1, StreamMultiplexorConfig::default());

    let listener = mux_0.bind(23).await?;
    tokio::spawn(async move {
        while let Ok(mut stream) = listener.accept().await {
            let _ = stream.write_all(b"Hello, world!").await;
        }
    });

    let mut stream = mux_1.connect(23).await?;
    let mut buf = [0u8; 16];
    if let Ok(bytes) = stream.read(&mut buf).await {
        println!("{:?}", std::str::from_utf8(&buf[0..bytes]));
    }

    Ok(())
}
