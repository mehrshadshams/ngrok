# Build Stage
FROM rust:1.88 AS builder
WORKDIR /app
COPY . .
# Build release binary
RUN cargo build --release --bin server

# Runtime Stage
FROM debian:bookworm-slim
WORKDIR /app
# Copy the binary from the builder
COPY --from=builder /app/target/release/server .

# Expose the ports
EXPOSE 8089 8081

# Run the server
CMD ["./server"]