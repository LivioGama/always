//! Fresh install, temp HOME: the first pasted utterance must land where the
//! GUI's History tab reads it (`~/Library/Application Support/always/history.jsonl`
//! on macOS, i.e. `config_dir()/history.jsonl`), with nothing pre-created and
//! transcript logging off (the release default).

#[test]
fn first_paste_on_fresh_home_is_readable_history() {
    let home = std::env::temp_dir().join(format!("always-fresh-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    // This test binary owns its env; nothing else runs in it concurrently.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::remove_var("ALWAYS_CONFIG_DIR");
        std::env::remove_var("ALWAYS_HISTORY_PATH");
        std::env::set_var("ALWAYS_LOG_TRANSCRIPTS", "0");
    }

    let path = always::always::history::history_path();
    assert!(
        path.starts_with(&home),
        "history must live under HOME: {}",
        path.display()
    );
    #[cfg(target_os = "macos")]
    assert!(
        path.ends_with("Library/Application Support/always/history.jsonl"),
        "GUI reads AppInstance.supportDir/history.jsonl, got {}",
        path.display()
    );
    assert!(!path.exists());

    always::always::history::append("bonjour", "Bonjour.");

    let content = std::fs::read_to_string(&path).expect("history written on first paste");
    let v: serde_json::Value = serde_json::from_str(content.lines().next().unwrap()).unwrap();
    assert_eq!(v["raw_text"], "bonjour");
    assert_eq!(v["processed_text"], "Bonjour.");
    assert!(v["ts"].as_u64().unwrap() > 0);
    let _ = std::fs::remove_dir_all(&home);
}
