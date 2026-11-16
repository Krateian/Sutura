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
