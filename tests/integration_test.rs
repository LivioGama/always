// Integration tests for the Always daemon
// These tests verify the daemon lifecycle, UDS communication, and end-to-end transcription flow

use std::time::Duration;
use tokio::net::UnixStream;
use tokio::time::sleep;

#[tokio::test]
#[ignore = "requires daemon binary on PATH; run with `cargo test -- --ignored`"]
async fn test_daemon_uds_connection() {
    use serde_json::json;

    let socket_path = always::always::uds_server::socket_path().unwrap();

    // Wait a bit for daemon to start
    sleep(Duration::from_millis(500)).await;

    // Connect to UDS socket
    let stream: UnixStream = UnixStream::connect(&socket_path)
        .await
        .expect("Failed to connect to UDS socket");

    // Send a TogglePause command
    let command = json!({"type": "TogglePause"});
    let command_str = serde_json::to_string(&command).unwrap() + "\n";

    let (mut reader, mut writer) = stream.into_split();
    tokio::io::AsyncWriteExt::write_all(&mut writer, command_str.as_bytes())
        .await
        .expect("Failed to send command");

    // Read response (should get a Paused event)
    let mut buffer = vec![0u8; 1024];
    let n = tokio::io::AsyncReadExt::read(&mut reader, &mut buffer)
        .await
        .expect("Failed to read response");

    let response = String::from_utf8_lossy(&buffer[..n]);
    assert!(
        response.contains("Paused"),
        "Expected Paused event in response"
    );

    println!("UDS connection test passed");
}

#[tokio::test]
#[ignore = "requires daemon binary on PATH; run with `cargo test -- --ignored`"]
async fn test_daemon_lifecycle() {
    use std::process::Command;

    // Stop any existing daemon
    let _ = Command::new("always").arg("stop").output();

    // Start daemon
    let output = Command::new("always")
        .arg("start")
        .output()
        .expect("Failed to start daemon");

    assert!(output.status.success(), "Daemon start command failed");

    // Wait for daemon to initialize
    sleep(Duration::from_secs(2)).await;

    // Check daemon status
    let output = Command::new("always")
        .arg("status")
        .output()
        .expect("Failed to get daemon status");

    let status_output = String::from_utf8_lossy(&output.stdout);
    assert!(
        status_output.contains("Running"),
        "Daemon should be running"
    );

    // Stop daemon
    let output = Command::new("always")
        .arg("stop")
        .output()
        .expect("Failed to stop daemon");

    assert!(output.status.success(), "Daemon stop command failed");

    println!("Daemon lifecycle test passed");
}

#[tokio::test]
#[ignore = "requires daemon binary on PATH; run with `cargo test -- --ignored`"]
async fn test_config_commands() {
    use std::process::Command;

    // Test setting a config value
    let output = Command::new("always")
        .args(["config", "set", "energy_threshold", "0.5"])
        .output()
        .expect("Failed to set config");

    assert!(output.status.success(), "Config set command failed");

    // Test getting config value
    let output = Command::new("always")
        .args(["config", "show"])
        .output()
        .expect("Failed to get config");

    let config_output = String::from_utf8_lossy(&output.stdout);
    assert!(
        config_output.contains("energy_threshold"),
        "Config should contain energy_threshold"
    );

    println!("Config commands test passed");
}

#[tokio::test]
#[ignore] // Requires API key and daemon to be running
async fn test_toggle_pause() {
    use std::process::Command;

    // Start daemon if not running
    let _ = Command::new("always").arg("start").output();

    sleep(Duration::from_secs(2)).await;

    // Toggle pause
    let output = Command::new("always")
        .args(["toggle", "pause"])
        .output()
        .expect("Failed to toggle pause");

    assert!(output.status.success(), "Toggle pause command failed");

    // Toggle auto-enter
    let output = Command::new("always")
        .args(["toggle", "auto-enter"])
        .output()
        .expect("Failed to toggle auto-enter");

    assert!(output.status.success(), "Toggle auto-enter command failed");

    println!("Toggle commands test passed");
}

#[test]
fn test_socket_path_resolution() {
    let socket_path = always::always::uds_server::socket_path().unwrap();

    // On macOS, should use ~/Library/Caches/Always/always.sock
    // On Linux, should use XDG_RUNTIME_DIR or /tmp
    #[cfg(target_os = "macos")]
    {
        assert!(
            socket_path
                .to_string_lossy()
                .contains("Library/Caches/Always")
        );
    }

    #[cfg(not(target_os = "macos"))]
    {
        // Either XDG_RUNTIME_DIR or /tmp
        let path_str = socket_path.to_string_lossy();
        assert!(
            path_str.contains("runtime-dir") || path_str.contains("/tmp"),
            "Socket path should use runtime directory or /tmp"
        );
    }

    println!("Socket path resolution test passed");
}
