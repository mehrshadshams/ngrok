# Rust Ngrok Clone

A high-performance, async, multiplexed reverse tunnel service written in Rust. This tool allows you to expose a local TCP service (like a web server) to the public internet via a relay server, similar to `ngrok`.

## 🚀 Features
- **Reverse Tunneling**: Expose local ports behind NAT/Firewalls.
- **Multiplexing**: Uses `yamux` to run multiple logical streams over a single TCP connection.
- **Async I/O**: Built on `tokio` for high concurrency.
- **Memory Safe**: Written in pure, safe Rust.

## 🛠️ Architecture
The system consists of two binaries:
1.  **Server (Relay)**: Listens on a public port for incoming traffic and a control port for worker connections.
2.  **Client (Worker)**: Connects to the Server's control port and bridges traffic to your local service.

## 📦 Getting Started

### Prerequisites
- Rust and Cargo installed.

### 1. Start the Relay Server
The server routes traffic from the public world to your connected client.
```bash
# Listen on control port 8089 and public ingress port 8081
cargo run --bin server
```

### 2. Start the Local Client
The client connects to the server and forwards traffic to your local app (e.g., running on port 3000).
```bash
# Connect to server at 127.0.0.1:8089 and forward to localhost:3000
cargo run --bin client -- --local-port 3000
```

### 3. Verify Connectivity
Start a local web server (e.g., Python) and access it through the tunnel.
```bash
# In a separate terminal
python3 -m http.server 3000

# Access via the public ingress port
curl http://localhost:8081/
```

## 🗺️ Production Roadmap
The current version is an functional MVP. To make this production-ready, the following roadmap is planned:

### 1. Security (Critical)
- [ ] **End-to-End Encryption (TLS)**: Wrap control and data streams with `tokio-rustls`.
- [ ] **Authentication**: Require an `authtoken` handshake before accepting client connections.

### 2. Reliability
- [ ] **Automatic Reconnection**: Implement exponential backoff for client retries.
- [ ] **Heartbeats**: Detect and close stalled connections using keep-alives.
- [ ] **Graceful Shutdown**: Handle signals to close streams cleanly.

### 3. Scalability
- [ ] **Hostname Routing**: Route traffic based on `Host` headers (e.g., `app.tunnel.com`) instead of dedicated ports.
- [ ] **Connection Pooling**: Limit concurrent streams per user.

### 4. HTTP Protocol Features
- [ ] **Header Rewriting**: Rewrite `Host` headers to match the local target (e.g., `localhost:3000`).
- [ ] **X-Forwarded-For**: Append original client IPs to headers.

### 5. Observability
- [ ] **Metrics**: Export connection counts and bandwidth via Prometheus.
- [ ] **Structured Logging**: Add request IDs for distributed tracing.
