use anyhow::{Context, Result};
use futures::{future, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_retry::strategy::ExponentialBackoff;
use tokio_yamux::{Config, Control, Session, StreamHandle};
use tracing::{error, info, warn};

/// Configuration for retry mechanisms
#[derive(Clone, Debug)]
pub struct RetryConfig {
    pub max_attempts: usize,
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(5),
        }
    }
}

/// Determines if an error is transient and should be retried
pub fn is_transient_error(error: &anyhow::Error) -> bool {
    let error_str = error.to_string().to_lowercase();
    
    // Network-related transient errors
    error_str.contains("connection refused") ||
    error_str.contains("connection reset") ||
    error_str.contains("connection aborted") ||
    error_str.contains("timed out") ||
    error_str.contains("timeout") ||
    error_str.contains("temporary failure") ||
    error_str.contains("resource temporarily unavailable") ||
    // Yamux-specific transient errors
    error_str.contains("stream reset") ||
    error_str.contains("session closed") ||
    // I/O errors that might be transient
    error_str.contains("broken pipe") ||
    error_str.contains("network unreachable")
}

/// Retry a future with exponential backoff for transient errors
pub async fn retry_with_backoff<F, Fut, T>(
    operation: F,
    config: &RetryConfig,
    operation_name: &str,
) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let retry_strategy = ExponentialBackoff::from_millis(config.initial_delay.as_millis() as u64)
        .max_delay(config.max_delay)
        .take(config.max_attempts);

    let mut attempts = 0;
    let mut last_error = None;

    for delay in retry_strategy {
        attempts += 1;
        
        match operation().await {
            Ok(result) => return Ok(result),
            Err(error) => {
                if is_transient_error(&error) {
                    warn!("Transient error in {} (attempt {}): {}. Retrying...", 
                         operation_name, attempts, error);
                    last_error = Some(error);
                    
                    // If this wasn't the last attempt, wait before retrying
                    if attempts < config.max_attempts {
                        tokio::time::sleep(delay).await;
                    }
                } else {
                    error!("Non-transient error in {}: {}. Not retrying.", operation_name, error);
                    return Err(error);
                }
            }
        }
    }

    // If we exhausted all retries, return the last error
    Err(last_error
        .unwrap_or_else(|| anyhow::anyhow!("Operation failed"))
        .context(format!("Failed {} after {} attempts", operation_name, attempts)))
}

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
    handle_public_connection_with_retry(public_socket, state, &RetryConfig::default()).await
}

/// Handle a public connection with configurable retry behavior
pub async fn handle_public_connection_with_retry(
    mut public_socket: TcpStream,
    state: Arc<Mutex<AppState>>,
    retry_config: &RetryConfig,
) -> Result<()> {
    let control = {
        let guard = state.lock().await;
        guard
            .client_control
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No client connected"))?
    };

    // Open stream with retry logic for transient failures
    let yamux_stream = {
        let control = Arc::new(Mutex::new(control));
        retry_with_backoff(
            || {
                let control = control.clone();
                async move {
                    let mut control_guard = control.lock().await;
                    control_guard
                        .open_stream()
                        .await
                        .context("Failed to open stream to client")
                }
            },
            retry_config,
            "stream creation",
        )
        .await?
    };

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
    handle_server_stream_with_retry(server_stream, local_port, &RetryConfig::default()).await
}

/// Handle a server stream with configurable retry behavior
pub async fn handle_server_stream_with_retry(
    server_stream: StreamHandle,
    local_port: u16,
    retry_config: &RetryConfig,
) -> Result<()> {
    // Connect to local service with retry logic for transient failures
    let local_addr = format!("127.0.0.1:{}", local_port);
    info!("Forwarding stream to {}", local_addr);

    let mut local_socket = retry_with_backoff(
        || async {
            TcpStream::connect(&local_addr)
                .await
                .context("Failed to connect to local service")
        },
        retry_config,
        "local service connection",
    )
    .await?;

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
    use std::net::SocketAddr;
    use std::time::Duration;
    use tokio::net::TcpListener;
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

    /// Create a mock local service that fails initially then succeeds (for testing retries)
    pub async fn create_flaky_mock_service(port: u16, fail_count: Arc<Mutex<u32>>) -> Result<()> {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let listener = TcpListener::bind(addr).await?;
        
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let fail_count = fail_count.clone();
                tokio::spawn(async move {
                    // Check if we should fail this connection
                    let should_fail = {
                        let mut count = fail_count.lock().await;
                        if *count > 0 {
                            *count -= 1;
                            true
                        } else {
                            false
                        }
                    };
                    
                    if should_fail {
                        // Close the connection immediately to simulate failure
                        drop(socket);
                    } else {
                        // Echo service
                        let (mut reader, mut writer) = socket.split();
                        let _ = tokio::io::copy(&mut reader, &mut writer).await;
                    }
                });
            }
        });
        
        // Give the service time to start
        tokio::time::sleep(Duration::from_millis(50)).await;
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