//! Compile-time product version.
//!
//! The Python CLI is still the release's single source of truth for the
//! product version (`sutura/repair.py`: `VERSION = "..."`). Reading it here
//! during the build keeps the Rust and Python binaries in lockstep; a missing
//! file or assignment is a hard build error, never a silent fallback.
//!
//! When the Python runtime is retired, the source moves to the Cargo
//! workspace `[workspace.package].version` and this file reads that instead
//! (recorded in `rust/sutura-core/PURE_API.md` and the Phase 6 CLI plan).

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set"),
    );
    let repo = manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("rust/sutura-cli must live under the repository root");
    let repair = repo.join("sutura").join("repair.py");
    println!("cargo:rerun-if-changed={}", repair.display());

    let text = std::fs::read_to_string(&repair).unwrap_or_else(|e| {
        panic!(
            "cannot read the product version from {}: {e}. The Python CLI is \
             still the single version source; restore the file or move the \
             version to [workspace.package] first.",
            repair.display()
        )
    });

    let version = text
        .lines()
        .find_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("VERSION")?;
            let rest = rest.trim_start().strip_prefix('=')?.trim_start();
            let rest = rest.strip_prefix('"')?;
            let end = rest.find('"')?;
            let value = &rest[..end];
            if value.is_empty()
                || !value
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c == '+')
            {
                return None;
            }
            Some(value.to_string())
        })
        .unwrap_or_else(|| {
            panic!(
                "no `VERSION = \"...\"` assignment in {}; the Rust CLI no longer \
                 has a product version source",
                repair.display()
            )
        });

    println!("cargo:rustc-env=SUTURA_PRODUCT_VERSION={version}");
}
