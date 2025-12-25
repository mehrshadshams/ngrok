use std::net::SocketAddr;

use async_tungstenite::tokio::accept_async; // Use async-tungstenite for compatibility
use futures::StreamExt;
use tokio::net::TcpListener;
use tokio::{io::copy_bidirectional, net::TcpStream};
use tokio_util::compat::FuturesAsyncReadCompatExt; // Import compat trait
use tokio_yamux::{Config, Session};
use tracing::{error, info};
use ws_stream_tungstenite::WsStream; // For Yamux session next()

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    tracing_subscriber::fmt::init();

    // 1. LISTEN FOR THE AGENT (The Tunnel)
    // In a real app, you'd put this in a separate task or use a select! loop,
    // but for simplicity, we accept ONE agent first, then start serving users.
    let tunnel_listener = TcpListener::bind("0.0.0.0:8089").await?;
    info!("SERVER: Waiting for Agent to connect on port 8089...");

    loop {
        match tunnel_listener.accept().await {
            Ok((socket, addr)) => {
                info!("New control connection from {}", addr);
                tokio::spawn(async move {
                    _ = handle_agent_connection(socket, addr).await;
                });
            }
            Err(e) => {
                error!("Error accepting control connection: {}", e);
                // Continue accepting connections despite individual failures
            }
        }
    }
}

async fn handle_agent_connection(
    raw_socket: TcpStream,
    addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    // let (raw_socket, addr) = tunnel_listener.accept().await?;
    info!("SERVER: Agent connecting from: {}", addr);

    // 2. UPGRADE TO WEBSOCKET
    // async-tungstenite v0.29 accepts tokio::net::TcpStream directly with tokio-runtime feature
    let ws_stream = accept_async(raw_socket).await?;
    info!("SERVER: Agent upgraded to WebSocket!");

    // 3. WRAP IN YAMUX
    // Convert WS message stream -> AsyncRead/Write Byte stream
    let ws_stream_adapter = WsStream::new(ws_stream);

    // Adapt Futures AsyncRead/Write to Tokio AsyncRead/Write
    let compat_stream = ws_stream_adapter.compat();

    let config = Config::default();
    // tokio-yamux config doesn't have set_keep_alive_interval exposing directly usually,
    // but Yamux config does. tokio-yamux re-exports yamux::Config?
    // Let's check. If not, we skip it or use available options.
    // config.set_keep_alive_interval(Some(std::time::Duration::from_secs(10)));
    // Commenting out keep-alive for now to ensure compilation, or check API later.

    // Session::new_server takes the stream and config.
    let mut session = Session::new_server(compat_stream, config);

    // We need the 'control' handle to open streams *down* to the agent
    let ctrl = session.control();

    // Spawn the Yamux connection loop in the background.
    // This pumps data back and forth.
    tokio::spawn(async move {
        while let Some(_) = session.next().await {}
        info!("SERVER: Agent disconnected!");
    });

    // 4. LISTEN FOR PUBLIC USERS (The Internet)
    let public_listener = TcpListener::bind("0.0.0.0:8081").await?;
    info!("SERVER: Ready! Listening for public users on port 8081...");

    loop {
        let (mut public_socket, user_addr) = public_listener.accept().await?;
        info!("SERVER: New user: {}", user_addr);

        // Clone the control handle so we can move it into the task
        // tokio_yamux::Control is cloneable
        let mut ctrl_clone = ctrl.clone();

        tokio::spawn(async move {
            // A. Open a new stream INSIDE the tunnel to the agent
            info!("SERVER: Opening tunnel stream for user...");
            let result = ctrl_clone.open_stream().await;

            match result {
                Ok(mut tunnel_stream) => {
                    // B. Bridge the Public User <-> Tunnel Stream
                    // This pipes data both ways until one side disconnects.
                    info!("SERVER: Bridging traffic...");
                    // tunnel_stream implements tokio::io::AsyncRead/Write
                    let _ = copy_bidirectional(&mut public_socket, &mut tunnel_stream).await;
                    info!("SERVER: Connection closed for {}", user_addr);
                }
                Err(e) => {
                    error!("SERVER: Failed to open stream to agent: {}", e);
                }
            }
        });
    }
}
