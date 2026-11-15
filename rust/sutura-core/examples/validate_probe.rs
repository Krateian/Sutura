use sutura_core::config::policy::load_policy;
use sutura_core::validate::{dry_run_mesh, validate_mesh};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("mesh path");
    let engine = args.next().unwrap_or_else(|| "experimental".into());
    let si_mode = args.next().unwrap_or_else(|| "auto".into());
    let mesh = sutura_core::load_mesh(&path).expect("load mesh");
    let loaded = load_policy(None).expect("policy");
    let kind = std::env::var("PROBE_KIND").unwrap_or_else(|_| "validate".into());
    let result = if kind == "dry-run" {
        dry_run_mesh(&mesh, &loaded, "auto", None, &engine, None, None, &si_mode, true)
    } else {
        validate_mesh(&mesh, &loaded, &engine, None, &si_mode)
    };
    match result {
        Ok(mut v) => {
            let text = serde_json::to_string(&v).unwrap();
            v = serde_json::from_str(&text).unwrap();
            println!("{}", serde_json::to_string(&v).unwrap());
        }
        Err(e) => {
            println!("{}", serde_json::json!({"error": e}));
            std::process::exit(1);
        }
    }
}
