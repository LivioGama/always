//! LIVE voice-edit test against a real app (TextEdit) — real AX, real
//! keystrokes, real grammar + gate + planner + executor (`voice_edit::run`).
//! Keystrokes are posted to the target pid only, never to the focused app.
//!
//! ```text
//! HOME=<scratch> VOICE_EDIT_LIVE_PID=<TextEdit pid> \
//!   cargo test --lib voice_edit_live -- --ignored --nocapture --test-threads=1
//! ```
//! `HOME=<scratch>` keeps the correction-learning glossary write and the
//! shadow dataset out of the user's real files.

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::always::ax_edit::{self, KeyTarget, Stroke, len_u16};
    use crate::always::voice_edit::{self, Outcome};

    /// Only ever touch OUR document: other agents use TextEdit too.
    fn elem(pid: i32) -> ax_edit::AxElem {
        let e = ax_edit::focused_element_of(pid).expect("TextEdit focused text view");
        let want =
            std::env::var("VOICE_EDIT_LIVE_TITLE").unwrap_or_else(|_| "voice-edit-live.txt".into());
        let title = e.window_title().unwrap_or_default();
        assert!(
            want == "*" || title.starts_with(&want),
            "focused TextEdit window is {title:?}, not {want:?} — aborting"
        );
        e
    }

    fn value(pid: i32) -> String {
        elem(pid).value().unwrap_or_default()
    }

    fn wait_until(pid: i32, f: impl Fn(&str) -> bool) -> (bool, f64) {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(1500) {
            if f(&value(pid)) {
                return (true, t0.elapsed().as_secs_f64() * 1000.0);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        (false, t0.elapsed().as_secs_f64() * 1000.0)
    }

    fn clear(pid: i32) {
        let e = elem(pid);
        let n = len_u16(&e.value().unwrap_or_default());
        e.set_selected_range(0, n);
        e.set_selected_text("");
        assert_eq!(value(pid), "");
    }

    /// Stand-in for Always's ⌘V paste: type the text into the pid, then
    /// record the anchor exactly as `deliver_paste` does.
    fn dictate(pid: i32, text: &str) {
        let before = value(pid);
        // Exactly Always's paste: pasteboard + ⌘V (its own undo step, no
        // auto-capitalisation). The caller restores the user's clipboard.
        crate::always::paste::copy_to_clipboard(text.to_string()).unwrap();
        ax_edit::post_strokes(
            KeyTarget::Pid(pid),
            &[Stroke::Key {
                code: 9,
                mods: ax_edit::Mods::CMD,
                repeat: 1,
            }],
        )
        .unwrap();
        let want = format!("{before}{text}");
        let (ok, _) = wait_until(pid, |v| v == want);
        assert!(ok, "dictation did not land: {:?}", value(pid));
        voice_edit::note_paste_in(text, Some(pid), true);
    }

    fn say(pid: i32, utterance: &str) -> Outcome {
        let o = voice_edit::run(
            utterance,
            false,
            "live",
            KeyTarget::Pid(pid),
            Some(pid),
            &[],
        );
        eprintln!(
            "  {utterance:<34} claimed={} op={} method={} reason={} run_ms={:.1}",
            o.claimed, o.op, o.method, o.reason, o.ms
        );
        o
    }

    /// Run a command, then wait for the field to reach `want`; returns the
    /// end-to-end ms (command start → field shows the result).
    fn expect(pid: i32, utterance: &str, want: &str, timings: &mut Vec<(String, f64)>) {
        let t0 = Instant::now();
        let o = say(pid, utterance);
        assert!(o.claimed, "{utterance} not claimed: {}", o.reason);
        let (ok, _) = wait_until(pid, |v| v == want);
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert!(ok, "{utterance}: field is {:?}, want {want:?}", value(pid));
        eprintln!("    → field ok in {ms:.1} ms: {want:?}");
        timings.push((utterance.to_string(), ms));
    }

    #[test]
    #[ignore = "drives a live TextEdit window; needs VOICE_EDIT_LIVE_PID"]
    fn voice_edit_live() {
        let Ok(pid) = std::env::var("VOICE_EDIT_LIVE_PID") else {
            return;
        };
        let pid: i32 = pid.parse().unwrap();
        let mut t = Vec::new();
        let saved_clipboard = crate::always::paste::read_clipboard_text().unwrap_or_default();
        clear(pid);

        eprintln!("scratch that");
        dictate(pid, "Intro line. ");
        dictate(pid, "Hello world. ");
        expect(pid, "Scratch that.", "Intro line. ", &mut t);
        expect(pid, "Scratch that.", "", &mut t);

        eprintln!("replace + single undo + redo + correction learning");
        dictate(pid, "I use cloud code every day. ");
        expect(
            pid,
            "Change cloud code to Claude Code.",
            "I use Claude Code every day. ",
            &mut t,
        );
        if std::env::var("VOICE_EDIT_LIVE_SKIP_UNDO").is_ok() {
            eprintln!("  (Undo/Redo keystroke steps skipped by env)");
        } else {
            expect(pid, "Undo.", "I use cloud code every day. ", &mut t);
            expect(pid, "Redo.", "I use Claude Code every day. ", &mut t);
        }
        expect(
            pid,
            "Replace every with each",
            "I use Claude Code each day. ",
            &mut t,
        );

        eprintln!("case / list");
        clear(pid);
        dictate(pid, "the quick brown fox. ");
        expect(pid, "Capitalise that.", "The Quick Brown Fox. ", &mut t);
        clear(pid);
        dictate(pid, "Buy milk. Call mom. Ship the build. ");
        expect(
            pid,
            "Make that a list.",
            "- Buy milk\n- Call mom\n- Ship the build ",
            &mut t,
        );

        eprintln!("new line / new paragraph / navigation / selection / deletes");
        clear(pid);
        dictate(pid, "First sentence here. Second one there. ");
        expect(
            pid,
            "New line.",
            "First sentence here. Second one there. \n",
            &mut t,
        );
        expect(
            pid,
            "New paragraph.",
            "First sentence here. Second one there. \n\n\n",
            &mut t,
        );
        clear(pid);
        dictate(pid, "First sentence here. Second one there. ");
        let o = say(pid, "Select the last sentence.");
        assert!(o.claimed);
        assert_eq!(
            elem(pid).selected_text().as_deref(),
            Some("Second one there.")
        );
        let o = say(pid, "Go to the end.");
        assert!(o.claimed);
        let e = elem(pid);
        assert_eq!(e.selected_range(), Some((len_u16(&value(pid)), 0)));
        expect(
            pid,
            "Delete the last word.",
            "First sentence here. Second one ",
            &mut t,
        );
        expect(
            pid,
            "Delete the last sentence.",
            "First sentence here. ",
            &mut t,
        );
        let o = say(pid, "Select the last word.");
        assert!(o.claimed);
        assert_eq!(elem(pid).selected_text().as_deref(), Some("here."));
        let o = say(pid, "Go to the start.");
        assert!(o.claimed);
        assert_eq!(elem(pid).selected_range(), Some((0, 0)));
        let o = say(pid, "Go to the end.");
        assert!(o.claimed);

        eprintln!("dictation is never claimed");
        let before = value(pid);
        for u in [
            "Scratch that, I meant the other file.",
            "Can you change the colour to blue?",
            "Change dog to cat.", // not in the last paste
            "Undo the last commit.",
        ] {
            let o = say(pid, u);
            assert!(!o.claimed, "{u} was claimed");
        }
        assert_eq!(value(pid), before);

        eprintln!("keystroke fallback (field treated as non-AX)");
        clear(pid);
        dictate(pid, "typed into a terminal ");
        let planned = voice_edit::plan(
            &crate::always::voice_edit_grammar::parse("scratch that").unwrap(),
            "scratch that",
            Some("typed into a terminal "),
            &voice_edit::Field {
                terminal: true,
                ..Default::default()
            },
            true,
            false,
        )
        .unwrap();
        let t0 = Instant::now();
        let m = voice_edit::execute(&planned, None, KeyTarget::Pid(pid)).unwrap();
        let (ok, _) = wait_until(pid, |v| v.is_empty());
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert!(ok, "keys fallback left {:?}", value(pid));
        eprintln!("    → keys scratch ({m}) ok in {ms:.1} ms");
        t.push(("scratch that (keys fallback)".into(), ms));

        eprintln!("Iris primitives: get target + single-edit replace + undo");
        clear(pid);
        dictate(
            pid,
            "this is a long rambling sentence that should be shorter. ",
        );
        let target = voice_edit::current_target(Some(pid));
        assert_eq!(target.source, "last_paste");
        assert_eq!(
            target.text,
            "this is a long rambling sentence that should be shorter."
        );
        let t0 = Instant::now();
        voice_edit::replace_target(
            &target.text,
            "This should be shorter.",
            Some(pid),
            KeyTarget::Pid(pid),
        )
        .unwrap();
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(value(pid), "This should be shorter. ");
        t.push(("replace_target (rewrite commit)".into(), ms));
        // ⌘Z restores the original in one step.
        ax_edit::post_strokes(
            KeyTarget::Pid(pid),
            &[Stroke::Key {
                code: ax_edit::keys::Z,
                mods: ax_edit::Mods::CMD,
                repeat: 1,
            }],
        )
        .unwrap();
        let (ok, _) = wait_until(pid, |v| {
            v == "this is a long rambling sentence that should be shorter. "
        });
        assert!(ok, "single ⌘Z did not restore: {:?}", value(pid));
        // Selection target: select a word, replace it.
        let e = elem(pid);
        e.set_selected_range(10, 4); // "long"
        let target = voice_edit::current_target(Some(pid));
        assert_eq!((target.source, target.text.as_str()), ("selection", "long"));
        voice_edit::replace_target("long", "short", Some(pid), KeyTarget::Pid(pid)).unwrap();
        assert_eq!(
            value(pid),
            "this is a short rambling sentence that should be shorter. "
        );
        // Stale expectation is refused.
        elem(pid).set_selected_range(0, 4); // "this"
        assert_eq!(
            voice_edit::replace_target("long", "x", Some(pid), KeyTarget::Pid(pid)).err(),
            Some("target_changed")
        );

        clear(pid);
        let _ = crate::always::paste::copy_to_clipboard(saved_clipboard);
        eprintln!("\nTIMINGS (command start → field shows result)");
        for (u, ms) in &t {
            eprintln!("  {ms:>7.1} ms  {u}");
        }
        let max = t.iter().map(|x| x.1).fold(0.0, f64::max);
        eprintln!("  max {max:.1} ms");
    }
}

/// LIVE rewrite: target from the field → Iris's exact on-device prompt (a
/// node runner, `VOICE_EDIT_REWRITE_CMD`, prints the rewrite) → ONE AX
/// replacement → a single ⌘Z restores the original.
#[cfg(test)]
mod rewrite_live {
    use std::time::{Duration, Instant};

    use crate::always::ax_edit::{self, KeyTarget, Mods, Stroke};
    use crate::always::voice_edit;

    /// Edit ▸ Undo through Accessibility — the action ⌘Z triggers, invoked
    /// without depending on key-equivalent delivery to a background app.
    fn undo(pid: i32) {
        let st = std::process::Command::new("osascript")
            .arg("-e")
            .arg(format!(
                "tell application \"System Events\" to tell (first process whose unix id is {pid}) to click menu item 1 of menu \"Edit\" of menu bar 1"
            ))
            .output()
            .unwrap();
        assert!(
            st.status.success(),
            "{}",
            String::from_utf8_lossy(&st.stderr)
        );
    }

    #[test]
    #[ignore = "drives a live TextEdit window + Apple FM; needs VOICE_EDIT_LIVE_PID + VOICE_EDIT_REWRITE_CMD"]
    fn voice_edit_live_rewrite() {
        let (Ok(pid), Ok(cmd)) = (
            std::env::var("VOICE_EDIT_LIVE_PID"),
            std::env::var("VOICE_EDIT_REWRITE_CMD"),
        ) else {
            return;
        };
        let pid: i32 = pid.parse().unwrap();
        let want = std::env::var("VOICE_EDIT_LIVE_TITLE").unwrap_or_default();
        let e = ax_edit::focused_element_of(pid).unwrap();
        assert!(
            want == "*" || e.window_title().unwrap_or_default().starts_with(&want),
            "wrong window"
        );
        let n = ax_edit::len_u16(&e.value().unwrap_or_default());
        e.set_selected_range(0, n);
        e.set_selected_text("");
        let original = "so basically i was thinking that maybe we could possibly move the meeting to thursday if thats ok with everyone because tuesday doesnt really work for me ";
        // Insert like a paste (typed keys would trigger TextEdit's
        // auto-capitalisation, which a ⌘V paste does not).
        e.set_selected_text(original);
        // Separate undo group from the setup edits (a real ⌘V paste is its
        // own user event; AX edits in one burst can coalesce).
        // A real ⌘V paste is its own undo step; AX insertions coalesce with
        // the setup edits like typing does. A caret move (→ at the end, a
        // no-op) closes the group the way the paste would.
        ax_edit::post_strokes(
            KeyTarget::Pid(pid),
            &[Stroke::Key {
                code: 124,
                mods: Mods::NONE,
                repeat: 1,
            }],
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let t = Instant::now();
        while e.value().as_deref() != Some(original) && t.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        voice_edit::note_paste_in(original, Some(pid), true);
        for style in ["formal", "shorter", "translate:nepali"] {
            let t0 = Instant::now();
            let target = voice_edit::current_target(Some(pid));
            assert_eq!(
                (target.source, target.text.as_str()),
                ("last_paste", original.trim())
            );
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("{cmd} {style} \"$0\"",))
                .arg(&target.text)
                .output()
                .unwrap();
            let rewritten = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let fm = String::from_utf8_lossy(&out.stderr).trim().to_string();
            assert!(!rewritten.is_empty(), "no rewrite: {fm}");
            let tr = Instant::now();
            let now = voice_edit::current_target(Some(pid));
            eprintln!(
                "    pre-replace target={:?}/{:?} sel={:?} value={:?}",
                now.source,
                now.text,
                e.selected_range(),
                e.value()
            );
            let (method, _) = voice_edit::replace_target(
                &target.text,
                &rewritten,
                Some(pid),
                KeyTarget::Pid(pid),
            )
            .unwrap();
            let replace_ms = tr.elapsed().as_secs_f64() * 1000.0;
            let total = t0.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(e.value().unwrap(), format!("{rewritten} "));
            eprintln!(
                "  {style}: {fm} (incl. helper spawn) replace={replace_ms:.1}ms ({method}) total={total:.0}ms\n    → {rewritten:?}"
            );
            // One ⌘Z restores the original.
            undo(pid);
            let t = Instant::now();
            while e.value().as_deref() != Some(original) && t.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                e.value().as_deref(),
                Some(original),
                "single ⌘Z did not restore"
            );
            eprintln!(
                "    ⌘Z restored original in {:.0} ms",
                t.elapsed().as_secs_f64() * 1000.0
            );
            // Undo selects the restored text; put the caret back at the end
            // where the paste left it. The anchor still describes it.
            e.set_selected_range(ax_edit::len_u16(original), 0);
            voice_edit::note_paste_in(original, Some(pid), true);
        }
        let n = ax_edit::len_u16(&e.value().unwrap_or_default());
        e.set_selected_range(0, n);
        e.set_selected_text("");
    }
}
