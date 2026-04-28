//! Loading overlay system for showing voice processing status

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Global loading state
static LOADING_STATE: once_cell::sync::Lazy<Arc<AtomicBool>> =
    once_cell::sync::Lazy::new(|| Arc::new(AtomicBool::new(false)));

/// Show a loading overlay while processing voice input
pub fn show_loading_overlay() -> anyhow::Result<()> {
    LOADING_STATE.store(true, Ordering::Relaxed);
    eprintln!("🎤 Processing audio...");
    Ok(())
}

/// Hide the loading overlay
pub fn hide_loading_overlay() -> anyhow::Result<()> {
    LOADING_STATE.store(false, Ordering::Relaxed);
    Ok(())
}

/// Show an animated loading indicator with dots
pub fn show_animated_loading() -> anyhow::Result<std::thread::JoinHandle<()>> {
    LOADING_STATE.store(true, Ordering::Relaxed);

    let handle = thread::spawn(|| {
        let dots = ["🎤 Processing   ", "🎤 Processing.  ", "🎤 Processing.. ", "🎤 Processing..."];
        let mut counter = 0;

        while LOADING_STATE.load(Ordering::Relaxed) {
            eprint!("\r{}", dots[counter % 4]);
            std::io::Write::flush(&mut std::io::stderr()).unwrap();
            counter += 1;
            thread::sleep(Duration::from_millis(500));
        }
        eprintln!(); // New line when done
    });

    Ok(handle)
}

/// Show a heads-up display overlay with loading dots
pub fn show_hud_loading() -> anyhow::Result<std::thread::JoinHandle<()>> {
    LOADING_STATE.store(true, Ordering::Relaxed);

    let handle = thread::spawn(|| {
        eprintln!("🎤 Processing audio...");
        thread::sleep(Duration::from_millis(500));
    });

    Ok(handle)
}

/// Show a simple persistent overlay
pub fn show_persistent_overlay() -> anyhow::Result<()> {
    eprintln!("🎤 Processing audio...");
    Ok(())
}

/// Hide any active loading overlays
pub fn stop_loading() {
    LOADING_STATE.store(false, Ordering::Relaxed);
    eprint!("\r   \r");
    let _ = std::io::Write::flush(&mut std::io::stderr());
}

/// Check if loading is currently active
pub fn is_loading() -> bool {
    LOADING_STATE.load(Ordering::Relaxed)
}