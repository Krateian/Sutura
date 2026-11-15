// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Human report rendering (`--human`) and the Turkish post-processor.
//!
//! Mirrors `sutura_engine/pipeline/report.py::human_report` and
//! `sutura/report_lang.py` for the fields the Rust pipeline produces. The
//! machine-readable JSON is never localised.

use serde_json::{Map, Value};

fn get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key)
}

fn int_of(value: Option<&Value>) -> i64 {
    value
        .and_then(Value::as_i64)
        .or_else(|| value.and_then(Value::as_f64).map(|x| x as i64))
        .unwrap_or(0)
}

fn num_of(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64)
}

fn display(value: &Value) -> String {
    match value {
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Null => "None".to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn number(value: f64) -> String {
    serde_json::Number::from_f64(value)
        .map(|n| n.to_string())
        .unwrap_or_else(|| value.to_string())
}

struct Brief {
    verdict: &'static str,
    holes_closed: i64,
    orientation_corrected: bool,
    parts_before: Option<i64>,
    parts_after: Option<i64>,
    shape_change_pct: Option<f64>,
    shape_source: Option<&'static str>,
    notes: Vec<&'static str>,
    error: Option<String>,
}

fn fixed_code(report: &Value, code: &str) -> i64 {
    report
        .get("print_report")
        .and_then(|p| p.get("fixed"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .find(|f| f.get("code").and_then(Value::as_str) == Some(code))
                .and_then(|f| f.get("count"))
                .and_then(Value::as_i64)
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

fn classify(report: &Value) -> (String, Vec<String>) {
    let (category, issues, _key) = sutura_core::report::classify_value(report);
    (category, issues)
}

fn brief(report: &Value) -> Brief {
    let (category, issues) = classify(report);
    let s1 = get(report, "stage1").cloned().unwrap_or(Value::Null);
    let pr = get(report, "print_report").cloned().unwrap_or(Value::Null);
    let geom = get(&pr, "geometry").cloned().unwrap_or(Value::Null);

    let mut holes = fixed_code(report, "holes_closed");
    if holes == 0 {
        holes = int_of(get(&s1, "holes_closed"));
    }
    let mut orientation = fixed_code(report, "orientation_corrected") != 0;
    if !orientation {
        if let (Some(vb), Some(va)) = (
            num_of(get(&s1, "volume_before")),
            num_of(get(&s1, "volume_after")),
        ) {
            orientation = vb < 0.0 && va > 0.0;
        }
    }
    let parts_before = num_of(get(&geom, "components_before"))
        .or_else(|| num_of(get(&s1, "components_before")))
        .map(|v| v as i64);
    let parts_after = num_of(get(&geom, "components_after"))
        .or_else(|| num_of(get(&s1, "components")))
        .map(|v| v as i64);
    let mut shape = num_of(get(&geom, "deviation_max_pct"));
    let mut shape_source = shape.map(|_| "deviation");
    if shape.is_none() {
        shape = num_of(get(&geom, "volume_change_pct"));
        if shape.is_some() {
            shape_source = Some("volume");
        }
    }

    let mut notes: Vec<&'static str> = Vec::new();
    for code in &issues {
        let note = match code.as_str() {
            "stage2_skipped" => "stage2_skipped",
            "stage2_error" => "stage2_error",
            "shape_changed" => "shape_changed",
            "volume_warning" => "volume_warning",
            "graft_detail_loss" => "graft_detail_loss",
            "dressing_detail_loss" => "dressing_detail_loss",
            "reload_non_manifold_vertices" => "reload_non_manifold_vertices",
            _ => continue,
        };
        if !notes.contains(&note) {
            notes.push(note);
        }
    }
    if get(report, "unit_warning")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        notes.push("unit_warning");
    }
    if get(report, "material_discarded")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        notes.push("material_discarded");
    }

    let output_missing = get(report, "output")
        .and_then(Value::as_str)
        .map(|s| s.is_empty())
        .unwrap_or(true);
    let failed = category == "error"
        || matches!(
            sutura_core::report::classify_value(report).2.as_str(),
            "partial" | "holes"
        )
        || output_missing;
    let verdict = if failed {
        let holes_left = pr
            .get("issues")
            .and_then(Value::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .find(|i| i.get("code").and_then(Value::as_str) == Some("holes_remaining"))
            })
            .and_then(|i| i.get("count"))
            .and_then(Value::as_i64)
            .filter(|n| *n > 0)
            .unwrap_or_else(|| int_of(get(&s1, "holes_remaining")));
        if holes_left > 0 {
            notes.push("holes_remaining");
        } else if category != "error" {
            notes.push("not_closed");
        }
        "failed"
    } else if category == "watertight" && issues.is_empty() && notes.is_empty() {
        "ready"
    } else {
        "notes"
    };

    Brief {
        verdict,
        holes_closed: holes,
        orientation_corrected: orientation,
        parts_before,
        parts_after,
        shape_change_pct: shape,
        shape_source,
        notes,
        error: get(report, "error")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn headline(report: &Value, brief: &Brief) -> (String, i64) {
    let code = get(report, "repair_verdict_code")
        .and_then(Value::as_str)
        .unwrap_or("");
    let areas = int_of(get(report, "repair_verdict_areas"));
    if brief.verdict == "failed" || code == "not_repaired" {
        return ("not_repaired".to_string(), areas);
    }
    if code.is_empty() || code == "unavailable" {
        return (brief.verdict.to_string(), 0);
    }
    let health = num_of(get(report, "repair_health"));
    let low_health = health.map(|h| h < 60.0).unwrap_or(false);
    if code == "preserved" && (brief.verdict == "notes" || low_health) {
        return ("preserved_notes".to_string(), areas);
    }
    (code.to_string(), areas)
}

fn verdict_text(code: &str, areas: i64) -> String {
    match code {
        "ready" => "Ready to print".to_string(),
        "notes" => "Repaired with notes".to_string(),
        "failed" => "Could not fully repair".to_string(),
        "preserved" => "Repaired, shape preserved".to_string(),
        "preserved_notes" => "Repaired with notes \u{2014} shape preserved".to_string(),
        "changed" => {
            if areas <= 0 {
                "Repaired, shape changed - check".to_string()
            } else {
                format!("Repaired, shape changed in {areas} area(s) - check")
            }
        }
        "low_confidence" => {
            if areas <= 0 {
                "Repaired, shape may be rebuilt - check".to_string()
            } else {
                format!("Repaired, shape changed in {areas} guessed area(s) - check")
            }
        }
        "not_repaired" => "Not fully repaired".to_string(),
        _ => "Shape check unavailable".to_string(),
    }
}

fn count_line(brief: &Brief) -> String {
    let mut fields = vec![
        format!("Holes closed: {}", brief.holes_closed),
        format!(
            "Orientation: {}",
            if brief.orientation_corrected {
                "corrected"
            } else {
                "already correct"
            }
        ),
    ];
    if let (Some(pb), Some(pa)) = (brief.parts_before, brief.parts_after) {
        if pa < pb {
            fields.push(format!("Parts: {pb} -> {pa} ({} removed)", pb - pa));
        } else if pa > pb {
            fields.push(format!("Parts: {pb} -> {pa} ({} added)", pa - pb));
        } else {
            fields.push(format!("Parts: {pa} kept"));
        }
    }
    if let Some(shape) = brief.shape_change_pct {
        let suffix = if brief.shape_source == Some("volume") {
            " (volume)"
        } else {
            ""
        };
        fields.push(format!("Shape change: {shape:.2}%{suffix}"));
    }
    format!("  {}", fields.join(" \u{00b7} "))
}

fn note_lines(brief: &Brief) -> Vec<String> {
    brief
        .notes
        .iter()
        .map(|code| match *code {
            "stage2_skipped" => "  Note: The final solid check was skipped; the closed model was kept.".to_string(),
            "stage2_error" => "  Note: The final solid check could not confirm the model.".to_string(),
            "shape_changed" => "  Note: The shape changed; check it in the slicer.".to_string(),
            "volume_warning" => "  Note: The volume changed; check it in the slicer.".to_string(),
            "graft_detail_loss" => "  Note: The surface was smoothed, so fine detail may be reduced.".to_string(),
            "dressing_detail_loss" => "  Note: The surface was coated, so it may sit slightly outside the original.".to_string(),
            "reload_non_manifold_vertices" => "  Note: The saved file reopens with a topology problem.".to_string(),
            "unit_warning" => "  Note: The model may not be in millimetres.".to_string(),
            "material_discarded" => "  Note: OBJ material references were not preserved.".to_string(),
            "holes_remaining" => "  Note: 1 hole(s) remain.".to_string(),
            "not_closed" => "  Note: The mesh is still not closed.".to_string(),
            _ => String::new(),
        })
        .filter(|line| !line.is_empty())
        .collect()
}

fn score_field(name: &str, value: Option<&Value>) -> String {
    match value.and_then(Value::as_f64) {
        Some(v) if v.fract() == 0.0 => format!("{name}: {}/100", v as i64),
        Some(v) => format!("{name}: {}/100", number(v)),
        None => format!("{name}: n/a"),
    }
}

fn print_block(report: &Value) -> Option<String> {
    let pr = report.get("print_report")?;
    if !pr.is_object() {
        return None;
    }
    let mut lines = vec![String::new(), "Print impact:".to_string()];
    let fixed = pr.get("fixed").and_then(Value::as_array).cloned().unwrap_or_default();
    if fixed.is_empty() {
        lines.push("  Fixed : nothing (input was already sound)".to_string());
    } else {
        let labels = |code: &str| match code {
            "holes_closed" => "hole(s) closed".to_string(),
            "non_manifold_fixed" => "non-manifold edge(s) fixed".to_string(),
            "faces_removed" => "bad face(s) removed".to_string(),
            "shells_removed" => "loose shell(s) removed".to_string(),
            "orientation_corrected" => "orientation corrected".to_string(),
            other => other.to_string(),
        };
        let parts: Vec<String> = fixed
            .iter()
            .map(|f| {
                format!(
                    "{} {}",
                    int_of(f.get("count")),
                    labels(f.get("code").and_then(Value::as_str).unwrap_or(""))
                )
            })
            .collect();
        lines.push(format!("  Fixed : {}", parts.join(", ")));
    }
    let issues = pr.get("issues").and_then(Value::as_array).cloned().unwrap_or_default();
    if issues.is_empty() {
        lines.push("  Remaining: none".to_string());
    } else {
        let severity = |s: &str| match s {
            "breaks" => "breaks the print",
            "may" => "may affect the print",
            "cosmetic" => "cosmetic, slicers ignore it",
            _ => "?",
        };
        let parts: Vec<String> = issues
            .iter()
            .map(|i| {
                let code = i.get("code").and_then(Value::as_str).unwrap_or("").replace('_', " ");
                match i.get("count").and_then(Value::as_i64) {
                    Some(n) => format!(
                        "{n} {code} [{}]",
                        severity(i.get("severity").and_then(Value::as_str).unwrap_or(""))
                    ),
                    None => format!(
                        "{code} [{}]",
                        severity(i.get("severity").and_then(Value::as_str).unwrap_or(""))
                    ),
                }
            })
            .collect();
        lines.push(format!("  Remaining: {}", parts.join(", ")));
    }
    let geom = pr.get("geometry").cloned().unwrap_or(Value::Null);
    let mut seg = Vec::new();
    if let Some(v) = num_of(geom.get("volume_change_pct")) {
        seg.push(format!("volume {}%", number(v)));
    }
    if let Some(v) = num_of(geom.get("deviation_max_pct")) {
        seg.push(format!("deviation max {}% of size", number(v)));
    }
    if !seg.is_empty() {
        lines.push(format!("  Geometry: {}", seg.join(", ")));
    }
    Some(lines.join("\n"))
}

fn stage2_lines(stage2: &Value) -> Vec<String> {
    const ORDER: [&str; 11] = [
        "construct_status",
        "input_vertices",
        "input_faces",
        "input_watertight",
        "ok",
        "volume_before",
        "shells_found",
        "shells",
        "volume_after",
        "output_vertices",
        "output_faces",
    ];
    let mut out = Vec::new();
    if let Some(obj) = stage2.as_object() {
        for key in ORDER {
            if let Some(value) = obj.get(key) {
                out.push(format!("  {key}: {}", display(value)));
            }
        }
        for (key, value) in obj {
            if !ORDER.contains(&key.as_str()) {
                out.push(format!("  {key}: {}", display(value)));
            }
        }
    }
    out
}

pub fn human_report(report: &Value, _show_defects: bool, _show_diff: bool) -> String {
    if report.get("error").is_some() && report.get("stage1").is_none() {
        return format!(
            "ERROR: {}",
            report.get("error").and_then(Value::as_str).unwrap_or("")
        );
    }
    let b = brief(report);
    let s1 = report.get("stage1").cloned().unwrap_or(Value::Null);
    let (headline_code, areas) = headline(report, &b);
    let mut lines = vec![verdict_text(&headline_code, areas), count_line(&b)];
    lines.extend(note_lines(&b));
    if b.verdict == "failed" {
        if let Some(err) = &b.error {
            lines.push(format!("  Error: {err}"));
        }
    }
    lines.push(String::new());
    lines.push(format!(
        "Input : {}",
        report.get("input").and_then(Value::as_str).unwrap_or("")
    ));
    lines.push(format!(
        "Output: {}",
        report.get("output").and_then(Value::as_str).unwrap_or("")
    ));
    lines.push(format!(
        "Mode  : {}",
        report.get("repair_mode").and_then(Value::as_str).unwrap_or("auto")
    ));
    if report.get("unit_warning").and_then(Value::as_bool).unwrap_or(false) {
        lines.push(format!(
            "WARNING: {}",
            report.get("unit_hint").and_then(Value::as_str).unwrap_or("")
        ));
    }
    if let Some(engine) = report.get("classifier_engine").and_then(Value::as_str) {
        lines.push(format!("Classifier: {engine}"));
    }
    if let Some(profile) = report.get("repair_profile").and_then(Value::as_str) {
        lines.push(format!("Profile: {profile}"));
    }
    lines.push(format!(
        "Intensity: {}",
        report.get("triage_intensity").and_then(Value::as_str).unwrap_or("balanced")
    ));
    if let Some(dt) = report.get("detected_type").and_then(Value::as_str) {
        let conf = num_of(report.get("detected_confidence")).unwrap_or(0.0);
        let mut line = format!("Type  : {dt} (confidence {conf:.2})");
        match report.get("tuning_applied").and_then(Value::as_bool) {
            Some(true) => line.push_str(" - tuned thresholds"),
            Some(false) => line.push_str(" - default thresholds (below confidence gate)"),
            None => {}
        }
        lines.push(line);
    }
    if let Some(rc) = report.get("repair_confidence").and_then(Value::as_i64) {
        let label = report
            .get("repair_confidence_label")
            .and_then(Value::as_str)
            .unwrap_or("?");
        let title = {
            let mut c = label.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        };
        lines.push(format!("Confidence: {rc}/100 ({title})"));
    }
    if report.get("repair_health").is_some()
        || report.get("repair_risk").is_some()
        || report.get("repair_fidelity").is_some()
    {
        lines.push(format!(
            "{}   {}   {}",
            score_field("Health", report.get("repair_health")),
            score_field("Risk", report.get("repair_risk")),
            score_field("Fidelity", report.get("repair_fidelity"))
        ));
        if let Some(note) = report.get("repair_fidelity_note").and_then(Value::as_str) {
            lines.push(format!("Fidelity note: {note}"));
        }
    }
    if let Some(block) = print_block(report) {
        lines.push(block);
    }
    if let Some(mh) = report.get("method_health") {
        lines.push(sutura_core::report::human_line_json(&mh.to_string()).unwrap_or_default());
    }
    if let Some(status) = report.get("repair_status").and_then(Value::as_str) {
        lines.push(format!("Repair status: {status}"));
    }
    lines.push(String::new());
    lines.push("Stage 1 (MeshLab):".to_string());
    lines.push(format!("  Holes closed            : {}", int_of(s1.get("holes_closed"))));
    lines.push(format!("  Holes remaining         : {}", int_of(s1.get("holes_remaining"))));
    lines.push(format!(
        "  Non-manifold edges fixed: {}",
        int_of(s1.get("non_manifold_edges_fixed"))
    ));
    lines.push(format!("  Faces removed           : {}", int_of(s1.get("faces_removed"))));
    lines.push(format!(
        "  Connected components    : {}",
        int_of(s1.get("components"))
    ));
    lines.push(format!(
        "  Two-manifold            : {}",
        if s1.get("two_manifold").and_then(Value::as_bool).unwrap_or(false) {
            "YES"
        } else {
            "NO"
        }
    ));
    if let Some(vol) = s1.get("volume_change_percent") {
        lines.push(format!("  Volume change          : {}%", display(vol)));
    }
    if let Some(stage2) = report.get("stage2") {
        lines.push(String::new());
        lines.push("Stage 2 (Manifold):".to_string());
        if let Some(err) = stage2.get("error").and_then(Value::as_str) {
            if err.starts_with("Stage 2 skipped") {
                lines.push(format!("  SKIPPED: {err}"));
            } else {
                lines.push(format!("  ERROR: {err}"));
            }
        } else {
            lines.extend(stage2_lines(stage2));
        }
    }
    lines.join("\n")
}

// --- Turkish post-processor (port of sutura/report_lang.py) ----------------

const VERDICTS: [(&str, &str); 11] = [
    ("Repaired, shape preserved", "Onarıldı, şekil korundu"),
    (
        "Repaired with notes — shape preserved",
        "Notlarla onarıldı — şekil korundu",
    ),
    (
        "Repaired, shape changed - check",
        "Onarıldı, şekil değişti - kontrol edin",
    ),
    (
        "Repaired, shape may be rebuilt - check",
        "Onarıldı, şekil yeniden kurulmuş olabilir - kontrol edin",
    ),
    ("Not fully repaired", "Tam onarılmadı"),
    ("Shape check unavailable", "Şekil kontrolü kullanılamıyor"),
    ("Safe to inspect", "İncelemek güvenli"),
    ("Review recommended", "İnceleme önerilir"),
    ("Caution advised", "Dikkatli olun"),
    ("Failed / inspect", "Başarısız / inceleyin"),
    ("Scoring unavailable", "Puanlama kullanılamıyor"),
];

const LABELS: [(&str, &str); 53] = [
    ("Input :", "Girdi :"),
    ("Output:", "Çıktı:"),
    ("Mode  :", "Mod   :"),
    ("Classifier:", "Sınıflandırıcı:"),
    ("Profile:", "Profil:"),
    ("Intensity:", "Yoğunluk:"),
    ("Type  :", "Tür   :"),
    ("Confidence:", "Güven:"),
    ("Health:", "Sağlık:"),
    ("Risk:", "Risk:"),
    ("Fidelity:", "Sadakat:"),
    ("Fidelity note:", "Sadakat notu:"),
    ("Repair status:", "Onarım durumu:"),
    ("Budget:", "Bütçe:"),
    ("Material:", "Malzeme:"),
    ("Stage 1 (MeshLab):", "Aşama 1 (MeshLab):"),
    ("Stage 2 (Manifold):", "Aşama 2 (Manifold):"),
    ("3MF objects repaired:", "Onarılan 3MF nesnesi:"),
    ("Objects watertight:", "Su geçirmez nesneler:"),
    ("Methods:", "Yöntemler:"),
    ("Print impact:", "Baskı etkisi:"),
    ("Fixed :", "Düzeltilen:"),
    ("Fixed:", "Düzeltilen:"),
    ("Remaining:", "Kalan:"),
    ("Geometry:", "Geometri:"),
    ("Orientation:", "Yönelim:"),
    ("Parts:", "Parçalar:"),
    ("Shape change:", "Şekil değişimi:"),
    ("Volume:", "Hacim:"),
    ("Deviation:", "Sapma:"),
    ("Holes closed", "Kapatılan delikler"),
    ("Holes remaining", "Kalan delikler"),
    ("Non-manifold edges fixed", "Düzeltilen non-manifold kenarlar"),
    ("Faces removed", "Kaldırılan yüzler"),
    ("Connected components", "Bağlı bileşenler"),
    ("Two-manifold", "İki-manifold"),
    ("Vertices", "Tepe noktaları"),
    ("Faces", "Yüzler"),
    ("Surface area change", "Yüzey alanı değişimi"),
    ("Volume change", "Hacim değişimi"),
    ("Pinched vertices split", "Sıkışmış tepe noktaları ayrıldı"),
    ("Reload-safe final pass", "Reload-güvenli son geçiş"),
    ("Extreme passes", "Ekstrem geçişler"),
    ("Autorefine (experiment)", "Otomatik iyileştirme (deneysel)"),
    (
        "Indirect autorefine (experiment)",
        "Dolaylı otomatik iyileştirme (deneysel)",
    ),
    ("fTetWild fallback", "fTetWild yedeği"),
    ("WARNING:", "UYARI:"),
    ("ERROR:", "HATA:"),
    ("SKIPPED:", "ATLANDI:"),
    ("Note:", "Not:"),
    ("Objects:", "Nesneler:"),
    ("watertight", "su geçirmez"),
    ("stage 2 skipped", "aşama 2 atlandı"),
];

const VALUES: [(&str, &str); 25] = [
    (
        "stage 2 skipped",
        "aşama 2 atlandı",
    ),
    ("stage 2 error", "aşama 2 hatası"),
    (
        "no improvement (kept reload-welded state)",
        "iyileşme yok (reload-welded durum korundu)",
    ),
    (
        "default thresholds (below confidence gate)",
        "varsayılan eşikler (güven kapısının altında)",
    ),
    (
        "nothing (input was already sound)",
        "yok (girdi zaten sağlamdı)",
    ),
    ("within budget", "bütçe içinde"),
    ("reduced budget", "azaltılmış bütçe"),
    ("tuned thresholds", "ayarlanmış eşikler"),
    ("already correct", "zaten doğru"),
    ("not kept", "korunmadı"),
    ("adopted", "benimsendi"),
    ("EXCEEDED", "AŞILDI"),
    ("YES", "EVET"),
    ("NO", "HAYIR"),
    ("watertight", "su geçirmez"),
    ("deviation max", "maksimum sapma"),
    (" of size", " (boyuta göre)"),
    ("volume ", "hacim "),
    (" kept", " korundu"),
    ("confidence ", "güven "),
    ("Medium", "Orta"),
    ("High", "Yüksek"),
    ("Low", "Düşük"),
    ("corrected", "düzeltildi"),
    ("already correct", "zaten doğru"),
];

fn looks_like_path(text: &str) -> bool {
    let body = text.trim().trim_end_matches(['.', ',', ';']);
    if body.is_empty() {
        return false;
    }
    if body.contains('/') || body.contains('\\') {
        return true;
    }
    let lower = body.to_lowercase();
    [".stl", ".obj", ".3mf", ".ply", ".gcode", ".zip", ".json"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

fn translate_values(text: &str) -> String {
    if looks_like_path(text) {
        return text.to_string();
    }
    let mut out = text.to_string();
    for (src, dst) in VALUES {
        out = out.replace(src, dst);
    }
    out
}

pub fn translate(text: &str, lang: &str) -> String {
    if lang != "tr" {
        return text.to_string();
    }
    let mut text_out = text.to_string();
    for (src, dst) in VERDICTS {
        text_out = text_out.replace(src, dst);
    }
    let mut out = Vec::new();
    for line in text_out.split('\n') {
        let indent_len = line.len() - line.trim_start().len();
        let (indent, body) = line.split_at(indent_len);
        let mut matched = false;
        let mut translated = body.to_string();
        for (src, dst) in LABELS {
            if body.starts_with(src) {
                translated = format!("{dst}{}", translate_values(&body[src.len()..]));
                matched = true;
                break;
            }
        }
        if !matched && (body.trim_start().starts_with('-') || body.trim_start().starts_with('*')) {
            translated = translate_values(body);
        }
        out.push(format!("{indent}{translated}"));
    }
    let text_out = out.join("\n");
    // "Repaired, shape changed in N area(s) - check"
    let mut final_out = String::with_capacity(text_out.len());
    let mut rest = text_out.as_str();
    const PREFIX: &str = "Repaired, shape changed in ";
    while let Some(pos) = rest.find(PREFIX) {
        final_out.push_str(&rest[..pos]);
        let after = &rest[pos + PREFIX.len()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        let suffix = " area(s) - check";
        if !digits.is_empty() && after[digits.len()..].starts_with(suffix) {
            final_out.push_str(&format!(
                "Onarıldı, şekil {digits} alanda değişti - kontrol edin"
            ));
            rest = &after[digits.len() + suffix.len()..];
        } else {
            final_out.push_str(PREFIX);
            rest = after;
        }
    }
    final_out.push_str(rest);
    final_out
}

#[allow(dead_code)]
fn _m(_: &Map<String, Value>) {}
