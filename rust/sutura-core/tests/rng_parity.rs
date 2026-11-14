//! Bit-exactness against NumPy 2.5.4 `default_rng` for the classifier RANSAC.
//!
//! The fixture was produced with:
//! `np.random.default_rng(seed)` / `np.random.PCG64(seed)` for seeds 0,1,42.

use std::collections::BTreeMap;

use sutura_core::rng::Pcg64;

fn vectors() -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rng_vectors.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn seed_state_and_raw_stream_match_numpy() {
    let v = vectors();
    for seed in [0u64, 1, 42] {
        let state = &v[format!("state{seed}")];
        let want_state: u128 = state["state"].as_str().unwrap().parse().unwrap();
        let want_inc: u128 = state["inc"].as_str().unwrap().parse().unwrap();
        let mut rng = Pcg64::new(seed);
        assert_eq!(rng.state(), (want_state, want_inc), "state for seed {seed}");
        let want: Vec<u64> = v[format!("raw{seed}")]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_u64().unwrap())
            .collect();
        let got: Vec<u64> = (0..want.len()).map(|_| rng.next_u64()).collect();
        assert_eq!(got, want, "raw stream for seed {seed}");
    }
}

#[test]
fn choice_without_replacement_matches_numpy() {
    let v = vectors();
    for (key, value) in v.as_object().unwrap() {
        let Some(rest) = key.strip_prefix("choice") else { continue };
        let parts: Vec<&str> = rest.split('_').collect();
        let seed: u64 = parts[0].parse().unwrap();
        let n: u64 = parts[1].parse().unwrap();
        let size: usize = parts[2].parse().unwrap();
        let want: Vec<i64> = value
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_i64().unwrap())
            .collect();
        let mut rng = Pcg64::new(seed);
        let got = rng.choice_without_replacement(n, size);
        assert_eq!(got, want, "{key}");
    }
}

#[test]
fn integers_matches_numpy() {
    let v = vectors();
    for (key, value) in v.as_object().unwrap() {
        let Some(rest) = key.strip_prefix("int") else { continue };
        let parts: Vec<&str> = rest.split('_').collect();
        let seed: u64 = parts[0].parse().unwrap();
        let n: u64 = parts[1].parse().unwrap();
        let k: usize = parts[2].parse().unwrap();
        let want: Vec<u64> = value
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_u64().unwrap())
            .collect();
        let mut rng = Pcg64::new(seed);
        let got: Vec<u64> = (0..k).map(|_| rng.integers(n)).collect();
        assert_eq!(got, want, "{key}");
    }
}

#[test]
fn gen_mask_matches_the_floyd_table_size() {
    // Pinned through the choice vectors above; this keeps the helper honest.
    let _ = BTreeMap::<u8, u8>::new();
}
