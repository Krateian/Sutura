mod app;
mod i18n;
mod mesh;
mod options;
mod repeat_picker;
mod slicers;
mod state;
mod suggestions;
mod theme;
mod updater_ui;
mod viewer;
mod crash;
#[cfg(test)]
mod interaction_tests;
#[cfg(test)]
mod design_frames;

rust_i18n::i18n!("locales", fallback = "en");

fn main() -> eframe::Result<()> {
    // First thing: anything that panics from here on leaves a trace, including
    // a failure while parsing arguments or creating the window.
    crash::install_hooks();

    let mut args = std::env::args().skip(1);
    let mut files = Vec::new();
    let mut screenshot = None;
    let mut exit_after = false;
    let mut auto_repair = false;
    let mut auto_batch = false;
    let mut headless = false;
    let mut headless_out: Option<std::path::PathBuf> = None;
    let mut headless_report: Option<std::path::PathBuf> = None;
    let mut headless_cancel_ms: Option<u64> = None;
    let mut headless_analyze: Option<std::path::PathBuf> = None;
    let mut headless_objects: Option<std::path::PathBuf> = None;
    // Pixels to pick in headless mode: `--headless-pick X,Y` (repeatable).
    let mut headless_picks: Vec<(i64, i64)> = Vec::new();
    // Headless heatmap dump: the metric values, for the parity gate.
    let mut headless_heatmap = false;
    let mut heatmap_ref: Option<std::path::PathBuf> = None;
    let mut heatmap = false;
    // Test hook: force the After view's deviation reference (ramp demo).
    let mut deviation_reference: Option<std::path::PathBuf> = None;
    // Headless score dump: the Report panel's values, for CLI comparison.
    let mut headless_scores = false;
    let mut about = false;
    let mut updater = false;
    // Views a screenshot needs to reach, since `--screenshot` runs with no input
    // device and nothing can be clicked.
    let mut open = app::OpenOnStart::None;
    let mut select: Option<usize> = None;
    let mut settings_scroll = 0.0f32;
    let mut wait_frames = 3u8;
    // `[x,y,z]` centre, optionally `[x,y,z,zoom]`: see `--focus`.
    let mut focus: Option<([f32; 3], Option<f32>)> = None;
    let mut options_tab: Option<app::OptionsTab> = None;
    let mut options_scroll: Option<f32> = None;
    let mut methods_manual: Option<String> = None;
    let mut file_methods: Option<Vec<u8>> = None;
    // Window size, so a capture can prove the layout at the small end (Steam
    // Deck is 1280x800, the stated minimum 720x480). Not in `--help`: it exists
    // for scripted captures and for reproducing a layout report.
    let mut window_size: Option<[f32; 2]> = None;
    let mut intensity = app::Intensity::Balanced;
    let mut intensity_explicit = false;
    let mut language = i18n::Language::System;
    let mut language_explicit = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--screenshot" => screenshot = args.next().map(std::path::PathBuf::from),
            "--exit-after" => exit_after = true,
            "--repair" => auto_repair = true,
            "--repair-all" => auto_batch = true,
            // Hidden test hook: run the repair path with no window, so the parity
            // gate can compare this binary's output to the CLI's. Deliberately
            // absent from `--help` -- it is not a user-facing feature.
            "--headless-repair" => {
                headless = true;
                headless_out = args.next().map(std::path::PathBuf::from);
            }
            "--headless-report" => {
                headless_report = args.next().map(std::path::PathBuf::from);
            }
            // Hidden measurement hook: cancel the headless run after N ms, so the
            // cooperative-cancel latency can be measured rather than assumed.
            "--headless-cancel-after-ms" => {
                headless_cancel_ms = args.next().and_then(|s| s.parse::<u64>().ok());
            }
            // Hidden test hooks, absent from `--help`: they run the same code the
            // UI uses, without a window, so the gate can compare against the CLI.
            "--headless-analyze" => {
                headless_analyze = args.next().map(std::path::PathBuf::from);
            }
            "--headless-objects" => {
                headless_objects = args.next().map(std::path::PathBuf::from);
            }
            // Hidden gate hook: click these pixels through the picker and print
            // the resulting points, so Python's picker can be compared exactly.
            // Distinct from `--headless-report` (the repair path above): this
            // one dumps the panel values AFTER scoring, for the parity check.
            "--headless-scores" => headless_scores = true,
            "--headless-heatmap" => headless_heatmap = true,
            "--reference" => heatmap_ref = args.next().map(std::path::PathBuf::from),
            "--deviation-reference" => {
                deviation_reference = args.next().map(std::path::PathBuf::from)
            }
            "--headless-pick" => {
                if let Some(spec) = args.next() {
                    let mut it = spec.split(',');
                    if let (Some(x), Some(y)) = (it.next(), it.next()) {
                        if let (Ok(x), Ok(y)) = (x.trim().parse::<i64>(), y.trim().parse::<i64>()) {
                            headless_picks.push((x, y));
                        }
                    }
                }
            }
            "--heatmap" => heatmap = true,
            "--about" => about = true,
            "--updates" => updater = true,
            "--open-compare" => open = app::OpenOnStart::Compare,
            "--open-repeat-picker" => open = app::OpenOnStart::RepeatPicker,
            "--open-changelog" => open = app::OpenOnStart::Changelog,
            // Capture hook: open the Options window on one tab, so each of the
            // seven can be photographed without a click.
            // Capture hook: start Methods on Manual with this sequence, so the
            // Up/Down rows can be photographed.
            "--methods-manual" => methods_manual = args.next(),
            // Capture hook: seed the Options body's scroll, so a frame can show
            // a control that starts below the fold.
            "--options-scroll" => {
                options_scroll = args.next().and_then(|s| s.parse::<f32>().ok())
            }
            "--open-options" => {
                let tab = args.next();
                options_tab = Some(match tab.as_deref() {
                    Some("general") => app::OptionsTab::General,
                    Some("repair") => app::OptionsTab::Repair,
                    Some("methods") => app::OptionsTab::Methods,
                    Some("experimental") => app::OptionsTab::Experimental,
                    Some("updates") => app::OptionsTab::Updates,
                    Some("changelog") => app::OptionsTab::Changelog,
                    Some("engines") => app::OptionsTab::Engines,
                    other => {
                        eprintln!("--open-options: unknown tab {other:?}");
                        app::OptionsTab::Repair
                    }
                });
            }
            "--show-after" => open = app::OpenOnStart::After,
            "--defect-colors" => open = app::OpenOnStart::DefectColors,
            // Hidden capture hook: point the camera at a box centre and zoom in,
            // so a frame can show a defect that is invisible in a whole-part view.
            "--focus" => {
                focus = args.next().and_then(|spec| {
                    let nums: Vec<f32> = spec
                        .split(',')
                        .filter_map(|p| p.trim().parse::<f32>().ok())
                        .collect();
                    match nums.len() {
                        3 => Some(([nums[0], nums[1], nums[2]], None)),
                        4 => Some(([nums[0], nums[1], nums[2]], Some(nums[3]))),
                        _ => None,
                    }
                })
            }
            "--select" => select = args.next().and_then(|s| s.parse::<usize>().ok()),
            "--file-methods" => {
                file_methods = args.next().map(|spec| {
                    spec.split(',')
                        .filter_map(|t| t.trim().parse::<u8>().ok())
                        .collect()
                })
            }
            "--window-size" => {
                window_size = args.next().and_then(|spec| {
                    let mut it = spec.split('x');
                    match (it.next(), it.next()) {
                        (Some(w), Some(h)) => {
                            match (w.trim().parse::<f32>(), h.trim().parse::<f32>()) {
                                (Ok(w), Ok(h)) => Some([w, h]),
                                _ => None,
                            }
                        }
                        _ => None,
                    }
                })
            }
            "--wait-frames" => {
                wait_frames = args.next().and_then(|s| s.parse().ok()).unwrap_or(3)
            }
            "--settings-scroll" => {
                settings_scroll = args
                    .next()
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(0.0)
            }
            "--intensity" => {
                intensity_explicit = true;
                intensity = match args.next().as_deref() {
                    Some("quick") => app::Intensity::Quick,
                    Some("thorough") => app::Intensity::Thorough,
                    Some("extreme") => app::Intensity::Extreme,
                    _ => app::Intensity::Balanced,
                }
            }
            "--lang" => {
                language_explicit = true;
                language = match args.next().as_deref() {
                    Some("en") => i18n::Language::En,
                    Some("tr") => i18n::Language::Tr,
                    _ => i18n::Language::System,
                }
            }
            "--version" | "-V" => {
                println!("sutura-gui {}", app::SUTURA_VERSION);
                return Ok(());
            }
            "--help" | "-h" => {
                println!("sutura-gui [--lang en|tr|system] [--intensity quick|balanced|thorough|extreme] [--repair|--repair-all] [--heatmap] [--about] [--updates] [--screenshot PNG --exit-after] [FILES...]");
                return Ok(());
            }
            other => files.push(std::path::PathBuf::from(other)),
        }
    }
    i18n::apply(language);

    // Hidden headless modes (not in --help): run the UI's own analysis and object
    // listing without a window so the parity gate can compare them to the CLI.
    if headless_scores {
        let input = match files.first() {
            Some(p) => p.clone(),
            None => {
                eprintln!("--headless-scores needs an input file");
                std::process::exit(2);
            }
        };
        return app::headless_report(&input, None);
    }
    if headless_heatmap {
        let input = match files.first() {
            Some(p) => p.clone(),
            None => {
                eprintln!("--headless-heatmap needs an input file");
                std::process::exit(2);
            }
        };
        return app::headless_heatmap(&input, heatmap_ref.as_deref());
    }
    if !headless_picks.is_empty() {
        let input = match files.first() {
            Some(p) => p.clone(),
            None => {
                eprintln!("--headless-pick needs an input file");
                std::process::exit(2);
            }
        };
        return app::headless_pick(&input, &headless_picks);
    }
    if headless_analyze.is_some() || headless_objects.is_some() {
        let input = match files.first() {
            Some(p) => p.clone(),
            None => {
                eprintln!("a headless mode needs an input file");
                std::process::exit(2);
            }
        };
        if let Some(path) = headless_analyze {
            return app::headless_analyze(&input, Some(path));
        }
        return app::headless_objects(&input, headless_objects);
    }

    if headless {
        let input = match files.first() {
            Some(p) => p.clone(),
            None => {
                eprintln!("--headless-repair needs an input file");
                std::process::exit(2);
            }
        };
        let out = headless_out.unwrap_or_else(|| app::fixed_path(&input));
        return app::headless_repair(&input, &out, intensity, headless_report, headless_cancel_ms);
    }

    let options = eframe::NativeOptions {
        viewport: {
            let mut vp = egui::ViewportBuilder::default()
                .with_title(rust_i18n::t!("app.title").to_string())
                .with_min_inner_size([720.0, 480.0])
                .with_drag_and_drop(true);
            // macOS: one header, not two. The title bar is transparent and the
            // content runs under it, so the traffic lights sit inside the app's
            // own header row (the app leaves room for them and drags the window
            // from that row). Other platforms keep their native decorations.
            #[cfg(target_os = "macos")]
            {
                vp = vp
                    .with_fullsize_content_view(true)
                    .with_titlebar_shown(false)
                    .with_title_shown(false);
            }
            // Size at startup, in order of preference: an explicit --window-size,
            // the size this window last had, then maximized.
            //
            // "90% of the monitor" is not an option on Wayland: egui reports
            // `monitor_size` as None there, so the window used to open at the
            // 1024x720 default on a Steam Deck. Maximizing is the one request every
            // window manager honours, and the resulting size is what gets saved, so
            // later launches restore it directly.
            let saved = crate::state::load().window_size;
            match window_size.or(saved) {
                Some([w, h]) => {
                    vp = vp.with_inner_size([w, h]);
                    eprintln!(
                        "window: opening at {w:.0}x{h:.0} ({})",
                        if window_size.is_some() { "--window-size" } else { "saved" }
                    );
                }
                None if screenshot.is_none() => {
                    vp = vp.with_maximized(true);
                    eprintln!("window: opening maximized (no saved size)");
                }
                None => {
                    vp = vp.with_inner_size([1024.0, 720.0]);
                    eprintln!("window: opening at 1024x720 (capture default)");
                }
            }
            if screenshot.is_some() && window_size.is_none() {
                vp = vp.with_inner_size([1024.0, 720.0]);
            }
            if screenshot.is_some() {
                // A scripted capture must not flash a window over whatever the
                // user is doing. eframe still creates a native window -- there is
                // no way around that without a different backend -- but it is
                // placed off-screen and closed after a few frames.
                vp = vp.with_position([-20000.0, -20000.0]);
            }
            vp
        },
        ..Default::default()
    };
    eframe::run_native(
        "sutura-gui",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::SuturaApp::new(
                cc,
                files,
                language,
                app::StartupOptions {
                    screenshot_path: screenshot,
                    exit_after_screenshot: exit_after,
                    auto_repair,
                    auto_batch,
                    heatmap,
        deviation_reference,
                    about,
                    updater,
                    intensity,
                    intensity_explicit,
                    language_explicit,
                    open,
                    select,
                    settings_scroll,
                    wait_frames,
                    focus,
                    options_tab,
                    options_scroll,
                    methods_manual,
                    file_methods,
                },
            )))
        }),
    )
}
