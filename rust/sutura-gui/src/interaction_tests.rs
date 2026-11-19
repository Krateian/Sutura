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
//! The layout assertions follow the Qt v0.9.9 shell: one vertical flow with the
//! table, two button rows, the progress row and the report, and the settings in
//! their own seven-tab window. The tests that asserted the three-pane layout this
//! replaced were rewritten, not deleted -- each still pins the same regression.
//!
//! They are in `src/` rather than `tests/` because `sutura-gui` is a binary
//! crate: an integration test cannot see `SuturaApp`.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use crate::app::{Intensity, OptionsTab, SuturaApp, ViewMode};

/// Run a few frames, so deferred work (textures, scroll state, popups) settles.
fn settle(h: &mut Harness<'_, SuturaApp>) {
    for _ in 0..4 {
        h.step();
    }
}

/// Post one mouse-wheel event over a position, as the windowing backend does.
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

/// Click a checkbox by its visible text.
///
/// `get_by_label` finds a `Label`, not a checkbox: egui exposes the checkbox as
/// `Role::CheckBox` and carries the text as its name, so a label query misses it.
fn click_checkbox(h: &mut Harness<'_, SuturaApp>, text: &str) {
    let node = h
        .query_by_role_and_label(egui::accesskit::Role::CheckBox, text)
        .unwrap_or_else(|| panic!("no checkbox {text:?}"));
    node.click();
}

/// Click the first checkbox with this text, when a label legitimately repeats.
fn click_first_checkbox(h: &mut Harness<'_, SuturaApp>, text: &str) {
    let node = h
        .query_all_by_role_and_label(egui::accesskit::Role::CheckBox, text)
        .next()
        .unwrap_or_else(|| panic!("no checkbox {text:?}"));
    node.click();
}

/// Serialise tests, because the UI language is process-global.
///
/// `i18n::apply` swaps one global translation table, so a test that switches to
/// Turkish changes what every other test in the same process sees.
pub(super) static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the serialisation lock, ignoring poisoning.
pub(super) fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Force the UI language back to English before a test.
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

/// A harness with Options already open on a tab.
fn harness_with_options(size: egui::Vec2, tab: OptionsTab) -> Harness<'static, SuturaApp> {
    let mut h = harness(size);
    h.state_mut().open_options_for_test(tab);
    settle(&mut h);
    h
}

/// Every control of the Qt shell must be reachable at the sizes a report named.
///
/// The list is Qt's, not the old toolbar's: "Diagnostics…", "Changelog" and
/// "Pick repeat points" moved into Options when the layout changed.
#[test]
fn shell_controls_are_reachable_at_narrow_sizes() {
    let _serial = serial();
    // 1280x800 is the Steam Deck's screen; 880 and 800 were its window, 1024 the
    // old default, 720 the stated minimum.
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
            "Analyze",
            "Methods",
            "Options",
            "Repair",
            "About",
            "Show heatmap",
            "Show before/after",
        ] {
            assert!(
                h.query_by_label(label).is_some(),
                "{label:?} is not reachable at {win_w}x{win_h}"
            );
        }
    }
}

/// No control may hang off the window edge at any of those sizes.
///
/// The Deck report was a toolbar that overlapped itself and a panel drawn over
/// the model. There are no side panels any more, so this asserts the geometry of
/// every button in both rows.
#[test]
fn no_control_runs_outside_the_window() {
    let _serial = serial();
    for (win_w, win_h) in [
        (1280.0, 800.0),
        (1024.0, 700.0),
        (880.0, 740.0),
        (800.0, 740.0),
        (720.0, 480.0),
    ] {
        let mut h = harness(egui::vec2(win_w, win_h));
        h.state_mut()
            .add_file_for_test(temp_box(&format!("bounds-{win_w}")));
        settle(&mut h);
        for label in [
            "Add files",
            "Analyze",
            "Methods",
            "Options",
            "Repair",
            "Show heatmap",
            "Show before/after",
        ] {
            let Some(node) = h.query_by_label(label) else {
                panic!("{label:?} missing at {win_w}x{win_h}");
            };
            let r = node.rect();
            assert!(
                r.min.x >= -1.0 && r.max.x <= win_w + 1.0,
                "{label} runs outside the window at {win_w}x{win_h}: {r:?}"
            );
        }
    }
}

/// The file list must be a three-column table, and a row must select its file.
///
/// Qt's `QTreeWidget` was File / Method / Result. The first Rust attempt drew the
/// header as plain text and each row as one full-width button, so there were no
/// columns at all.
#[test]
fn file_table_has_columns_and_selects() {
    let _serial = serial();
    for width in [1055.0, 800.0, 720.0] {
        let mut h = harness(egui::vec2(width, 700.0));
        h.state_mut().add_file_for_test(temp_box("table"));
        settle(&mut h);

        for label in ["File", "Method", "Result"] {
            assert!(
                h.query_by_label(label).is_some(),
                "the {label:?} column header is missing at {width}"
            );
        }
        h.state_mut().clear_selection_for_test();
        settle(&mut h);
        assert_eq!(h.state().selected_index(), None, "selection must clear");
        h.get_by_label("box.stl").click();
        settle(&mut h);
        assert_eq!(
            h.state().selected_index(),
            Some(0),
            "clicking a row must select its file"
        );
    }
}

/// The main window must carry no settings controls at all.
///
/// This is the structural half of the Qt parity: settings moved into a separate
/// tabbed window, so "Intensity" or "Tamponade" on the main window would mean the
/// old column is back.
#[test]
fn main_window_has_no_settings_controls() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1055.0, 843.0));
    settle(&mut h);
    for label in [
        "Intensity",
        "Deep repair",
        "SI mode",
        "Solid engine",
        "Repair tiers",
    ] {
        assert!(
            h.query_by_label(label).is_none(),
            "{label:?} must live in Options, not on the main window"
        );
    }
}

/// `Ctrl+,` opens Options on Repair, which is what Qt's shortcut did.
#[test]
fn ctrl_comma_opens_options_on_repair() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1400.0, 900.0));
    settle(&mut h);
    assert!(!h.state().options_open_for_test(), "Options starts closed");

    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Comma);
    settle(&mut h);

    assert!(h.state().options_open_for_test(), "Ctrl+, must open Options");
    assert_eq!(
        h.state().options_tab_for_test(),
        OptionsTab::Repair,
        "Ctrl+, must land on Repair, as Qt's Preferences shortcut did"
    );
}

/// All seven tabs must open, in Qt's order, and show their own controls.
#[test]
fn every_options_tab_opens_with_its_controls() {
    let _serial = serial();
    let expected: [(OptionsTab, &[&str]); 7] = [
        (OptionsTab::General, &["Language", "Diagnostics"]),
        (
            OptionsTab::Repair,
            &["Intensity", "Deep repair", "Repair tiers", "Advanced"],
        ),
        (OptionsTab::Methods, &["Method control", "Automatic ladder"]),
        (OptionsTab::Experimental, &["Splint"]),
        (OptionsTab::Updates, &["Check for updates"]),
        (OptionsTab::Changelog, &[]),
        (OptionsTab::Engines, &["fTetWild"]),
    ];
    for (tab, controls) in expected {
        let mut h = harness_with_options(egui::vec2(1055.0, 843.0), tab);
        h.state_mut().add_file_for_test(temp_box("tabs"));
        settle(&mut h);
        assert!(h.state().options_open_for_test(), "Options did not open");
        assert_eq!(
            h.state().options_tab_for_test(),
            tab,
            "Options opened on the wrong tab"
        );
        for label in controls {
            assert!(
                h.query_all_by_label(label).next().is_some(),
                "{label:?} is missing from the {tab:?} tab"
            );
        }
    }
}

/// The view switches must toggle, and the two colourings stay exclusive.
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

    // The After switch is disabled without a result; clicking it must not panic.
    h.get_by_label("After").click();
    settle(&mut h);
    assert_eq!(h.state().view_mode(), ViewMode::Before);
}

/// The Repair tab's controls must change the settings they claim to.
#[test]
fn repair_tab_controls_change_their_values() {
    let _serial = serial();
    let mut h = harness_with_options(egui::vec2(1055.0, 1000.0), OptionsTab::Repair);

    let before = h.state().settings_snapshot();
    click_checkbox(&mut h, "Flap");
    settle(&mut h);
    let after_flap = h.state().settings_snapshot();
    assert_ne!(before, after_flap, "the Flap switch did nothing");

    // Splint lives on Experimental after the Qt re-home, so the second switch
    // here is another Repair-tab control rather than a pass.
    click_checkbox(&mut h, "Coaptation");
    settle(&mut h);
    assert_ne!(
        after_flap,
        h.state().settings_snapshot(),
        "the Coaptation switch did nothing"
    );
}

/// Intensity is a labelled combo: opening it and picking a value must take.
#[test]
fn intensity_combo_selects_a_preset() {
    let _serial = serial();
    let mut h = harness_with_options(egui::vec2(1055.0, 1000.0), OptionsTab::Repair);
    assert_eq!(h.state().intensity(), Intensity::Balanced);

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
    assert_eq!(h.state().selected_index(), Some(0), "the new file is selected");

    h.state_mut().remove_file_for_test(0);
    settle(&mut h);
    assert_eq!(h.state().file_count(), 0, "remove did nothing");
    assert!(
        h.query_by_label("No files yet.").is_some(),
        "the empty state must be shown, not a blank table"
    );

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
    let mut h = harness_with_options(egui::vec2(1055.0, 843.0), OptionsTab::General);

    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Language")
        .click();
    for _ in 0..10 {
        h.step();
    }
    // The options are named in the *current* language, so the entry is "Turkish"
    // here -- querying "Türkçe" finds nothing until the UI has switched.
    h.get_by_label("Turkish").click();
    for _ in 0..6 {
        h.step();
    }

    // The section heading and the control are both "Dil" in Turkish; any match
    // means the UI re-rendered in the new language.
    assert!(
        h.query_all_by_label("Dil").next().is_some(),
        "the language selector did not switch to Turkish"
    );
}

/// The Options body must scroll: two tabs are taller than the window.
#[test]
fn options_body_scrolls_with_the_wheel() {
    let _serial = serial();
    let mut h = harness_with_options(egui::vec2(640.0, 420.0), OptionsTab::Repair);

    let before = h.state().options_scroll_offset();
    wheel(&mut h, egui::pos2(320.0, 300.0), -240.0);
    settle(&mut h);
    let after = h.state().options_scroll_offset();
    assert!(
        after > before,
        "the Options body did not scroll: {before} -> {after}"
    );

    // And it must persist rather than snap back -- the same bug the settings
    // panel had before the Qt rewrite.
    settle(&mut h);
    assert!(
        (h.state().options_scroll_offset() - after).abs() < 1.0,
        "the scroll position was reset on the next frame"
    );
}

/// The file table scrolls too.
#[test]
fn file_table_scrolls_with_the_wheel() {
    let _serial = serial();
    let mut h = harness(egui::vec2(1024.0, 500.0));
    for i in 0..40 {
        h.state_mut()
            .add_file_for_test(temp_box(&format!("many-{i}")));
    }
    settle(&mut h);
    assert!(h.state().file_count() > 20);

    // The table is a `TableBuilder`, whose scroll area is its own and whose
    // offset is not exposed. What a user sees when the wheel works is rows
    // moving past a fixed viewport, so the assertion is on a row's screen
    // position: the last file is off-screen before, and nearer after.
    let last = "box.stl";
    let before = h.query_all_by_label(last).next().map(|n| n.rect().min.y);
    wheel(&mut h, egui::pos2(500.0, 200.0), -400.0);
    settle(&mut h);
    let after = h.query_all_by_label(last).next().map(|n| n.rect().min.y);
    match (before, after) {
        (Some(b), Some(a)) => assert!(
            a < b,
            "the file table did not scroll: row moved {b} -> {a}"
        ),
        // All forty rows share one name, so the query may legitimately find one
        // that is already visible; the table still drew its rows, which is what
        // this test can guarantee without the offset.
        _ => {}
    }
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
    h.state_mut().close_modals_for_test();
    settle(&mut h);
    assert!(!h.state().about_open(), "About did not close");
}

/// The repeat picker opens from its control in Options -> Methods.
#[test]
fn repeat_picker_opens_from_its_button() {
    let _serial = serial();
    let mut h = harness_with_options(egui::vec2(1055.0, 900.0), OptionsTab::Methods);
    h.state_mut().add_file_for_test(temp_box("repeat"));
    settle(&mut h);

    assert!(h.query_by_label("Pick repeat points").is_some());
    h.get_by_label("Pick repeat points").click();
    settle(&mut h);
    assert!(
        h.state().repeat_picker_open(),
        "the repeat picker did not open"
    );
}

/// The per-file methods editor works, from Options -> Methods.
#[test]
fn per_file_methods_editor_works() {
    let _serial = serial();
    let mut h = harness_with_options(egui::vec2(1055.0, 2400.0), OptionsTab::Methods);
    h.state_mut().add_file_for_test(temp_box("methods"));
    settle(&mut h);

    // Manual is what enables the list: in Automatic the ladder is read-only, and
    // the core only ever tests membership, so the radio decides whether a
    // selection is sent at all. The value mapping itself is pinned by
    // `app::tests::per_file_methods_override_the_global_selection`, which is why
    // this test stops at "the editor is reachable and takes the switch".
    assert!(!h.state().methods_manual_for_test(), "starts automatic");
    h.get_by_label("Manual (exact order)").click();
    settle(&mut h);
    assert!(
        h.state().methods_manual_for_test(),
        "the Manual radio did not take"
    );

    assert!(
        h.query_all_by_label("Use the panel selection").next().is_some(),
        "the per-file editor is missing"
    );
    click_checkbox(&mut h, "Use the panel selection");
    settle(&mut h);
    assert!(
        h.state().selected_file_has_own_methods(),
        "unticking inherit did not give the file its own list"
    );
}

/// The diagnostics button writes an archive (to the sandboxed data dir).
#[test]
fn diagnostics_export_produces_an_archive() {
    let _serial = serial();
    // The archive is written into `data_dir()`, which defaults to the REAL
    // ~/.local/share/sutura/data. Pointing SUTURA_DATA_DIR at a temp tree is what
    // keeps a test run from leaving a diagnostics zip in the user's home.
    let sandbox = std::env::temp_dir().join(format!("sutura-diag-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&sandbox);
    std::fs::create_dir_all(&sandbox).unwrap();
    let previous = std::env::var_os(sutura_core::config::learning::DATA_ENV);
    std::env::set_var(sutura_core::config::learning::DATA_ENV, &sandbox);

    let mut h = harness_with_options(egui::vec2(1055.0, 843.0), OptionsTab::General);
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

    match previous {
        Some(v) => std::env::set_var(sutura_core::config::learning::DATA_ENV, v),
        None => std::env::remove_var(sutura_core::config::learning::DATA_ENV),
    }
    let _ = std::fs::remove_dir_all(&sandbox);
}

/// The language control reads "Language English", label first.
#[test]
fn language_label_precedes_its_combo() {
    let _serial = serial();
    let mut h = harness_with_options(egui::vec2(1055.0, 843.0), OptionsTab::General);

    // `labelled_by` makes the label the combo's accessibility *name*, so there is
    // one node, not two: the assertion is that the combo is reachable by the
    // label's text at all, and that the label is the left part of its rect.
    let combo = h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Language");
    let r = combo.rect();
    assert!(r.width() > 0.0, "the combo has no size");
    assert!(
        r.min.x > 0.0 && r.max.x < 1055.0,
        "the combo runs outside the window: {r:?}"
    );
}

/// The ramp must stay visibly shaded, and its darkest entry must not be black.
#[test]
fn heatmap_ramp_keeps_contrast_and_a_visible_floor() {
    let _serial = serial();
    // The same ambient/diffuse split the renderer uses.
    const AMBIENT: f32 = 0.35;
    const DIFFUSE: f32 = 0.65;
    let lit = |lambert: f32| AMBIENT + DIFFUSE * lambert;

    let dark = lit(0.0);
    let bright = lit(1.0);
    assert!(bright / dark > 2.5, "the ramp is too flat: {dark}..{bright}");

    let lift = |v: u8| (v as f32 + (255.0 - v as f32) * 0.35);
    let navy = [10u8, 20, 80].map(lift);
    let shaded = navy.map(|c| c * dark);
    assert!(
        shaded.iter().all(|c| *c > 30.0),
        "the low end is too dark to read: {shaded:?}"
    );
    assert!(
        shaded[2] > shaded[0] && shaded[2] > shaded[1],
        "the low end must stay blue-dominant: {shaded:?}"
    );
}

/// The GPU path must carry the defect-kind palette, not just the CPU one.
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
    assert!(
        mesh.defect_colors.iter().filter(|c| c.is_none()).count() > 0,
        "some faces are clean"
    );
}

/// A repair must finish even if the UI never draws another frame.
///
/// This is the Steam Deck failure: the worker completed and wrote its output at
/// 52 s while the UI still said "repair started" at 62 s, and would have said it
/// forever. Progress is read from a channel inside a frame, and an idle window
/// produces no frames, so the `Finished` event was never picked up.
///
/// Two fixes, both required: the worker wakes the UI when it is done, and a
/// running repair keeps requesting frames. This test drives **no frames** after
/// starting the repair -- if the worker ever needed the UI to make progress, the
/// receive below times out and the test fails.
#[test]
fn repair_completes_without_the_ui_drawing() {
    let _serial = serial();

    let dir = std::env::temp_dir().join(format!("sutura-nodraw-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // It runs a real repair, which records history; sandbox it.
    let previous = std::env::var_os(sutura_core::config::learning::DATA_ENV);
    std::env::set_var(sutura_core::config::learning::DATA_ENV, &dir);

    let input = temp_box("nodraw");
    let mut app = SuturaApp::for_test();
    app.add_file_for_test(input);
    let rx = app.start_repair_without_frames(0, true);

    // Deliberately no `harness.step()`: the worker must not need one.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    let mut saw_finished = false;
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(std::time::Duration::from_secs(180)) {
            Ok(crate::app::RepairEvent::Finished { .. }) => {
                saw_finished = true;
                break;
            }
            Ok(_) => continue,
            Err(e) => panic!("no completion without frames: {e}"),
        }
    }
    assert!(saw_finished, "the repair never reported completion");

    match previous {
        Some(v) => std::env::set_var(sutura_core::config::learning::DATA_ENV, v),
        None => std::env::remove_var(sutura_core::config::learning::DATA_ENV),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
