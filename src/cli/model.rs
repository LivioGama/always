//! `always model` — Settings → Models for installs without the Settings app
//! (the Linux tarball, headless machines). It sends the daemon the same
//! commands the panel does — `ListModels`, `DownloadModel`,
//! `SetActiveTranscriber` — and reads the same events back, so it can never
//! disagree with the panel about what is installed or active.

use anyhow::{Context, Result, anyhow, bail};
use clap::Subcommand;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Subcommand)]
pub enum ModelAction {
    /// List the catalogue: id, size, languages, downloaded, active
    List,
    /// Download a model, verify its checksum and wait until it is installed
    Download {
        /// Model id, as shown by `always model list`
        id: String,
    },
    /// Make a model the speech engine (`groq` and `none` are accepted too)
    Use {
        /// Model id, as shown by `always model list`
        id: String,
    },
}

pub fn handle_model(action: ModelAction) -> Result<()> {
    match action {
        ModelAction::List => {
            let mut models = None;
            exchange(
                json!({"type": "ListModels"}),
                Duration::from_secs(10),
                |ev| match ev["type"].as_str() {
                    Some("ModelsList") => {
                        models = Some(ev["data"]["models"].clone());
                        None
                    }
                    // ListModels answers with the catalogue, then the backend.
                    Some("ActiveTranscriberChanged") => models.as_ref().map(|m| {
                        let active = ev["data"]["backend"].as_str().unwrap_or("none");
                        println!("speech engine: {active}");
                        for row in model_rows(m, active) {
                            println!("{row}");
                        }
                        Ok(())
                    }),
                    _ => None,
                },
            )
        }
        ModelAction::Download { id } => {
            use std::io::IsTerminal as _;
            // A redrawn progress line is only legible on a terminal.
            let tty = std::io::stderr().is_terminal();
            let mut shown = -1;
            exchange(
                json!({"type": "DownloadModel", "data": {"model_id": id}}),
                // Verifying a multi-GB file sends no progress for a while.
                Duration::from_secs(300),
                |ev| {
                    if ev["type"] == "ModelDownloadProgress"
                        && ev["data"]["model_id"] == id.as_str()
                    {
                        let pct = ev["data"]["percentage"].as_f64().unwrap_or(0.0) as i64;
                        if tty && pct != shown {
                            shown = pct;
                            eprint!("\rdownloading {id}: {pct}%");
                        }
                    }
                    download_verdict(ev, &id)
                },
            )?;
            if shown >= 0 {
                eprintln!();
            }
            println!("{id}: installed and verified — `always model use {id}` selects it");
            Ok(())
        }
        ModelAction::Use { id } => {
            let backend = backend_for(&id);
            // Reject a malformed id here: the daemon only logs it and never answers.
            backend
                .parse::<always::stt_dispatch::TranscriberBackendChoice>()
                .map_err(|e| anyhow!("{e}"))?;
            exchange(
                json!({"type": "SetActiveTranscriber", "data": {"backend": backend}}),
                // The Models panel gives an engine load 120 s before giving up.
                Duration::from_secs(130),
                |ev| use_verdict(ev, &backend),
            )?;
            println!("speech engine: {backend}");
            Ok(())
        }
    }
}

/// Send one command, then feed every daemon event to `verdict` until it
/// returns a result. `idle` bounds the wait between two events.
fn exchange(
    cmd: Value,
    idle: Duration,
    mut verdict: impl FnMut(&Value) -> Option<Result<()>>,
) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::{BufRead as _, BufReader, Write as _};
        let sock = always::always::daemon::socket_path()
            .context("no daemon socket path on this platform")?;
        let mut stream = std::os::unix::net::UnixStream::connect(&sock).with_context(|| {
            format!(
                "cannot reach the daemon at {} — is it running? (`always start`, or \
                 `systemctl --user start always-daemon`)",
                sock.display()
            )
        })?;
        writeln!(stream, "{cmd}").context("failed to send the command to the daemon")?;
        stream.set_read_timeout(Some(idle)).ok();
        for line in BufReader::new(stream).lines() {
            let line = line.map_err(|e| {
                anyhow!(
                    "no answer from the daemon within {} s ({e})",
                    idle.as_secs()
                )
            })?;
            let Ok(ev) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(result) = verdict(&ev) {
                return result;
            }
        }
        bail!("the daemon closed the connection before answering")
    }
    #[cfg(not(unix))]
    {
        let _ = (cmd, idle, &mut verdict);
        bail!("`always model` is not available on this platform yet — use Settings → Models")
    }
}

/// `moonshine-small-streaming-en` → `local:moonshine-small-streaming-en`;
/// wire forms (`groq`, `none`, `apple`, `local:…`) pass through.
fn backend_for(id: &str) -> String {
    if id.contains(':') || matches!(id, "groq" | "none" | "apple") {
        id.to_string()
    } else {
        format!("local:{id}")
    }
}

/// Done once the catalogue shows the model installed; failed on the
/// download's own failure events or when the id is not in the catalogue.
fn download_verdict(ev: &Value, id: &str) -> Option<Result<()>> {
    let data = &ev["data"];
    let ours = data["model_id"] == id;
    match ev["type"].as_str()? {
        "ModelDownloadFailed" | "ModelExtractionFailed" if ours => Some(Err(anyhow!(
            "{id}: {}",
            data["error"].as_str().unwrap_or("download failed")
        ))),
        "ModelDownloadCancelled" if ours => Some(Err(anyhow!("{id}: download cancelled"))),
        "ModelsList" => {
            let models = data["models"].as_array()?;
            match models.iter().find(|m| m["id"] == id) {
                None => Some(Err(anyhow!(
                    "unknown model id: {id} (see `always model list`)"
                ))),
                Some(m) if m["is_downloaded"] == true => Some(Ok(())),
                Some(_) => None,
            }
        }
        _ => None,
    }
}

/// Done when the daemon names `backend` as running; failed when it reports
/// that `backend` would not load. A `Changed` naming another backend is the
/// state the daemon sends on connect, before it reads the command — not an
/// answer.
fn use_verdict(ev: &Value, backend: &str) -> Option<Result<()>> {
    let data = &ev["data"];
    if data["backend"] != backend {
        return None;
    }
    match ev["type"].as_str()? {
        "ActiveTranscriberChanged" => Some(Ok(())),
        "ActiveTranscriberFailed" => Some(Err(anyhow!(
            "{backend} did not load: {}",
            data["error"].as_str().unwrap_or("unknown error")
        ))),
        _ => None,
    }
}

fn model_rows(models: &Value, active: &str) -> Vec<String> {
    let mut rows = vec![format!(
        "  {:<34} {:>7}  {:<5} STATUS",
        "ID", "SIZE", "LANG"
    )];
    for m in models.as_array().into_iter().flatten() {
        let id = m["id"].as_str().unwrap_or("?");
        let langs = m["supported_languages"]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        let lang = match langs {
            [one] => one.as_str().unwrap_or("?").to_string(),
            many => many.len().to_string(),
        };
        let is_active = active == format!("local:{id}");
        let mut status = Vec::new();
        if is_active {
            status.push("active");
        }
        if m["is_downloaded"] == true {
            status.push("downloaded");
        } else if m["is_downloading"] == true {
            status.push("downloading");
        }
        if m["is_recommended"] == true {
            status.push("recommended");
        }
        rows.push(format!(
            "{} {:<34} {:>4} MB  {:<5} {}",
            if is_active { "*" } else { " " },
            id,
            m["size_mb"].as_u64().unwrap_or(0),
            lang,
            status.join(", ")
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: &str, data: Value) -> Value {
        json!({"type": kind, "data": data})
    }

    #[test]
    fn backend_for_prefixes_only_bare_model_ids() {
        assert_eq!(
            backend_for("moonshine-small-streaming-en"),
            "local:moonshine-small-streaming-en"
        );
        assert_eq!(backend_for("groq"), "groq");
        assert_eq!(backend_for("local:x"), "local:x");
    }

    #[test]
    fn download_waits_for_the_installed_catalogue_and_fails_precisely() {
        let list = |downloaded| {
            ev(
                "ModelsList",
                json!({"models": [{"id": "m", "is_downloaded": downloaded}]}),
            )
        };
        assert!(download_verdict(&list(false), "m").is_none());
        assert!(download_verdict(&list(true), "m").unwrap().is_ok());
        assert!(download_verdict(&list(true), "other").unwrap().is_err());
        let failed = ev(
            "ModelDownloadFailed",
            json!({"model_id": "m", "error": "sha256 mismatch"}),
        );
        let err = download_verdict(&failed, "m")
            .unwrap()
            .unwrap_err()
            .to_string();
        assert!(err.contains("sha256 mismatch"), "{err}");
        // Another model's failure is not ours.
        assert!(download_verdict(&failed, "n").is_none());
        let progress = ev(
            "ModelDownloadProgress",
            json!({"model_id": "m", "percentage": 40.0}),
        );
        assert!(download_verdict(&progress, "m").is_none());
    }

    #[test]
    fn use_ignores_the_connect_time_backend_and_reports_load_failures() {
        let want = "local:m";
        let changed = |b: &str| ev("ActiveTranscriberChanged", json!({"backend": b}));
        assert!(use_verdict(&changed("groq"), want).is_none());
        assert!(use_verdict(&changed(want), want).unwrap().is_ok());
        let failed = ev(
            "ActiveTranscriberFailed",
            json!({"backend": want, "error": "missing files"}),
        );
        let err = use_verdict(&failed, want).unwrap().unwrap_err().to_string();
        assert!(err.contains("missing files"), "{err}");
    }

    #[test]
    fn list_marks_the_active_model() {
        let models = json!([
            {"id": "a", "size_mb": 99, "supported_languages": ["en"], "is_downloaded": true},
            {"id": "b", "size_mb": 2474, "supported_languages": ["en", "de"], "is_recommended": true},
        ]);
        let rows = model_rows(&models, "local:a");
        assert!(rows[1].starts_with("* a"), "{}", rows[1]);
        assert!(rows[1].ends_with("active, downloaded"), "{}", rows[1]);
        assert!(rows[2].contains("2474 MB  2"), "{}", rows[2]);
        assert!(rows[2].ends_with("recommended"), "{}", rows[2]);
    }
}
