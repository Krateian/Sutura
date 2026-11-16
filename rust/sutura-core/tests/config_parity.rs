//! Cross-language parity tests for `config` (policy, intensity, learning).
//!
//! The hash, the `--list-intensities` text and the learning bonus are Python
//! byte/semantic contracts; these tests pin the expected values captured from
//! `sutura_engine.policy` / `triage` / `learning` on the same fixture set.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// `SUTURA_POLICY_USER` is process-global, so the override tests serialize.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A poisoned lock only means an earlier test failed while holding it; the
/// tests still run one at a time, so the guard is still exclusive.
fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    match ENV_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

use sutura_core::config::intensity::{
    format_intensities, presets, resolve_intensity, triage_report_fields, IntensitySpec,
    ProfileEntry,
};
use sutura_core::config::learning::{
    learning_bonus, record_learning, triage_stats, LearningKnobs, MethodTrial,
};
use sutura_core::config::policy::{default_policy, load_policy, parse_toml, python_float_repr};

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn default_policy_hash_matches_python() {
    let loaded = default_policy().expect("packaged policy");
    assert_eq!(loaded.schema_version, 1);
    assert_eq!(loaded.content_hash, "abb52584b7c6eee4");
    assert!(loaded.policy_error.is_none());
    assert!(loaded.ignored_keys.is_empty());
}

#[test]
fn python_float_repr_matches_python_json() {
    // Values taken from Python's json.dumps of the policy document.
    let cases = [
        (0.5, "0.5"),
        (0.005, "0.005"),
        (0.0125, "0.0125"),
        (0.0001, "0.0001"),
        (0.00001, "1e-05"),
        (1e-9, "1e-09"),
        (30.0, "30.0"),
        (3600.0, "3600.0"),
        (10000.0, "10000.0"),
        (1000000.0, "1000000.0"),
        (1.5, "1.5"),
        (0.75, "0.75"),
    ];
    for (value, expected) in cases {
        assert_eq!(python_float_repr(value), expected, "repr of {value}");
    }
}

fn fixture_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("policy")
}

fn user_policy(name: &str) -> std::path::PathBuf {
    fixture_dir().join(format!("{name}.toml"))
}

#[test]
fn user_override_hashes_match_python() {
    // (fixture, expected hash, expected ignored keys) captured from
    // sutura_engine.policy with SUTURA_POLICY_USER pointing at the same file.
    let cases: [(&str, &str, &[&str]); 4] = [
        (
            "order_swap",
            "083522b13ed79e27",
            &[],
        ),
        ("tier_tweak", "8637b5438a95c9d4", &[]),
        (
            "locked_gate",
            "abb52584b7c6eee4",
            &["auto.generative_max_hausdorff_rel", "gates.adoption", "si_escalation"],
        ),
        ("unknown_key", "591d4625a31def43", &["ranking.nope"]),
    ];
    let _guard = env_guard();
    for (name, hash, ignored) in cases {
        std::env::set_var("SUTURA_POLICY_USER", user_policy(name));
        let merged = load_policy(None).expect("merged policy");
        let mut got = merged.ignored_keys.clone();
        got.sort();
        let mut want: Vec<String> = ignored.iter().map(|s| (*s).to_string()).collect();
        want.sort();
        assert_eq!(merged.content_hash, hash, "hash for {name}");
        assert_eq!(got, want, "ignored keys for {name}");
        assert!(merged.user_override, "{name} must report the override");
    }
    std::env::remove_var("SUTURA_POLICY_USER");
}

#[test]
fn malformed_user_policy_falls_back_with_an_error() {
    let _guard = env_guard();
    let broken = fixture_dir().join("broken.toml");
    std::env::set_var("SUTURA_POLICY_USER", &broken);
    let merged = load_policy(None).expect("packaged default");
    std::env::remove_var("SUTURA_POLICY_USER");
    assert_eq!(merged.content_hash, "abb52584b7c6eee4");
    // Python marks the override as soon as the user file exists, then falls
    // back to the packaged default with a recorded error.
    assert!(merged.user_override);
    let error = merged.policy_error.expect("recorded policy error");
    assert!(error.starts_with("TomlDecodeError"), "{error}");
}

#[test]
fn explicit_path_skips_the_user_override() {
    let loaded = load_policy(Some(&user_policy("tier_tweak"))).unwrap_err();
    assert!(loaded.0.contains("missing section"), "{}", loaded.0);
}

#[test]
fn overrides_outside_the_base_schema_are_ignored() {
    let _guard = env_guard();
    let doc = parse_toml("[ranking]\ntop_n = 4\n[mystery]\nx = 1\n").expect("toml");
    let _ = doc;
    std::env::set_var("SUTURA_POLICY_USER", user_policy("unknown_key"));
    let merged = load_policy(None).unwrap();
    std::env::remove_var("SUTURA_POLICY_USER");
    // `modes.low.mincomponentsize` is a known key, so it survives; only the
    // unknown `ranking.nope` is dropped.
    assert_eq!(merged.ignored_keys, vec!["ranking.nope".to_string()]);
    let modes = merged.get("modes").unwrap().get("low").unwrap();
    assert_eq!(modes.get("mincomponentsize").unwrap().as_i64(), Some(9));
}

#[test]
fn presets_match_the_python_preset_table() {
    let loaded = default_policy().unwrap();
    let table = presets(&loaded);
    let quick = table.get("quick").unwrap();
    assert!(!quick.ftetwild_enabled);
    assert_eq!(quick.ftetwild_max_faces, None);
    assert_eq!(quick.deep_repair, "off");
    assert!(quick.dense_target_ladder.is_empty());

    let balanced = table.get("balanced").unwrap();
    assert_eq!(balanced.deep_repair, "full");
    assert_eq!(balanced.ftetwild_timeout, 180.0);

    let thorough = table.get("thorough").unwrap();
    assert_eq!(thorough.ftetwild_timeout, 600.0);
    assert_eq!(thorough.graft_timeout, 900.0);
    assert_eq!(thorough.ftetwild_hausdorff_samples, 400_000);

    let extreme = table.get("extreme").unwrap();
    assert_eq!(extreme.ftetwild_max_faces, None);
    assert_eq!(extreme.graft_max_faces, None);
    assert!(extreme.ftetwild_optimize_retry_on_dense_fail);
    assert_eq!(extreme.ftetwild_hausdorff_samples, 1_000_000);
}

#[test]
fn intensity_priority_is_cli_then_env_then_config() {
    let loaded = default_policy().unwrap();
    let profiles = BTreeMap::new();
    let config = fixture_dir().join("config_thorough.json");
    let environment = env(&[("SUTURA_INTENSITY", "quick")]);

    // CLI wins over env and config.
    let cli = resolve_intensity(
        &loaded,
        Some("extreme"),
        Some(&environment),
        Some(&config),
        Some(&profiles),
        None,
    );
    assert_eq!(cli.name, "extreme");

    // Env wins over config.
    let from_env = resolve_intensity(
        &loaded,
        None,
        Some(&environment),
        Some(&config),
        Some(&profiles),
        None,
    );
    assert_eq!(from_env.name, "quick");

    // Config used when CLI and env are absent.
    let from_config = resolve_intensity(
        &loaded,
        None,
        Some(&BTreeMap::new()),
        Some(&config),
        Some(&profiles),
        None,
    );
    assert_eq!(from_config.name, "thorough");

    // Unknown values everywhere fall back to the policy default.
    let fallback = resolve_intensity(
        &loaded,
        Some("does-not-exist"),
        Some(&environment),
        Some(&config),
        Some(&profiles),
        None,
    );
    assert_eq!(fallback.name, "quick");
}

#[test]
fn unknown_intensity_without_other_sources_falls_back_to_balanced() {
    let loaded = default_policy().unwrap();
    let spec = resolve_intensity(
        &loaded,
        Some("nope"),
        Some(&BTreeMap::new()),
        Some(std::path::Path::new("/nonexistent/config.json")),
        Some(&BTreeMap::new()),
        None,
    );
    assert_eq!(spec.name, "balanced");
}

#[test]
fn list_intensities_text_matches_python_byte_for_byte() {
    let loaded = default_policy().unwrap();
    let expected = concat!(
        "Intensity presets (built-in, read-only):\n",
        "  quick                ftetwild=off deep_repair=off ladder=- ratio=4 min_faces=20000 hausdorff=200000 graft(cap=2000000,timeout=300s)\n",
        "  balanced             ftetwild=on(cap=300000,timeout=180s,optretry=no) deep_repair=full ladder=1.5,3.0 ratio=4 min_faces=20000 hausdorff=200000 graft(cap=2000000,timeout=300s)\n",
        "  thorough             ftetwild=on(cap=300000,timeout=600s,optretry=no) deep_repair=full ladder=1.5,3.0,threshold ratio=4 min_faces=20000 hausdorff=400000 graft(cap=2000000,timeout=900s)\n",
        "  extreme              ftetwild=on(cap=no-limit,timeout=1800s,optretry=yes) deep_repair=full ladder=1.5,3.0,threshold ratio=4 min_faces=20000 hausdorff=1000000 graft(cap=no-limit,timeout=1800s)\n",
        "User profiles (base + effective values):\n",
        "  (none)",
    );
    assert_eq!(format_intensities(&loaded, Some(&BTreeMap::new())), expected);
}

#[test]
fn user_profile_derives_from_its_base() {
    let loaded = default_policy().unwrap();
    let mut overrides = BTreeMap::new();
    overrides.insert(
        "ftetwild_timeout".to_string(),
        sutura_core::config::PolicyValue::Float(42.0),
    );
    overrides.insert(
        "ftetwild_hausdorff_samples".to_string(),
        sutura_core::config::PolicyValue::Int(1),
    );
    let mut profiles = BTreeMap::new();
    profiles.insert(
        "tiny".to_string(),
        ProfileEntry {
            base: "quick".to_string(),
            overrides,
        },
    );
    let spec = resolve_intensity(
        &loaded,
        Some("tiny"),
        Some(&BTreeMap::new()),
        Some(std::path::Path::new("/nonexistent/config.json")),
        Some(&profiles),
        None,
    );
    assert_eq!(spec.name, "tiny");
    assert_eq!(spec.base.as_deref(), Some("quick"));
    assert_eq!(spec.ftetwild_timeout, 42.0);
    // The Hausdorff floor clamps a too-small sample count.
    assert_eq!(spec.ftetwild_hausdorff_samples, 200_000);
    assert!(!spec.ftetwild_enabled);
}

#[test]
fn report_fields_are_the_triage_intensity_block() {
    let spec = IntensitySpec {
        name: "custom".to_string(),
        base: Some("balanced".to_string()),
        ..IntensitySpec::default()
    };
    let fields = triage_report_fields(Some(&spec));
    assert_eq!(
        fields.get("triage_intensity").unwrap().as_str(),
        Some("custom")
    );
    assert_eq!(
        fields.get("triage_profile_base").unwrap().as_str(),
        Some("balanced")
    );
    let default_fields = triage_report_fields(None);
    assert_eq!(
        default_fields.get("triage_intensity").unwrap().as_str(),
        Some("balanced")
    );
}

fn learning_fixture() -> std::path::PathBuf {
    fixture_dir().join("triage_learning.json")
}

#[test]
fn learning_bonus_matches_python_within_1e9() {
    // Captured from sutura_engine.learning.learning_bonus on the fixture with
    // the same knobs and no classifier version stamp.
    let stats = triage_stats(None, Some(&learning_fixture()));
    let knobs = LearningKnobs::default();
    let cases = [
        ("mechanical", "1", 0.02),
        ("mechanical", "2", 0.0),
        ("mechanical", "13", 0.0),
        ("organic", "1", 0.016667),
        ("organic", "7", 0.016667),
        ("_none_", "9", 0.005),
        ("nowhere", "1", 0.0),
        ("mechanical", "", 0.0),
    ];
    for (template, method, expected) in cases {
        let got = learning_bonus(Some(template), Some(method), Some(&stats), None, knobs);
        assert!(
            (got - expected).abs() <= 1e-9,
            "{template}/{method}: {got} != {expected}"
        );
    }
}

#[test]
fn learning_bonus_rewards_a_successful_method() {
    let path = std::env::temp_dir().join(format!(
        "sutura-learning-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let trials = vec![
        MethodTrial {
            id: 3,
            outcome: "accepted".to_string(),
            template: Some("mechanical".to_string()),
            elapsed_ms: Some(100.0),
        },
        MethodTrial {
            id: 3,
            outcome: "accepted".to_string(),
            template: Some("mechanical".to_string()),
            elapsed_ms: Some(100.0),
        },
        MethodTrial {
            id: 3,
            outcome: "rejected".to_string(),
            template: Some("mechanical".to_string()),
            elapsed_ms: Some(100.0),
        },
    ];
    record_learning(&trials, None, Some(&path)).expect("record");
    let stats = triage_stats(None, Some(&path));
    let knobs = LearningKnobs::default();
    let bonus = learning_bonus(Some("mechanical"), Some("3"), Some(&stats), None, knobs);
    // Beta(2,2) posterior with 2/3 success is 0.5714...; the speed factor is 1
    // (single bucket = the median), so the bonus is bounded by max_bonus.
    assert!(bonus > 0.0 && bonus <= knobs.max_bonus, "{bonus}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn other_version_buckets_are_not_counted() {
    let path = std::env::temp_dir().join(format!(
        "sutura-learning-cv-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(
        &path,
        r#"{"schema_version":2,"templates":{"mechanical":{"1":{"n":4,"ok":4,"total_ms":40,"cv":1}}}}"#,
    )
    .unwrap();
    let stats = triage_stats(Some(2), Some(&path));
    let bonus = learning_bonus(
        Some("mechanical"),
        Some("1"),
        Some(&stats),
        Some(2),
        LearningKnobs::default(),
    );
    assert_eq!(bonus, 0.0);
    let _ = std::fs::remove_file(&path);
}
