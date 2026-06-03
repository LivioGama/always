//! Wire-format contract tests for the UDS event/command protocol.
//!
//! These tests pin the JSON shape that the Mac app (`Always/Sources/.../UDSClient.swift`)
//! decodes. Breaking either side without bumping `PROTOCOL_VERSION`
//! silently desynchronizes the daemon and the GUI, which is the kind of
//! bug that only surfaces in user reports days later.

use always::always::event::{DaemonCommand, DaemonEvent, PROTOCOL_VERSION};

#[test]
fn protocol_version_is_one() {
    assert_eq!(
        PROTOCOL_VERSION, 1,
        "protocol version bumped without updating UDSClient.swift?"
    );
}

#[test]
fn hello_serializes_with_version_in_data() {
    let hello = DaemonEvent::Hello {
        version: PROTOCOL_VERSION,
    };
    let json = serde_json::to_string(&hello).unwrap();
    assert_eq!(json, r#"{"type":"Hello","data":{"version":1}}"#);
}

#[test]
fn listening_started_serializes_without_data() {
    let json = serde_json::to_string(&DaemonEvent::ListeningStarted).unwrap();
    assert_eq!(json, r#"{"type":"ListeningStarted"}"#);
}

#[test]
fn transcript_final_serializes_with_text_payload() {
    let ev = DaemonEvent::TranscriptFinal {
        text: "git status".to_string(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert_eq!(
        json,
        r#"{"type":"TranscriptFinal","data":{"text":"git status"}}"#
    );
}

#[test]
fn paused_resumed_round_trip() {
    let cases = [
        (DaemonEvent::Paused, r#"{"type":"Paused"}"#),
        (DaemonEvent::Resumed, r#"{"type":"Resumed"}"#),
        (
            DaemonEvent::AutoEnterEnabled,
            r#"{"type":"AutoEnterEnabled"}"#,
        ),
        (
            DaemonEvent::AutoEnterDisabled,
            r#"{"type":"AutoEnterDisabled"}"#,
        ),
        (DaemonEvent::Heartbeat, r#"{"type":"Heartbeat"}"#),
    ];
    for (event, expected_json) in cases {
        let json = serde_json::to_string(&event).expect("serialize");
        assert_eq!(json, expected_json, "wire format drift for {event:?}");
    }
}

#[test]
fn json_line_terminates_with_newline() {
    let line = DaemonEvent::ListeningStarted.to_json_line().unwrap();
    assert!(line.ends_with('\n'));
    assert!(!line[..line.len() - 1].contains('\n'));
}

#[test]
fn command_round_trip() {
    let json = r#"{"type":"TogglePause"}"#;
    let cmd = DaemonCommand::from_json_line(json).unwrap();
    assert!(matches!(cmd, DaemonCommand::TogglePause));

    let json2 = r#"{"type":"ToggleAutoEnter"}"#;
    let cmd2 = DaemonCommand::from_json_line(json2).unwrap();
    assert!(matches!(cmd2, DaemonCommand::ToggleAutoEnter));
}

#[test]
fn unknown_command_rejected() {
    let json = r#"{"type":"FormatHardDrive"}"#;
    assert!(DaemonCommand::from_json_line(json).is_err());
}
