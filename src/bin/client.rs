use async_tungstenite::tokio::connect_async;
use clap::Parser;
use futures::StreamExt;
use tokio::io::copy_bidirectional;
use tokio::net::TcpStream;
use tokio_util::compat::FuturesAsyncReadCompatExt; // Import compat
use tokio_yamux::{Config, Session}; // Import StreamHandle
use tracing::{error, info};
use ws_stream_tungstenite::WsStream; // For Yamux session next

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Address of that ngrok server's control port
    #[arg(short, long, default_value = "127.0.0.1:8089")]
    server_addr: String,

    /// Local port to forward traffic to
    #[arg(short, long)]
    local_port: u16,

    /// Secret token for authentication
    #[arg(long, default_value = "mysecret")]
    secret: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    // CONFIGURATION
    // Where is the public server?
    // Matches server2.rs listening port 8089
    let server_url = format!("ws://{}", args.server_addr);
    // Where is your local app?
    let local_app_addr = format!("127.0.0.1:{}", args.local_port);

    info!("AGENT: Connecting to server at {server_url}...");

    // 1. CONNECT TO SERVER (WebSocket)
    // connect_async accepts &str which implements IntoClientRequest
    let (ws_stream, _) = connect_async(server_url).await?;
    info!("AGENT: Connected to Server via WebSocket!");

    // 2. WRAP IN YAMUX
    let ws_stream_adapter = WsStream::new(ws_stream);

    // Adapt Futures AsyncRead/Write to Tokio AsyncRead/Write
    let compat_stream = ws_stream_adapter.compat();

    let config = Config::default();
    let mut session = Session::new_client(compat_stream, config);

    info!("AGENT: Connected using Yamux. Waiting for incoming requests...");

    // 3. LISTEN FOR INCOMING STREAMS FROM SERVER
    // The server "opens" streams when a public user hits port 8081 (as per server2.rs).
    while let Some(stream_result) = session.next().await {
        match stream_result {
            Ok(mut tunnel_stream) => {
                // tunnel_stream is tokio_yamux::StreamHandle
                info!("AGENT: Incoming request! Connecting to local app...");

                // 4. CONNECT TO LOCAL APP
                match TcpStream::connect(local_app_addr.clone()).await {
                    Ok(mut local_socket) => {
                        // 5. BRIDGE TUNNEL <-> LOCAL APP
                        tokio::spawn(async move {
                            let _ = copy_bidirectional(&mut tunnel_stream, &mut local_socket).await;
                            info!("AGENT: Request finished.");
                        });
                    }
                    Err(e) => error!("AGENT: Failed to connect to local app: {e}"),
                }
            }
            Err(e) => error!("AGENT: Multiplex connection error: {e}"),
        }
    }

    Ok(())
}
