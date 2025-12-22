use anyhow::{Context, Result};
use futures::{future, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_yamux::{Config, Control, Session, StreamHandle};
use tracing::{error, info, warn};

/// Holds the active client session/control to open new streams.
#[derive(Clone)]
pub struct AppState {
    pub client_control: Option<Control>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            client_control: None,
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle a control connection from a tunnel client
pub async fn handle_control_connection(socket: TcpStream, state: Arc<Mutex<AppState>>) {
    let config = Config::default();

    // tokio-yamux: Server side session
    let mut session = Session::new_server(socket, config);
    let control = session.control();

    // Save the control handle
    {
        let mut guard = state.lock().await;
        if guard.client_control.is_some() {
            warn!("Replacing existing client connection");
        }
        guard.client_control = Some(control);
    }

    // Drive the session
    tokio::spawn(async move {
        while let Some(result) = session.next().await {
            match result {
                Ok(stream) => {
                    warn!("Client opened an unexpected stream");
                    drop(stream);
                }
                Err(e) => {
                    error!("Yamux connection error: {}", e);
                    break;
                }
            }
        }
        info!("Control connection closed");
    });
}

/// Handle a public connection by creating a stream to the client
pub async fn handle_public_connection(
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

/// Handle a server stream by forwarding to local service
pub async fn handle_server_stream(server_stream: StreamHandle, local_port: u16) -> Result<()> {
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

#[cfg(test)]
pub mod test_utils {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    /// Create a mock local service that echoes data back
    pub async fn create_mock_echo_service(port: u16) -> Result<()> {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let listener = TcpListener::bind(addr).await?;
        
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let (mut reader, mut writer) = socket.split();
                    let _ = tokio::io::copy(&mut reader, &mut writer).await;
                });
            }
        });
        
        // Give the service time to start
        tokio::time::sleep(Duration::from_millis(10)).await;
        Ok(())
    }

    /// Create a mock client that connects to the server and establishes yamux session
    pub async fn create_mock_client(server_addr: &str) -> Result<Session<TcpStream>> {
        let socket = TcpStream::connect(server_addr).await?;
        let config = Config::default();
        let session = Session::new_client(socket, config);
        Ok(session)
    }

    /// Wait for a condition with timeout
    pub async fn wait_for_condition<F, Fut>(mut condition: F, timeout_duration: Duration) -> Result<()>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        timeout(timeout_duration, async {
            loop {
                if condition().await {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.context("Condition timeout")?;
        Ok(())
    }

    /// Get a free port for testing
    pub async fn get_free_port() -> Result<u16> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        drop(listener);
        Ok(port)
    }
}

#[cfg(test)]
mod tests;