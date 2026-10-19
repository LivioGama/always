//! Wire-format contract tests for the UDS event/command protocol.
//!
//! These tests pin the JSON shape that the Mac app (`Always/Sources/.../UDSClient.swift`)
//! decodes. Breaking either side without bumping `PROTOCOL_VERSION`
//! silently desynchronizes the daemon and the GUI, which is the kind of
//! bug that only surfaces in user reports days later.

use always::always::event::{BUILD_ID, DaemonCommand, DaemonEvent, PROTOCOL_VERSION};

#[test]
fn protocol_version_matches_swift_client() {
    // Pinned to whatever value the Swift `UDS_PROTOCOL_VERSION` constant
    // in `Always/Sources/Always/Services/UDSClient.swift` holds. Bumping
    // one without the other will fail both this test and the Swift
    // `testProtocolVersionMatchesDaemon` mirror.
    assert_eq!(
        PROTOCOL_VERSION, 12,
        "protocol version bumped without updating UDSClient.swift?"
    );
}

#[test]
fn hello_serializes_with_version_in_data() {
    let hello = DaemonEvent::Hello {
        version: PROTOCOL_VERSION,
        build_id: Some(BUILD_ID.to_string()),
    };
    let json = serde_json::to_string(&hello).unwrap();
    // Format against the constant so future bumps don't desynchronize
    // the assertion from the declared version.
    assert_eq!(
        json,
        format!(
            r#"{{"type":"Hello","data":{{"version":{PROTOCOL_VERSION},"build_id":"{BUILD_ID}"}}}}"#
        )
    );
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
fn transcription_interim_serializes_with_text_payload() {
    let ev = DaemonEvent::TranscriptionInterim {
        text: "hello".to_string(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert_eq!(
        json,
        r#"{"type":"TranscriptionInterim","data":{"text":"hello"}}"#
    );
}

#[test]
fn stt_fallback_engaged_serializes_with_model_payload() {
    // The Swift client decodes this through the loose `[String:String]`
    // data dict, so the payload must be a flat string map.
    let ev = DaemonEvent::SttFallbackEngaged {
        model: "parakeet-tdt-0.6b-v2".to_string(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert_eq!(
        json,
        r#"{"type":"SttFallbackEngaged","data":{"model":"parakeet-tdt-0.6b-v2"}}"#
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

    let json3 = r#"{"type":"SetAutoEnter","data":{"enabled":true}}"#;
    let cmd3 = DaemonCommand::from_json_line(json3).unwrap();
    assert!(matches!(
        cmd3,
        DaemonCommand::SetAutoEnter { enabled: true }
    ));

    let json4 = r#"{"type":"ApplyRuntimePreferences","data":{"auto_enter_delay_ms":2000,"energy_threshold":0.012,"silence_secs":2.0,"cooldown_ms":150,"silero_threshold":0.5}}"#;
    let cmd4 = DaemonCommand::from_json_line(json4).unwrap();
    assert!(matches!(
        cmd4,
        DaemonCommand::ApplyRuntimePreferences {
            auto_enter_delay_ms: 2000,
            ..
        }
    ));

    // RespawnRecorder is a unit variant — no `data` field, like TogglePause.
    let json5 = r#"{"type":"RespawnRecorder"}"#;
    let cmd5 = DaemonCommand::from_json_line(json5).unwrap();
    assert!(matches!(cmd5, DaemonCommand::RespawnRecorder));
}

#[test]
fn unknown_command_rejected() {
    let json = r#"{"type":"FormatHardDrive"}"#;
    assert!(DaemonCommand::from_json_line(json).is_err());
}

// ── Updates (SPEC §12a) ─────────────────────────────────────────────────────
//
// Every field is a string so the Mac app's loose `[String: String]` payload
// decode accepts these frames. PROTOCOL_VERSION is NOT bumped: the variants
// are additive and an older client ignores events it does not know.

#[test]
fn update_available_serializes_all_string_fields() {
    let ev = DaemonEvent::UpdateAvailable {
        current: "0.1.0".to_string(),
        latest: "0.2.0".to_string(),
        url: "https://github.com/LivioGama/always/releases/tag/v0.2.0".to_string(),
        can_apply: "true".to_string(),
        apply_hint: String::new(),
    };
    assert_eq!(
        serde_json::to_string(&ev).unwrap(),
        r#"{"type":"UpdateAvailable","data":{"current":"0.1.0","latest":"0.2.0","url":"https://github.com/LivioGama/always/releases/tag/v0.2.0","can_apply":"true","apply_hint":""}}"#
    );
    // Round trip: the daemon's own parser accepts what it emits.
    let line = ev.to_json_line().unwrap();
    assert!(matches!(
        DaemonEvent::from_json_line(&line).unwrap(),
        DaemonEvent::UpdateAvailable { .. }
    ));
}

#[test]
fn update_check_done_progress_and_failed_serialize_flat_strings() {
    let done = DaemonEvent::UpdateCheckDone {
        result: "up_to_date".to_string(),
        current: "0.1.0".to_string(),
        latest: "0.1.0".to_string(),
    };
    assert_eq!(
        serde_json::to_string(&done).unwrap(),
        r#"{"type":"UpdateCheckDone","data":{"result":"up_to_date","current":"0.1.0","latest":"0.1.0"}}"#
    );

    let progress = DaemonEvent::UpdateProgress {
        phase: "downloading".to_string(),
        percent: "42".to_string(),
        detail: "always-v0.2.0-aarch64-apple-darwin.tar.gz".to_string(),
    };
    assert_eq!(
        serde_json::to_string(&progress).unwrap(),
        r#"{"type":"UpdateProgress","data":{"phase":"downloading","percent":"42","detail":"always-v0.2.0-aarch64-apple-darwin.tar.gz"}}"#
    );

    let failed = DaemonEvent::UpdateFailed {
        stage: "verify".to_string(),
        message: "signature did not verify".to_string(),
    };
    assert_eq!(
        serde_json::to_string(&failed).unwrap(),
        r#"{"type":"UpdateFailed","data":{"stage":"verify","message":"signature did not verify"}}"#
    );
}

#[test]
fn update_commands_are_unit_variants() {
    assert!(matches!(
        DaemonCommand::from_json_line(r#"{"type":"CheckForUpdates"}"#).unwrap(),
        DaemonCommand::CheckForUpdates
    ));
    assert!(matches!(
        DaemonCommand::from_json_line(r#"{"type":"InstallUpdate"}"#).unwrap(),
        DaemonCommand::InstallUpdate
    ));
}
