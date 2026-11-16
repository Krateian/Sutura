mod app;
mod i18n;
mod mesh;
mod viewer;

rust_i18n::i18n!("locales", fallback = "en");

fn main() -> eframe::Result<()> {
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
    let mut heatmap = false;
    let mut about = false;
    let mut updater = false;
    let mut intensity = app::Intensity::Balanced;
    let mut language = i18n::Language::System;
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
            "--heatmap" => heatmap = true,
            "--about" => about = true,
            "--updates" => updater = true,
            "--intensity" => {
                intensity = match args.next().as_deref() {
                    Some("quick") => app::Intensity::Quick,
                    Some("thorough") => app::Intensity::Thorough,
                    Some("extreme") => app::Intensity::Extreme,
                    _ => app::Intensity::Balanced,
                }
            }
            "--lang" => {
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
                    about,
                    updater,
                    intensity,
                },
            )))
        }),
    )
}
