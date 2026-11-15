// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Runtime configuration: repair policy, intensity presets and learning.
//!
//! Mirrors the stdlib-only Python modules `sutura_engine.policy`,
//! `sutura_engine.triage` (preset/profile part) and `sutura_engine.learning`.
//! None of them may depend on pyo3, numpy or a geometry crate.

pub mod intensity;
pub mod learning;
pub mod policy;

pub use intensity::{
    build_spec, effective_spec, format_intensities, list_intensities, load_profiles,
    name_error, normalize_entry, preset_from_policy, presets, profile_overrides,
    resolve_intensity, resolve_intensity_detail, save_profiles, triage_report_fields,
    unique_profile_name, IntensitySpec, LadderEntry, ProfileEntry, DEFAULT_INTENSITY,
    DEEP_REPAIR_MODES, HAUSDORFF_FLOOR, INTENSITIES, INTENSITY_ENV,
    PROFILES_SCHEMA_VERSION, PROFILE_FIELDS,
};
pub use learning::{
    clear_learning, data_dir, data_file, learning_bonus, learning_enabled, learning_path,
    load_learning, record_learning, round6, triage_stats, LearningKnobs, MethodTrial,
};
pub use policy::{
    default_policy, default_policy_path, load_policy, parse_toml, policy_hash,
    python_float_repr, user_policy_path, validate, LoadedPolicy, PolicyError, PolicyValue,
    DEFAULT_POLICY_TOML, SCHEMA_VERSION, USER_POLICY_ENV,
};
