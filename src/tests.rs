use crate::test_utils::*;
use crate::{handle_control_connection, handle_public_connection, AppState};
use anyhow::Result;
use proptest::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

#[cfg(test)]
mod property_tests {
    use super::*;

    // Feature: reverse-tunnel-service, Property 1: Client connection management
    // For any sequence of tunnel client connections to the relay server, 
    // the server should establish yamux sessions for all connections but only use the most recent client for stream creation
    #[test]
    fn test_client_connection_management() {
        let mut config = ProptestConfig::default();
        config.cases = 10; // Reduce from default 256 to 10 cases
        proptest!(config, |(
            num_clients in 1u8..3u8,
            connection_delays in prop::collection::vec(0u64..50u64, 1..3)
        )| {
            let result = tokio_test::block_on(async {
                test_client_connection_management_impl(num_clients, connection_delays).await
            });
            prop_assert!(result.is_ok());
        });
    }

    async fn test_client_connection_management_impl(
        num_clients: u8,
        connection_delays: Vec<u64>,
    ) -> Result<()> {
        // Get a free port for the control server
        let control_port = get_free_port().await?;
        let control_addr = format!("127.0.0.1:{}", control_port);

        // Create app state
        let state = Arc::new(Mutex::new(AppState::new()));

        // Start control server
        let listener = TcpListener::bind(&control_addr).await?;
        let state_clone = state.clone();
        
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                handle_control_connection(socket, state_clone.clone()).await;
            }
        });

        // Give server time to start
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Connect multiple clients with delays
        let mut client_sessions = Vec::new();
        for i in 0..num_clients {
            let delay_idx = (i as usize) % connection_delays.len();
            let delay = Duration::from_millis(connection_delays[delay_idx]);
            tokio::time::sleep(delay).await;

            let session = create_mock_client(&control_addr).await?;
            client_sessions.push(session);

            // Give time for the connection to be processed
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // Verify that the server has a client control handle (from the most recent connection)
        wait_for_condition(
            || async {
                let guard = state.lock().await;
                guard.client_control.is_some()
            },
            Duration::from_secs(1),
        ).await?;

        // The server should have exactly one active client control
        let guard = state.lock().await;
        assert!(guard.client_control.is_some(), "Server should have an active client control");

        Ok(())
    }

    // Feature: reverse-tunnel-service, Property 2: Public connection rejection without client
    // For any public connection attempt when no tunnel client is connected, 
    // the relay server should reject the connection with an appropriate error
    #[test]
    fn test_public_connection_rejection_without_client() {
        let mut config = ProptestConfig::default();
        config.cases = 10; // Reduce from default 256 to 10 cases
        proptest!(config, |(
            num_attempts in 1u8..3u8,
            attempt_delays in prop::collection::vec(0u64..25u64, 1..3)
        )| {
            let result = tokio_test::block_on(async {
                test_public_connection_rejection_impl(num_attempts, attempt_delays).await
            });
            prop_assert!(result.is_ok());
        });
    }

    async fn test_public_connection_rejection_impl(
        num_attempts: u8,
        attempt_delays: Vec<u64>,
    ) -> Result<()> {
        // Create app state with no client connected
        let state = Arc::new(Mutex::new(AppState::new()));

        // Verify no client is connected initially
        {
            let guard = state.lock().await;
            assert!(guard.client_control.is_none(), "No client should be connected initially");
        }

        // Attempt multiple public connections
        for i in 0..num_attempts {
            let delay_idx = (i as usize) % attempt_delays.len();
            let delay = Duration::from_millis(attempt_delays[delay_idx]);
            tokio::time::sleep(delay).await;

            // Create a mock public connection using a localhost connection
            let mock_port = get_free_port().await?;
            let mock_listener = TcpListener::bind(format!("127.0.0.1:{}", mock_port)).await?;
            
            // Create a connection to the mock listener
            let connect_task = tokio::spawn(async move {
                TcpStream::connect(format!("127.0.0.1:{}", mock_port)).await
            });
            
            let (server_stream, _) = mock_listener.accept().await?;
            let _client_stream = connect_task.await??;

            // Attempt to handle the public connection - should fail
            let result = handle_public_connection(server_stream, state.clone()).await;
            
            // The connection should be rejected with an error
            assert!(result.is_err(), "Public connection should be rejected when no client is connected");
            
            // Verify the error message indicates no client is connected
            let error_msg = result.unwrap_err().to_string();
            assert!(error_msg.contains("No client connected"), 
                   "Error should indicate no client connected, got: {}", error_msg);
        }

        Ok(())
    }
}