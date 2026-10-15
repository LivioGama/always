//! `always update` / `always update --check` (SPEC §12a).

use std::io::{IsTerminal as _, Write as _};

use always::always::updater::apply::{self, Context, Downloader, Progress};
use always::always::updater::release::{self, CheckOutcome, Endpoints, Release};
use always::always::updater::verify::Verifier;
use anyhow::Result;

/// Exit codes of `always update --check`.
pub const EXIT_UP_TO_DATE: i32 = 0;
pub const EXIT_UPDATE_AVAILABLE: i32 = 10;
pub const EXIT_ERROR: i32 = 1;

/// Returns the process exit code.
pub fn run(check_only: bool) -> Result<i32> {
    let current = release::current_version();
    let outcome = release::fetch_latest(&Endpoints::github(), &current);
    tracing::info!(result = outcome.result(), check_only, "update_check_done");
    println!("{}", outcome.describe(&current));
    match outcome {
        CheckOutcome::Available(release) => {
            println!("  {}", release.html_url);
            if check_only {
                println!("  Run `always update` to install it.");
                return Ok(EXIT_UPDATE_AVAILABLE);
            }
            install(&release)
        }
        CheckOutcome::UpToDate { .. } | CheckOutcome::NoRelease | CheckOutcome::Skipped(_) => {
            Ok(EXIT_UP_TO_DATE)
        }
        CheckOutcome::Offline(_) | CheckOutcome::RateLimited { .. } | CheckOutcome::Failed(_) => {
            Ok(EXIT_ERROR)
        }
    }
}

fn install(release: &Release) -> Result<i32> {
    let plan = match apply::plan_for_host(Context::Cli) {
        Ok(plan) => plan,
        Err(why) => {
            eprintln!("Cannot update this install: {why}");
            return Ok(EXIT_ERROR);
        }
    };
    let verifier = match Verifier::embedded() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Refusing to update: {e}");
            return Ok(EXIT_ERROR);
        }
    };
    let downloader = match Downloader::new(&Endpoints::github()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Update stopped: {e}");
            return Ok(EXIT_ERROR);
        }
    };
    apply::sweep_stale_downloads();
    apply::remove_leftover_exe();

    let tty = std::io::stdout().is_terminal();
    let mut last_line = false;
    let staged = apply::stage(release, &plan, &verifier, &downloader, &mut |p| match p {
        Progress::Downloading { asset, percent } => match percent {
            Some(pc) if tty => {
                print!("\rDownloading {asset} ... {pc}%");
                let _ = std::io::stdout().flush();
                last_line = true;
            }
            Some(0) | None => println!("Downloading {asset} ..."),
            Some(_) => {}
        },
        Progress::Verifying => {
            if last_line {
                println!();
                last_line = false;
            }
            println!("Verifying signatures ...");
        }
    });
    let staged = match staged {
        Ok(s) => s,
        Err(e) => {
            if last_line {
                println!();
            }
            eprintln!("Update stopped ({}): {e}", e.stage());
            eprintln!("Nothing was installed.");
            return Ok(EXIT_ERROR);
        }
    };

    println!(
        "Verified against the Always release key. Installing {} into {} ...",
        release.tag,
        plan.location.display()
    );
    let status = apply::run_installer_wait(&staged, &plan, &release.tag);
    staged.discard();
    match status {
        Ok(s) if s.success() => {
            println!("Always updated to {}.", release.tag);
            if always::always::daemon::is_running() && !cfg!(target_os = "macos") {
                println!(
                    "A daemon is still running the old version; restart it to use the new one."
                );
            }
            Ok(EXIT_UP_TO_DATE)
        }
        Ok(s) => {
            eprintln!("The installer exited with {s}.");
            Ok(EXIT_ERROR)
        }
        Err(e) => {
            eprintln!("Update stopped ({}): {e}", e.stage());
            Ok(EXIT_ERROR)
        }
    }
}
