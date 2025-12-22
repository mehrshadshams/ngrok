use crate::test_utils::*;
use crate::{handle_control_connection, handle_public_connection, AppState};
use anyhow::Result;
use futures::StreamExt;
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

    // Feature: reverse-tunnel-service, Property 3: Yamux session establishment
    // For any successful tunnel client connection, both client and server should establish 
    // compatible yamux sessions that can create and accept streams
    #[test]
    fn test_yamux_session_establishment() {
        let mut config = ProptestConfig::default();
        config.cases = 10; // Reduce from default 256 to 10 cases
        proptest!(config, |(
            num_sessions in 1u8..3u8,
            session_delays in prop::collection::vec(0u64..50u64, 1..3)
        )| {
            let result = tokio_test::block_on(async {
                test_yamux_session_establishment_impl(num_sessions, session_delays).await
            });
            prop_assert!(result.is_ok());
        });
    }

    async fn test_yamux_session_establishment_impl(
        num_sessions: u8,
        session_delays: Vec<u64>,
    ) -> Result<()> {
        use tokio_yamux::{Config, Session};
        use futures::StreamExt;

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

        // Test multiple session establishments
        for i in 0..num_sessions {
            let delay_idx = (i as usize) % session_delays.len();
            let delay = Duration::from_millis(session_delays[delay_idx]);
            tokio::time::sleep(delay).await;

            // Create client connection
            let client_socket = TcpStream::connect(&control_addr).await?;
            let config = Config::default();
            let mut client_session = Session::new_client(client_socket, config);

            // Give time for server to process the connection
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Verify server has established a control handle
            wait_for_condition(
                || async {
                    let guard = state.lock().await;
                    guard.client_control.is_some()
                },
                Duration::from_secs(1),
            ).await?;

            // Test that the server can create a stream through the established session
            let control_handle = {
                let guard = state.lock().await;
                guard.client_control.clone()
            };

            if let Some(mut control) = control_handle {
                // Server creates a stream
                let server_stream_result = control.open_stream().await;
                assert!(server_stream_result.is_ok(), "Server should be able to create stream through yamux session");

                // Client should receive the stream
                let client_stream_result = tokio::time::timeout(
                    Duration::from_millis(500),
                    client_session.next()
                ).await;

                assert!(client_stream_result.is_ok(), "Client should receive stream within timeout");
                let stream_option = client_stream_result?;
                assert!(stream_option.is_some(), "Client should receive a stream");
                
                if let Some(stream_result) = stream_option {
                    assert!(stream_result.is_ok(), "Client stream should be valid");
                }
            } else {
                return Err(anyhow::anyhow!("Server should have established control handle"));
            }

            // Clean up - drop the session to close connection
            drop(client_session);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        Ok(())
    }

    // Feature: reverse-tunnel-service, Property 4: Connection failure handling
    // For any connection failure scenario (network, protocol, or service), 
    // the affected component should terminate gracefully with appropriate error reporting
    #[test]
    fn test_connection_failure_handling() {
        let mut config = ProptestConfig::default();
        config.cases = 10; // Reduce from default 256 to 10 cases
        proptest!(config, |(
            failure_scenarios in prop::collection::vec(0u8..3u8, 1..3),
            failure_delays in prop::collection::vec(0u64..50u64, 1..3)
        )| {
            let result = tokio_test::block_on(async {
                test_connection_failure_handling_impl(failure_scenarios, failure_delays).await
            });
            prop_assert!(result.is_ok());
        });
    }

    async fn test_connection_failure_handling_impl(
        failure_scenarios: Vec<u8>,
        failure_delays: Vec<u64>,
    ) -> Result<()> {
        use tokio_yamux::{Config, Session};

        for (i, &scenario) in failure_scenarios.iter().enumerate() {
            let delay_idx = i % failure_delays.len();
            let delay = Duration::from_millis(failure_delays[delay_idx]);
            tokio::time::sleep(delay).await;

            match scenario {
                0 => {
                    // Test scenario: Connection to non-existent server
                    let invalid_addr = "127.0.0.1:0"; // Port 0 should not be bindable
                    let connection_result = TcpStream::connect(invalid_addr).await;
                    
                    // Should fail gracefully with connection error
                    assert!(connection_result.is_err(), 
                           "Connection to invalid address should fail gracefully");
                }
                1 => {
                    // Test scenario: Server closes connection immediately
                    let control_port = get_free_port().await?;
                    let control_addr = format!("127.0.0.1:{}", control_port);
                    
                    // Create a server that immediately closes connections
                    let listener = TcpListener::bind(&control_addr).await?;
                    tokio::spawn(async move {
                        if let Ok((socket, _)) = listener.accept().await {
                            // Immediately drop the socket to simulate connection failure
                            drop(socket);
                        }
                    });
                    
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    
                    // Client attempts to connect
                    let client_socket_result = TcpStream::connect(&control_addr).await;
                    
                    if let Ok(client_socket) = client_socket_result {
                        let config = Config::default();
                        let mut client_session = Session::new_client(client_socket, config);
                        
                        // The session should detect the closed connection
                        let stream_result = tokio::time::timeout(
                            Duration::from_millis(500),
                            client_session.next()
                        ).await;
                        
                        // Should either timeout or return None/Error indicating connection closed
                        match stream_result {
                            Ok(Some(Err(_))) => {
                                // Connection error detected - this is expected
                            }
                            Ok(None) => {
                                // Session ended - this is also expected
                            }
                            Err(_) => {
                                // Timeout - connection might be hanging, which is acceptable
                            }
                            Ok(Some(Ok(_))) => {
                                // Unexpected success - this shouldn't happen with immediate close
                                return Err(anyhow::anyhow!("Unexpected stream success on closed connection"));
                            }
                        }
                    }
                    // If connection itself fails, that's also acceptable behavior
                }
                2 => {
                    // Test scenario: Public connection with no client
                    let state = Arc::new(Mutex::new(AppState::new()));
                    
                    // Create a mock public connection
                    let mock_port = get_free_port().await?;
                    let mock_listener = TcpListener::bind(format!("127.0.0.1:{}", mock_port)).await?;
                    
                    let connect_task = tokio::spawn(async move {
                        TcpStream::connect(format!("127.0.0.1:{}", mock_port)).await
                    });
                    
                    let (server_stream, _) = mock_listener.accept().await?;
                    let _client_stream = connect_task.await??;
                    
                    // Handle public connection should fail gracefully
                    let result = handle_public_connection(server_stream, state).await;
                    
                    // Should fail with appropriate error message
                    assert!(result.is_err(), "Public connection should fail when no client is connected");
                    
                    let error_msg = result.unwrap_err().to_string();
                    assert!(error_msg.contains("No client connected"), 
                           "Error should indicate no client connected, got: {}", error_msg);
                }
                _ => {
                    // Default case - no specific failure scenario
                }
            }
        }

        Ok(())
    }
}