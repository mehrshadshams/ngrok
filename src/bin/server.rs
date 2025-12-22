use anyhow::{Context, Result};
use clap::Parser;
use futures::{future, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_yamux::{Config, Control, Session};
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Port to listen for client (worker) connections
    #[arg(short, long, default_value_t = 4444)]
    control_port: u16,

    /// Port to listen for public ingress connections
    #[arg(short, long, default_value_t = 8080)]
    public_port: u16,
}

/// Holds the active client session/control to open new streams.
struct AppState {
    client_control: Option<Control>,
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let state = Arc::new(Mutex::new(AppState {
        client_control: None,
    }));

    // Start Control Server (listener for the worker)
    let control_addr = SocketAddr::from(([0, 0, 0, 0], args.control_port));
    let control_listener = TcpListener::bind(control_addr).await?;
    info!("Control server listening on {}", control_addr);

    let state_clone = state.clone();
    tokio::spawn(async move {
        loop {
            match control_listener.accept().await {
                Ok((socket, addr)) => {
                    info!("New control connection from {}", addr);
                    handle_control_connection(socket, state_clone.clone()).await;
                }
                Err(e) => {
                    error!("Error accepting control connection: {}", e);
                    // Continue accepting connections despite individual failures
                }
            }
        }
    });

    // Start Public Server (listener for users)
    let public_addr = SocketAddr::from(([0, 0, 0, 0], args.public_port));
    let public_listener = TcpListener::bind(public_addr).await?;
    info!("Public ingress listening on {}", public_addr);

    loop {
        match public_listener.accept().await {
            Ok((socket, addr)) => {
                info!("New public connection from {}", addr);
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_public_connection(socket, state).await {
                        error!("Error handling public connection from {}: {}", addr, e);
                    }
                });
            }
            Err(e) => {
                error!("Error accepting public connection: {}", e);
                // Continue accepting connections despite individual failures
            }
        }
    }
}

async fn handle_control_connection(socket: TcpStream, state: Arc<Mutex<AppState>>) {
    let peer_addr = socket.peer_addr().ok();
    let config = Config::default();

    // tokio-yamux: Server side session
    let mut session = Session::new_server(socket, config);
    let control = session.control();

    // Save the control handle
    {
        let mut guard = state.lock().await;
        if guard.client_control.is_some() {
            warn!("Replacing existing client connection with new connection from {:?}", peer_addr);
        }
        guard.client_control = Some(control);
    }

    // Drive the session
    tokio::spawn(async move {
        while let Some(result) = session.next().await {
            match result {
                Ok(stream) => {
                    warn!("Client from {:?} opened an unexpected stream", peer_addr);
                    drop(stream);
                }
                Err(e) => {
                    error!("Yamux connection error from {:?}: {}", peer_addr, e);
                    break;
                }
            }
        }
        info!("Control connection from {:?} closed", peer_addr);
    });
}

async fn handle_public_connection(
    mut public_socket: TcpStream,
    state: Arc<Mutex<AppState>>,
) -> Result<()> {
    let mut control = {
        let guard = state.lock().await;
        guard
            .client_control
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No client connected"))?
    };

    // Open stream. Control returns tokio_yamux::Stream which implements tokio AsyncRead/AsyncWrite
    let yamux_stream = control
        .open_stream()
        .await
        .context("Failed to open stream to client")?;

    let (mut client_r, mut client_w) = tokio::io::split(yamux_stream);
    let (mut public_r, mut public_w) = public_socket.split();

    let client_to_public = tokio::io::copy(&mut client_r, &mut public_w);
    let public_to_client = tokio::io::copy(&mut public_r, &mut client_w);

    match future::try_join(client_to_public, public_to_client).await {
        Ok(_) => info!("Connection bridge finished"),
        Err(e) => error!("Bridge error: {}", e),
    }

    Ok(())
}
