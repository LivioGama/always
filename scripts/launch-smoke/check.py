# Assertions for the launch smoke test (log + UDS events).
import json, sys, glob, os, collections
logs = sorted(glob.glob(os.path.expanduser("~/Library/Logs/Always-smoke/always.*")))
msgs = []
for p in logs:
    for l in open(p):
        try: d = json.loads(l)
        except Exception: continue
        msgs.append((d["timestamp"][11:23], d["fields"].get("message", ""), d["fields"]))
evs = [json.loads(json.loads(l)["ev"]) for l in open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "events.jsonl")) if json.loads(l)["ev"].strip()]
types = collections.Counter(e["type"] for e in evs)
def has(m): return [x for x in msgs if x[1] == m]
hb = [(x[2]["frames_total"]) for x in has("capture_heartbeat")]
hb_ts = [x[0] for x in has("capture_heartbeat")]
def secs(t): h, m, s_ = t.split(":"); return int(h) * 3600 + int(m) * 60 + float(s_)
rate = (hb[-1] - hb[0]) / (secs(hb_ts[-1]) - secs(hb_ts[0])) if len(hb) >= 2 else 0
hb_ok = abs(rate - 33.33) / 33.33 < 0.1
print(f"capture heartbeat: {len(hb)} ticks, {rate:.1f} frames/s, rms per tick {[round(x[2]['rms'], 5) for x in has('capture_heartbeat')]}")
self_pause = [x for x in has("microphone_auto_paused") if any(k in str(x[2].get("apps", "")) for k in ("Always", "always"))]
any_pause = has("microphone_auto_paused")
checks = [
    ("UDS client connected (Hello received)", types.get("Hello", 0) >= 1),
    ("no microphone_auto_paused naming Always/self", not self_pause),
    ("no capture_preempted_by_mic_conflict", not has("capture_preempted_by_mic_conflict")),
    ("no self-conflict watchdog error", not has("mic_conflict_self_only_ignored")),
    ("sandbox reaped no other daemon", not has("reaping daemon processes that don't hold the PID lock")),
    ("capture running: heartbeat frame rate within 10% of 33.3/s", hb_ok),
] + [
    (f"cue '{c}' played (status_sound_played)", any(x[2].get("cue") == c for x in has("status_sound_played")))
    for c in ("listening", "transcribing", "done", "error", "cancel", "command")
] + [
    ("no status_sound_failed", not has("status_sound_failed") and not has("status_sound_load_failed_all")),
    ("AEC capture: every cue routed through the helper's (un-ducked) output path and rendered by it with non-zero peak",
        (not has("capture_helper_spawned")) or (
            all(x[2].get("route") in ("capture", "capture-dry") for x in has("status_sound_played"))
            and sum(1 for x in has("capture_helper_stderr")
                    if ("cue_played" in str(x[2].get("line")) and "playing=1" in str(x[2].get("line")))
                    or ("cue_rendered" in str(x[2].get("line")) and "peak=0.0000" not in str(x[2].get("line")))) >= 6)),
    ("AEC capture: ducking set to minimum (read back)",
        (not has("capture_helper_spawned")) or any("ducking advanced=false level=10" in str(x[2].get("line")) for x in has("capture_helper_stderr"))),
]
for name, ok in checks: print(("✅" if ok else "❌"), name)
print("event types:", dict(types))
print("mic pauses (any):", [(t, f.get("apps")) for t, _, f in any_pause])
print("recorder:", "always-capture (AEC)" if has("capture_helper_spawned") else ("rec" if has("rec_spawned") else "?"))
print("cues:", [(x[2].get("cue"), x[2].get("route"), x[2].get("level"), x[2].get("latency_ms")) for x in has("status_sound_played")])
print("helper cue lines:", [str(x[2].get("line")) for x in has("capture_helper_stderr") if "cue_" in str(x[2].get("line")) or "ducking" in str(x[2].get("line"))])
for k in ("capture_helper_spawned", "preview_asr_ready", "preview_asr_spawned", "voice_detected", "listening_overlay_shown"):
    hits = has(k); print(f"{k}: n={len(hits)}", (hits[0][0], {a: b for a, b in hits[0][2].items() if a != 'message'}) if hits else "")
sys.exit(0 if all(ok for _, ok in checks) else 1)
