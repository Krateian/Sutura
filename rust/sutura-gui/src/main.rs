mod app;
mod i18n;
mod mesh;
mod repeat_picker;
mod state;
mod updater_ui;
mod viewer;
mod crash;

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
            "--defect-colors" => open = app::OpenOnStart::DefectColors,
            "--select" => select = args.next().and_then(|s| s.parse::<usize>().ok()),
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
        viewport: egui::ViewportBuilder::default()
            .with_title(rust_i18n::t!("app.title").to_string())
            .with_inner_size([1024.0, 720.0])
            .with_min_inner_size([720.0, 480.0])
            .with_drag_and_drop(true),
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
                },
            )))
        }),
    )
}
