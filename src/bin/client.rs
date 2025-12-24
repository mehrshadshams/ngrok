use anyhow::{Context, Result};
use clap::Parser;
use futures::{future, StreamExt};
use tokio::net::TcpStream;
use tokio_yamux::{Config, Session, StreamHandle};
use tracing::{error, info};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Address of that ngrok server's control port
    #[arg(short, long, default_value = "127.0.0.1:4444")]
    server_addr: String,

    /// Local port to forward traffic to
    #[arg(short, long, default_value_t = 3000)]
    local_port: u16,

    /// Secret token for authentication
    #[arg(long, default_value = "mysecret")]
    secret: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    // Connect to the server
    info!("Connecting to server at {}", args.server_addr);
    let mut socket = TcpStream::connect(&args.server_addr)
        .await
        .context("Failed to connect to server")?;

    // Handshake
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    {
        // 1. Send Secret
        let secret_packet = format!("{}\n", args.secret);
        socket
            .write_all(secret_packet.as_bytes())
            .await
            .context("Failed to send secret")?;

        // 2. Read Response using BufReader
        // We scope BufReader here so it is dropped before we pass socket to Yamux
        let mut buf_reader = BufReader::new(&mut socket);
        let mut response = String::new();
        buf_reader
            .read_line(&mut response)
            .await
            .context("Failed to read handshake response")?;

        if response.trim() != "OK" {
            anyhow::bail!("Authentication failed: Server rejected secret");
        }
    }
    // BufReader dropped here, socket is free.
    // NOTE: If server sent more than "OK\n", those bytes might have been buffered and lost.
    // Since server waits for handshake before starting Yamux, this is safe.

    info!("Authentication successful");

    // Setup Yamux - Client mode
    let config = Config::default();
    let mut session = Session::new_client(socket, config);

    info!("Connected using Yamux. Waiting for incoming streams...");

    // Accept incoming streams from the server
    while let Some(result) = session.next().await {
        match result {
            Ok(server_stream) => {
                info!("Server opened a new stream!");
                let local_port = args.local_port;
                tokio::spawn(async move {
                    if let Err(e) = handle_server_stream(server_stream, local_port).await {
                        error!("Error handling stream: {}", e);
                    }
                });
            }
            Err(e) => {
                error!("Connection error: {}", e);
                break;
            }
        }
    }

    Ok(())
}

async fn handle_server_stream(server_stream: StreamHandle, local_port: u16) -> Result<()> {
    // Connect to local service
    let local_addr = format!("127.0.0.1:{}", local_port);
    info!("Forwarding stream to {}", local_addr);

    let mut local_socket = TcpStream::connect(&local_addr)
        .await
        .context("Failed to connect to local service")?;

    // server_stream already implements tokio::io::AsyncRead/Write
    let (mut server_r, mut server_w) = tokio::io::split(server_stream);
    let (mut local_r, mut local_w) = local_socket.split();

    let server_to_local = tokio::io::copy(&mut server_r, &mut local_w);
    let local_to_server = tokio::io::copy(&mut local_r, &mut server_w);

    match future::try_join(server_to_local, local_to_server).await {
        Ok(_) => info!("Tunnel closed successfully"),
        Err(e) => error!("Tunnel error: {}", e),
    }

    Ok(())
}
