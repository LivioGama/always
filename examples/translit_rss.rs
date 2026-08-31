//! Resident-set cost of the learned transliterator, measured rather than
//! estimated.
//!
//! Run it three ways and diff the peaks:
//!
//! ```text
//! cargo run --release --example translit_rss -- english
//! cargo run --release --example translit_rss -- cached
//! cargo run --release --example translit_rss -- model
//! ```
//!
//! `english` never touches Devanagari, `cached` hits only the exact table,
//! and `model` forces one out-of-vocabulary word through ORT — which is the
//! only mode that builds the two ONNX sessions. The delta between `cached`
//! and `model` is what a Nepali session pays for the learned fallback.
use always::always::translit::romanize;

fn peak_rss_bytes() -> u64 {
    // ru_maxrss is in BYTES on macOS and KILOBYTES on Linux.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    if cfg!(target_os = "macos") {
        ru.ru_maxrss as u64
    } else {
        ru.ru_maxrss as u64 * 1024
    }
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "english".into());
    let before = peak_rss_bytes();
    let text = match mode.as_str() {
        // Every word is in `dev_roman.tsv`.
        "cached" => "हुन्छ मलाई धेरै राम्रो काम गर्न भयो छैन पर्छ",
        // `ट्रान्सलिटरेसन` is not, so this builds the ONNX sessions.
        "model" => "हुन्छ मलाई धेरै ट्रान्सलिटरेसन गर्न माइक्रोसफ्टको छैन पर्छ",
        _ => "send the quarterly report to Bob before five and cc the team",
    };
    let out = romanize(text);
    let after = peak_rss_bytes();
    println!("mode={mode}");
    println!("out={out}");
    println!(
        "peak rss: before={:.2} MiB  after={:.2} MiB  delta={:.2} MiB",
        before as f64 / 1048576.0,
        after as f64 / 1048576.0,
        (after - before) as f64 / 1048576.0
    );
    // `ru_maxrss` is a HIGH-WATER MARK, which for ORT includes the transient
    // arena it allocates while optimising the graph. What the daemon actually
    // carries afterwards is the resident set now, so report both.
    let rss = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(0.0);
    println!("steady rss: {:.2} MiB", rss / 1024.0);
}
