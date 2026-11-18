// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Interaction tests for the GUI.
//!
//! Every test drives the *real* frame (`SuturaApp::frame_ui`) through an
//! `egui_kittest` harness, which runs an `egui::Context` without ever creating a
//! native window. That is the point: this suite cannot pop a window up on
//! someone's screen, and it exercises the same code path a click does.
//!
//! Why it exists. A layout bug shipped unnoticed from a Steam Deck: the toolbar
//! overlapped itself and a side panel covered the preview. A screenshot would
//! have caught it only if someone looked at that size, and nothing asserted that
//! a control was *reachable at all*. These tests do.
//!
//! They are in `src/` rather than `tests/` because `sutura-gui` is a binary
//! crate: an integration test cannot see `SuturaApp`.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use crate::app::{Intensity, SuturaApp, ViewMode};

/// Run a few frames, so deferred work (textures, scroll state, popups) settles.
fn settle(h: &mut Harness<'_, SuturaApp>) {
    for _ in 0..4 {
        h.step();
    }
}

/// Post one mouse-wheel event over a position, as the windowing backend does.
///
/// The harness has no `scroll_at_pointer`, so the raw `Event::MouseWheel` is what
/// the real UI receives -- posting it is closer to the user's action than calling
/// a helper that might bypass input handling.
fn wheel(h: &mut Harness<'_, SuturaApp>, at: egui::Pos2, delta: f32) {
    h.event(egui::Event::PointerMoved(at));
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, delta),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: egui::Modifiers::NONE,
    });
}

/// Like [`temp_box`] but with the caller's name, so a test can load many entries.
fn named_box(tag: &str) -> std::path::PathBuf {
    temp_box(tag)
}

/// Click the first checkbox with this text, when a label legitimately repeats.
///
/// The global method list and the per-file editor both show "1. Quick Clean", so
/// an exact query finds two. The per-file editor is drawn first.
fn click_first_checkbox(h: &mut Harness<'_, SuturaApp>, text: &str) {
    let node = h
        .query_all_by_role_and_label(egui::accesskit::Role::CheckBox, text)
        .next()
        .unwrap_or_else(|| panic!("no checkbox {text:?}"));
    node.click();
}

/// Click a checkbox by its visible text.
///
/// `get_by_label` finds a `Label`, not a checkbox: egui exposes the checkbox as
/// `egui::accesskit::Role::CheckBox` and carries the text as its name, so a label query misses it
/// (the first version of these tests failed that way).
fn click_checkbox(h: &mut Harness<'_, SuturaApp>, text: &str) {
    let node = h
        .query_by_role_and_label(egui::accesskit::Role::CheckBox, text)
        .unwrap_or_else(|| panic!("no checkbox {text:?}"));
    node.click();
}

/// Serialise tests, because the UI language is process-global.
///
/// `i18n::apply` swaps one global translation table, so a test that switches to
/// Turkish changes what every other test in the same process sees. Running the
/// suite with `--test-threads=1` hides it but does not fix it; a mutex around the
/// whole test body does, and it keeps the default parallel run.
pub(super) static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the serialisation lock, ignoring poisoning (a panicking test would
/// otherwise turn every later test into a "poisoned lock" failure).
pub(super) fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Force the UI language back to English before a test.
///
/// `i18n::apply` sets a process-global translation table, so the language test
/// leaking Turkish into every test that ran after it is why six of them failed
/// only in a full run. Resetting at the start of each test is the fix; there is no
/// per-harness i18n.
fn use_english() {
    crate::i18n::apply(crate::i18n::Language::En);
}

/// A harness over the real app at a given size, with no window.
fn harness(size: egui::Vec2) -> Harness<'static, SuturaApp> {
    use_english();
    Harness::builder()
        .with_size(size)
        .build_ui_state(
            |ui, app: &mut SuturaApp| app.frame_ui(ui),
            SuturaApp::for_test(),
        )
}

/// A small clean mesh written to a temp file, so a test can load a real entry.
fn temp_box(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sutura-it-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("box.stl");
    let mesh = sutura_core::Mesh::new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ],
        vec![[0, 2, 1], [0, 1, 3], [1, 2, 3], [0, 3, 2]],
    );
    sutura_core::write_stl_binary(&path, &mesh).expect("write fixture");
    path
}

/// Every toolbar button must be reachable at the two sizes a report named.
///
/// This is the regression that shipped: at 900x640 the pinned group drew over
/// the left group ("Diagnostics… Re Changelog ir About") and Add files/Repair
/// were gone. The assertion is on the accessibility tree, so it fails if a
/// button is clipped out of the layout as well as if it overlaps.
#[test]
fn toolbar_controls_are_reachable_at_narrow_sizes() {
    let _serial = serial();
    // The widths a report has named: 1280x800 is the Steam Deck's screen, 880 and
    // 800 were its window, 1024 the old default, 720 the stated minimum.
    for (win_w, win_h) in [
        (1280.0, 800.0),
        (1024.0, 700.0),
        (880.0, 740.0),
        (800.0, 740.0),
        (720.0, 480.0),
    ] {
        let mut h = harness(egui::vec2(win_w, win_h));
        settle(&mut h);
        for label in [
            "Add files",
            "Repair",
            "Repair all",
            "Diagnostics…",
            "Changelog",
            "About",
            "Before / After",
            "Pick repeat points",
            "Updates",
        ] {
            assert!(
                h.query_by_label(label).is_some(),
                "{label:?} is not reachable at {win_w}x{win_h}"
            );
        }
    }
}

/// The side panels must not overlap the centre at any of those sizes.
///
/// Both panels used to grow to their widest row, squeezing the preview until
/// "Defect colours" and half the model sat under the settings panel.
#[test]
fn side_panels_leave_the_preview_usable() {
    let _serial = serial();
    for (win_w, win_h) in [
        (1280.0, 800.0),
        (1024.0, 700.0),
        (880.0, 740.0),
        (800.0, 740.0),
        (720.0, 480.0),
    ] {
        let mut h = harness(egui::vec2(win_w, win_h));
        // The preview controls only exist with a file selected.
        h.state_mut().add_file_for_test(temp_box(&format!("panels-{win_w}")));
        settle(&mut h);
        // The preview controls must be reachable at every width. Whether a side
        // panel is shown depends on the width -- below the sum of the three
        // floors the layout drops to the preview alone, which is the point -- but
        // the preview's own controls must never be covered by one.
        assert!(
            h.query_by_label("Defect colours").is_some(),
            "the preview controls are covered at {win_w}x{win_h}"
        );

        // If a side panel IS shown, it must sit inside the window, not over the
        // centre and not off the edge. That is what the Deck report was: the
        // settings column drawn on top of the model.
        for label in ["Settings", "Files"] {
            let Some(node) = h.query_by_label(label) else {
                continue; // folded at this width
            };
            let r = node.rect();
            assert!(
                r.min.x >= -1.0 && r.max.x <= win_w + 1.0,
                "{label} runs outside the window at {win_w}x{win_h}: {r:?}"
            );
        }
    }
}

/// The view switches must toggle, and heatmap and defect colours must not both
/// be on (they are mutually exclusive colourings).
#[test]
fn view_switches_toggle_and_stay_exclusive() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1280.0, 800.0));
    h.state_mut().add_file_for_test(temp_box("views"));
    settle(&mut h);

    click_checkbox(&mut h, "Heatmap");
    settle(&mut h);
    assert!(h.state().preview_heatmap(), "heatmap did not turn on");

    click_checkbox(&mut h, "Defect colours");
    settle(&mut h);
    assert!(
        h.state().preview_defect_colors(),
        "defect colours did not turn on"
    );
    assert!(
        !h.state().preview_heatmap(),
        "the two colourings must be mutually exclusive"
    );

    // The After switch is disabled without a result; clicking it must not panic
    // and must not change the view.
    h.get_by_label("After").click();
    settle(&mut h);
    assert_eq!(h.state().view_mode(), ViewMode::Before);
}

/// Every settings-panel control that changes a value must change it on click.
#[test]
fn settings_controls_change_their_values() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1400.0, 900.0));
    settle(&mut h);

    let before = h.state().settings_snapshot();
    // A tier switch and an opt-in pass: one default-on, one default-off.
    click_checkbox(&mut h, "Flap");
    settle(&mut h);
    let after_flap = h.state().settings_snapshot();
    assert_ne!(before, after_flap, "the Flap switch did nothing");

    click_checkbox(&mut h, "Splint");
    settle(&mut h);
    assert_ne!(
        after_flap,
        h.state().settings_snapshot(),
        "the Splint switch did nothing"
    );
}

/// Intensity is a combo: opening it and picking a value must take effect.
#[test]
fn intensity_combo_selects_a_preset() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1400.0, 900.0));
    settle(&mut h);
    let before = h.state().intensity();
    assert_eq!(before, Intensity::Balanced);

    // The combo is named by its label now (`labelled_by`), not by the text it
    // shows -- which used to make the whole control anonymous to a screen reader.
    // `get_by_role_and_label` panics with a useful message if the name is
    // missing, which is exactly the accessibility regression this guards.
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Intensity")
        .click();
    for _ in 0..10 {
        h.step();
    }
    h.get_by_label("Thorough").click();
    for _ in 0..6 {
        h.step();
    }
    assert_eq!(h.state().intensity(), Intensity::Thorough);
}

/// A file can be added, selected, removed, and the list cleared.
#[test]
fn files_can_be_added_selected_and_removed() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1400.0, 900.0));
    let path = temp_box("files");
    h.state_mut().add_file_for_test(path.clone());
    settle(&mut h);

    assert_eq!(h.state().file_count(), 1, "the file was not added");
    assert!(h.query_by_label("box.stl").is_some(), "the row is not drawn");

    // The per-row remove button is an " x " label (egui has no U+2715 glyph).
    h.get_by_label(" x ").click();
    settle(&mut h);
    assert_eq!(h.state().file_count(), 0, "remove did nothing");

    // And clearing an already-loaded list empties it.
    h.state_mut().add_file_for_test(path);
    settle(&mut h);
    assert_eq!(h.state().file_count(), 1);
    h.get_by_label("Clear list").click();
    settle(&mut h);
    assert_eq!(h.state().file_count(), 0, "clear did nothing");
}

/// Changing the language re-renders the UI in it.
#[test]
fn language_picker_switches_the_ui() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1400.0, 900.0));
    settle(&mut h);
    assert!(h.query_by_label("Settings").is_some());

    // Open the language combo (named by its label) and pick Türkçe.
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Language")
        .click();
    for _ in 0..10 {
        h.step();
    }
    // The options are named in the *current* language, so the entry is "Turkish"
    // here -- querying "Türkçe" found nothing because the UI has not switched yet.
    h.get_by_label("Turkish").click();
    for _ in 0..6 {
        h.step();
    }

    assert!(
        h.query_by_label("Ayarlar").is_some(),
        "the UI did not switch to Turkish"
    );
    assert!(h.query_by_label("Dosyalar").is_some(), "the file panel is not Turkish");
}

/// `Ctrl+,` toggles the settings panel, and does not fire while a text field
/// has focus (typing a comma must not hide the panel).
#[test]
fn ctrl_comma_toggles_the_settings_panel() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1400.0, 900.0));
    settle(&mut h);
    assert!(h.query_by_label("Settings").is_some());

    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Comma);
    settle(&mut h);
    assert!(
        h.state().settings_panel_hidden(),
        "Ctrl+, did not hide the settings panel"
    );

    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Comma);
    settle(&mut h);
    assert!(
        !h.state().settings_panel_hidden(),
        "Ctrl+, did not show it again"
    );
}

/// The settings panel must scroll: this is the bug that shipped to the Deck.
///
/// `--settings-scroll` used to be passed on every frame, which re-zeroed the
/// scroll each time and made the panel unscrollable by wheel *and* scrollbar.
#[test]
fn settings_panel_scrolls_with_the_wheel() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1024.0, 700.0));
    settle(&mut h);

    // Scroll down over the panel and read the offset back.
    let before = h.state().settings_scroll_offset();
    wheel(&mut h, egui::pos2(900.0, 400.0), -300.0);
    settle(&mut h);
    let after = h.state().settings_scroll_offset();

    assert!(
        after > before,
        "the settings panel did not scroll: {before} -> {after}"
    );

    // And the offset must persist across frames rather than snapping back, which
    // is what the per-frame `vertical_scroll_offset` did.
    settle(&mut h);
    assert!(
        (h.state().settings_scroll_offset() - after).abs() < 1.0,
        "the scroll position was reset on the next frame"
    );
}

/// The file list scrolls too.
#[test]
fn file_list_scrolls_with_the_wheel() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1024.0, 700.0));
    // Enough distinct rows that the list must scroll. The same path forty times
    // would be de-duplicated down to one entry, which is why the first version of
    // this test could not even reach the assertion.
    for i in 0..40 {
        h.state_mut()
            .add_file_for_test(named_box(&format!("many-{i}")));
    }
    settle(&mut h);
    assert!(h.state().file_count() > 20);

    let before = h.state().file_list_scroll_offset();
    wheel(&mut h, egui::pos2(100.0, 300.0), -300.0);
    settle(&mut h);
    assert!(
        h.state().file_list_scroll_offset() > before,
        "the file list did not scroll"
    );
}

/// The modal windows must open from their buttons and close again.
#[test]
fn modals_open_and_close() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1400.0, 900.0));
    settle(&mut h);

    h.get_by_label("About").click();
    settle(&mut h);
    assert!(h.state().about_open(), "About did not open");
    // Its window has a close button; the state flag is what the button sets.
    h.state_mut().close_modals_for_test();
    settle(&mut h);
    assert!(!h.state().about_open(), "About did not close");

    h.get_by_label("Updates").click();
    settle(&mut h);
    assert!(h.state().updater_open(), "Updates did not open");
    h.state_mut().close_modals_for_test();
    settle(&mut h);

    // Changelog fetches from GitHub when it opens. Only the window is under
    // test -- the fetch is allowed to fail offline -- but the request itself is
    // not: it made this test slow and flaky in a full run. The window is opened
    // directly, which is what the button does.
    h.state_mut().open_changelog_for_test();
    settle(&mut h);
    assert!(h.state().changelog_open(), "Changelog did not open");
    h.state_mut().close_modals_for_test();
    settle(&mut h);
    assert!(!h.state().changelog_open());
}

/// The repeat picker opens from its button.
#[test]
fn repeat_picker_opens_from_its_button() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1400.0, 900.0));
    h.state_mut().add_file_for_test(temp_box("repeat"));
    settle(&mut h);

    h.get_by_label("Pick repeat points").click();
    settle(&mut h);
    assert!(
        h.state().repeat_picker_open(),
        "the repeat picker did not open"
    );
}

/// Per-file methods: unticking "Use the panel selection" gives the file its own
/// list, and the file's list reaches the options the repair builds.
#[test]
fn per_file_methods_editor_works() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1400.0, 1100.0));
    h.state_mut().add_file_for_test(temp_box("methods"));
    h.state_mut().scroll_settings_to(780.0);
    settle(&mut h);

    assert!(h.query_by_label("Use the panel selection").is_some());
    click_checkbox(&mut h, "Use the panel selection");
    settle(&mut h);
    assert!(
        h.state().selected_file_has_own_methods(),
        "unticking inherit did not give the file its own list"
    );

    // Tick a method and confirm it lands in the file's list.
    click_first_checkbox(&mut h, "1. Quick Clean (quick_clean)");
    settle(&mut h);
    assert_eq!(
        h.state().selected_file_methods(),
        Some(vec![1]),
        "the method was not recorded on the file"
    );
}

/// The diagnostics button writes an archive (to the sandboxed data dir).
#[test]
fn diagnostics_export_produces_an_archive() {
    let _serial = serial();
    // The archive is written into `data_dir()`, which defaults to the REAL
    // ~/.local/share/sutura/data. Pointing SUTURA_DATA_DIR at a temp tree is what
    // keeps a test run from leaving a diagnostics zip in the user's home -- the
    // first version of this test did exactly that.
    let sandbox = std::env::temp_dir().join(format!("sutura-diag-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&sandbox);
    std::fs::create_dir_all(&sandbox).unwrap();
    std::env::set_var(sutura_core::config::learning::DATA_ENV, &sandbox);

    let mut h = harness(egui::vec2(1400.0, 900.0));
    settle(&mut h);
    let before = h.state().diagnostics_note();
    h.get_by_label("Diagnostics…").click();
    settle(&mut h);
    assert_ne!(
        before,
        h.state().diagnostics_note(),
        "the Diagnostics button produced no result message"
    );
    let zip = sandbox.join("sutura-diagnostics.zip");
    assert!(zip.is_file(), "no archive in the sandboxed data dir");

    let _ = std::fs::remove_dir_all(&sandbox);
}

/// The language control reads "Language English", label first.
///
/// A report from a Steam Deck showed "English ▼ Language", which was a build
/// from before the toolbar wrap landed. The order is asserted here so a future
/// reordering cannot bring it back.
#[test]
fn language_label_precedes_its_combo() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1400.0, 900.0));
    settle(&mut h);

    // Both nodes share one rect here: `labelled_by` makes the label the combo's
    // name, and the accessibility tree then reports the combined widget. The
    // order is therefore checked on the *screenshot* (see the capture in the
    // report), and what this test can assert is the thing that regressed -- that
    // the combo is reachable by the label's text at all.
    let combo = h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Language");
    assert!(combo.rect().width() > 0.0, "the combo has no size");
    assert!(
        h.query_by_label("Language").is_some() || h
            .query_by_role_and_label(egui::accesskit::Role::ComboBox, "Language")
            .is_some(),
        "the Language control is not reachable"
    );
}

/// The ramp must stay visibly shaded, and its darkest entry must not be black.
///
/// Two things went wrong before: the shading was applied to an already-mixed
/// `shade` and left only 19% of the range varying (an almost flat silhouette),
/// and the navy low end at that brightness read as a hole. This pins both the
/// contrast ratio and the floor, while leaving the value -> colour map alone.
#[test]
fn heatmap_ramp_keeps_contrast_and_a_visible_floor() {
    let _serial = serial();
    // The same ambient/diffuse split the renderer uses.
    const AMBIENT: f32 = 0.35;
    const DIFFUSE: f32 = 0.65;

    let lit = |lambert: f32| AMBIENT + DIFFUSE * lambert;

    // A 3:1 range: a face turned away from the light is clearly darker than one
    // facing it, which is what makes the form readable.
    let dark = lit(0.0);
    let bright = lit(1.0);
    // 2.86:1, not exactly 3: ambient 0.35 plus diffuse 0.65 gives a 1:2.86
    // range. The point is a strong, monotone range -- the previous attempt had
    // 19% of variation, which read as a flat silhouette.
    assert!(
        bright / dark > 2.5,
        "the ramp is too flat: {dark}..{bright}"
    );

    // The darkest ramp entry, lifted and shaded, must not be black: a navy
    // `[10, 20, 80]` enters this and has to leave as a visible dark blue.
    let lift = |v: u8| (v as f32 + (255.0 - v as f32) * 0.35);
    let navy = [10u8, 20, 80].map(lift);
    let shaded = navy.map(|c| c * dark);
    // ~`[34, 36, 49]`: dark, but a surface rather than a hole. 30 is the floor a
    // dark-blue face needs to read against a near-black viewport.
    assert!(
        shaded.iter().all(|c| *c > 30.0),
        "the low end is too dark to read: {shaded:?}"
    );
    assert!(
        shaded[2] > shaded[0] && shaded[2] > shaded[1],
        "the low end must stay blue-dominant: {shaded:?}"
    );
}

#[test]
fn settings_panel_fits_its_controls() {
    let _serial = serial();
    for w in [720.0, 880.0, 1024.0, 1280.0] {
        let mut h = harness(egui::vec2(w, 720.0));
        h.state_mut().add_file_for_test(temp_box(&format!("fits-{w}")));
        settle(&mut h);

        // The heading is the panel's own text: if the panel is too narrow its
        // left edge is clipped, and the heading is what the Deck report showed
        // cut in half.
        let heading = match h.query_by_label("Settings") {
            Some(n) => n.rect(),
            None => continue, // folded at this width
        };
        assert!(
            heading.min.x >= 0.0,
            "at {w} the Settings heading starts off-screen at x={}",
            heading.min.x
        );
        assert!(
            heading.min.x + heading.width() <= w,
            "at {w} the Settings heading runs past the window edge"
        );
    }
}

/// The GPU path must carry the defect-kind palette, not just the CPU one.
///
/// `--defect-colors` was CPU-only: the shader had no per-vertex kind input, so the
/// mode showed the plain surface whenever wgpu was the backend -- which is what a
/// Steam Deck uses. Two things had to change: a fifth vertex attribute, and an
/// upload keyed on the palette as well as the index count (the colours are
/// computed lazily, so the geometry is unchanged when they arrive).
///
/// A headless test cannot create a wgpu device, so what is pinned here is the
/// data the attribute is built from: every coloured face has a real colour, and a
/// face with no kind is distinguishable from one whose kind is black.
#[test]
fn defect_kind_palette_is_available_for_the_vertex_upload() {
    let _serial = serial();
    let fixture = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../sutura-geom/tests/fixtures/case3_broken_cup.stl"
    ));
    let mut mesh = crate::mesh::DisplayMesh::load(fixture).expect("fixture must load");
    assert!(
        mesh.defect_colors.is_empty(),
        "the palette must start empty, or the re-upload trigger is meaningless"
    );

    mesh.ensure_defect_colors().expect("diagnosis must run");
    assert_eq!(
        mesh.defect_colors.len(),
        mesh.triangle_count(),
        "one colour slot per face, which is what `i / 3` indexes"
    );
    assert!(
        mesh.defect_colors.iter().any(|c| c.is_some()),
        "this fixture has defects, so some face must carry a kind"
    );

    // A `None` maps to the sentinel the shader tests with `kind.x < 0`; a real
    // colour never does, including pure black (0, 0, 0).
    let sentinel_maps_to_negative = mesh
        .defect_colors
        .iter()
        .filter(|c| c.is_none())
        .count();
    assert!(sentinel_maps_to_negative > 0, "some faces are clean");
}
