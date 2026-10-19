This directory is the published `glib` 0.18.5 crates.io package, with one
backported source change in `src/variant_iter.rs` and local compatibility tests
in `tests/variant_iter_backport.rs`.

Provenance:

- Package: `glib` 0.18.5, crates.io archive `glib-0.18.5.crate`
- SHA-256 of the pristine archive: `233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5`
- Verified against the `glib` 0.18.5 checksum recorded in both shipping Cargo.lock files.
- Upstream fix: [gtk-rs/gtk-rs-core PR #1343](https://github.com/gtk-rs/gtk-rs-core/pull/1343),
  patch commit `b5a4071e439bef2b5eea76c3aa25e5ae84839e34`.
- Backport: in `VariantStrIter::impl_get`, make the output pointer mutable and pass `&mut p` to `g_variant_get_child`.
- Advisory: [GHSA-wrw7-89jp-8q8g](https://github.com/advisories/GHSA-wrw7-89jp-8q8g) /
  [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html).

The package manifest remains version 0.18.5. The `[patch.crates-io]` entries
in the two Tauri manifests select this local source while preserving the
upstream dependency constraints. The upstream MIT `LICENSE` and `COPYRIGHT`
files are retained unchanged. No other crate source is patched here.

Verification: `cargo test --release --manifest-path vendor/glib-0.18.5/Cargo.toml
--test variant_iter_backport` exercises the native GLib iterator with empty,
single-element, Unicode, mixed-direction, and wrong-type inputs. Undefined
behavior is not guaranteed to fail in an unpatched optimized build, so these
checks establish compatibility; the authoritative safety change is the
upstream mutable output-pointer fix. Version-based advisory scanners can still
report 0.18.5; do not suppress or dismiss the advisory based on the version.
