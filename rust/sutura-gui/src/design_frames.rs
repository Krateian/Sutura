// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Headless frame capture for design review.
//!
//! `egui_kittest` renders the real `frame_ui` through a GPU surface it owns, so
//! this produces the actual pixels **without creating a native window**. The
//! earlier `--screenshot` path ran the real binary, and eframe can only draw by
//! materialising a window -- on a reviewer's Mac that window appeared on screen.
//! A test that renders offscreen cannot.
//!
//! Every test is `#[ignore]`d: this is a capture tool, not an assertion suite,
//! and it must not run as part of `cargo test`. Invoke it explicitly:
//!
//! ```text
//! SUTURA_CAPTURE_DIR=/tmp/qt-vs-rust \
//!   cargo test -p sutura-gui design_frames -- --ignored --nocapture \
//!   --test-threads=1
//! ```

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

use crate::app::{OptionsTab, SuturaApp};

/// Serialise the captures: the UI language is process-global, as in the
/// interaction suite.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// A small mesh on disk, so the "file loaded" frame is a real file.
fn sample_mesh(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sutura-design-{}-{}", std::process::id(), tag));
    std::fs::create_dir_all(&dir).unwrap();
    // The table shows the file name, so the name has to differ per call or every
    // row reads "hero-broken.stl".
    let path = dir.join(format!("{tag}.stl"));
    let mesh = sutura_core::Mesh::new(
        vec![
            [0.0, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [0.0, 10.0, 0.0],
            [0.0, 0.0, 10.0],
            [10.0, 10.0, 0.0],
            [10.0, 0.0, 10.0],
            [0.0, 10.0, 10.0],
            [10.0, 10.0, 10.0],
        ],
        vec![
            [0, 2, 1],
            [0, 1, 3],
            [1, 2, 3],
            [4, 5, 6],
            [4, 7, 5],
            [5, 7, 6],
        ],
    );
    sutura_core::write_stl_binary(&path, &mesh).expect("write fixture");
    path
}

fn harness(size: egui::Vec2) -> Harness<'static, SuturaApp> {
    crate::i18n::apply(crate::i18n::Language::En);
    // `SuturaApp::for_test` skips the constructor, which is where the theme is
    // applied -- so a capture without this renders every widget in egui's stock
    // colours. That is exactly what the first design frames showed: a bright
    // turquoise selection in a window that was otherwise correctly dark.
    let mut h = Harness::builder()
        .with_size(size)
        .with_max_steps(24)
        .build_ui_state(
            |ui, app: &mut SuturaApp| app.frame_ui(ui),
            SuturaApp::for_test(),
        );
    let ctx = h.ctx.clone();
    crate::theme::apply(&ctx);
    h
}

fn save(h: &mut Harness<'_, SuturaApp>, name: &str) {
    let dir = std::env::var("SUTURA_CAPTURE_DIR")
        .unwrap_or_else(|_| "/tmp/sutura-design".to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let image = h.render().expect("offscreen render");
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    image.save(&path).expect("write png");
    println!("wrote {}", path.display());
}

/// The three widths the reviewer asked for, plus a loaded frame at each.
#[test]
#[ignore = "capture tool: run explicitly with --ignored"]
fn frame_widths() {
    let _serial = serial();
    for (name, w, h, files) in [
        ("design-narrow", 720.0, 900.0, true),
        ("design-medium", 1000.0, 900.0, true),
        ("design-wide", 1400.0, 900.0, true),
    ] {
        let mut h = harness(egui::vec2(w, h));
        if files {
            // Several files, and the names differ: `add_path` de-duplicates by
            // path, so repeated calls with one fixture produced ONE row.
            for i in 0..4 {
                h.state_mut()
                    .add_file_for_test(sample_mesh(&format!("{name}-{i}")));
            }
        }
        for _ in 0..10 {
            h.step();
        }
        h.state_mut().select_first_for_test();
        for _ in 0..2 {
            h.step();
        }
        eprintln!("{name}: {} file(s) loaded", h.state().file_count());
        save(&mut h, name);
    }
}

/// The empty state: one calm card with a drop hint, nothing else.
#[test]
#[ignore = "capture tool: run explicitly with --ignored"]
fn frame_empty_state() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1100.0, 760.0));
    for _ in 0..6 {
        h.step();
    }
    save(&mut h, "design-empty");
}

/// With a file loaded: the table has a row and the action row is live.
#[test]
#[ignore = "capture tool: run explicitly with --ignored"]
fn frame_with_file() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1100.0, 760.0));
    h.state_mut().add_file_for_test(sample_mesh("loaded"));
    for _ in 0..8 {
        h.step();
    }
    save(&mut h, "design-loaded");
}

/// Options in the same style, for the round after this one.
#[test]
#[ignore = "capture tool: run explicitly with --ignored"]
fn frame_options() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1100.0, 820.0));
    h.state_mut().add_file_for_test(sample_mesh("options"));
    h.state_mut().open_options_for_test(OptionsTab::Repair);
    for _ in 0..8 {
        h.step();
    }
    save(&mut h, "design-options");
}
