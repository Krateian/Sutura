//! Print-impact report assembly, matching `report.build_print_report`.

use serde_json::{json, Map, Value};
use sutura_geom::diagnosis::diagnose_core;
use sutura_geom::winding::MeshBvh;

const BREAKS: &str = "breaks";
const MAY: &str = "may_affect";
const COSMETIC: &str = "cosmetic";
const FACE_CAP: usize = 150_000;
const SAMPLES: usize = 20_000;
const THIN_RATIO: f64 = 0.02;
const THIN_AREA: f64 = 0.005;
const THIN_BIT: u8 = 128;
const VOL_MAY: f64 = 5.0;
const VOL_BREAKS: f64 = 15.0;
const DEV_MAY: f64 = 0.01;
const DEV_BREAKS: f64 = 0.05;

fn rank(s: &str) -> i64 {
    match s {
        BREAKS => 0,
        MAY => 1,
        COSMETIC => 2,
        _ => 9,
    }
}
fn int_of(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().map(|x| x as i64).unwrap_or(0),
        _ => 0,
    }
}
fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
}
fn diag(v: &[[f64; 3]]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let (mut lo, mut hi) = (v[0], v[0]);
    for p in v {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let d = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}
fn r4(x: f64) -> f64 {
    (x * 1e4).round() / 1e4
}
fn r3(x: f64) -> f64 {
    (x * 1e3).round() / 1e3
}
fn r2(x: f64) -> f64 {
    (x * 1e2).round() / 1e2
}

fn deviation(
    report: &Value,
    iv: Option<&[[f64; 3]]>,
    it: Option<&[[i32; 3]]>,
    ov: Option<&[[f64; 3]]>,
    ot: Option<&[[i32; 3]]>,
) -> Option<Value> {
    let iv = iv?;
    let d = diag(iv);
    if d > 0.0 {
        let pvd = report
            .get("graft")
            .and_then(|g| g.get("per_vertex_deviation"))
            .or_else(|| report.get("per_vertex_deviation"));
        if let Some(arr) = pvd.and_then(Value::as_array) {
            let vals: Vec<f64> = arr.iter().filter_map(Value::as_f64).map(f64::abs).collect();
            if !vals.is_empty() {
                let max = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                let mean = vals.iter().sum::<f64>() / vals.len() as f64;
                return Some(
                    json!({"max_pct": r4(max/d*100.0), "mean_pct": r4(mean/d*100.0),
                                   "samples": vals.len(), "source": "graft"}),
                );
            }
        }
        if let Some(hr) = report
            .get("dressing")
            .and_then(|x| x.get("hausdorff_rel_max"))
            .and_then(Value::as_f64)
        {
            return Some(json!({"max_pct": r4(hr*100.0), "mean_pct": Value::Null,
                               "samples": Value::Null, "source": "dressing"}));
        }
    }
    let (it, ov, ot) = (it?, ov?, ot?);
    if d <= 0.0 || it.len() > FACE_CAP || ot.len() > FACE_CAP {
        return None;
    }
    let pts: Vec<[f64; 3]> = if ov.len() > SAMPLES {
        let last = ov.len() - 1;
        (0..SAMPLES)
            .map(|i| ov[(i as f64 * last as f64 / (SAMPLES - 1) as f64).floor() as usize])
            .collect()
    } else {
        ov.to_vec()
    };
    let rt: Vec<[usize; 3]> = it
        .iter()
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    let bvh = MeshBvh::from_arrays(iv, &rt);
    let (mut max, mut sum) = (f64::NEG_INFINITY, 0.0);
    for p in &pts {
        let x = bvh.closest_point(*p).1;
        max = max.max(x);
        sum += x;
    }
    if pts.is_empty() {
        return None;
    }
    let mean = sum / pts.len() as f64;
    Some(
        json!({"max_pct": r4(max/d*100.0), "mean_pct": r4(mean/d*100.0),
                "samples": pts.len(), "source": "measured"}),
    )
}

fn volume_issue(vol: Option<f64>) -> Option<Value> {
    let v = vol?;
    let a = v.abs();
    let sev = if a > VOL_BREAKS {
        BREAKS
    } else if a > VOL_MAY {
        MAY
    } else {
        return None;
    };
    Some(
        json!({"code": "volume_change", "severity": sev, "count": Value::Null,
                "message_key": "pr_volume_change",
                "detail": {"pct": r2(v), "heuristic": true}}),
    )
}
fn deviation_issue(dev: Option<f64>) -> Option<Value> {
    let v = dev?;
    let a = v.abs();
    let sev = if a > DEV_BREAKS * 100.0 {
        BREAKS
    } else if a > DEV_MAY * 100.0 {
        MAY
    } else {
        return None;
    };
    Some(
        json!({"code": "shape_deviation", "severity": sev, "count": Value::Null,
                "message_key": "pr_shape_deviation",
                "detail": {"pct": r3(v), "heuristic": true}}),
    )
}

fn thin_advisory(
    report: &Value,
    iv: Option<&[[f64; 3]]>,
    it: Option<&[[i32; 3]]>,
) -> Option<Value> {
    let (iv, it) = (iv?, it?);
    if iv.is_empty() || it.is_empty() || it.len() > FACE_CAP {
        return None;
    }
    let rt: Vec<[usize; 3]> = it
        .iter()
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    let d = diagnose_core(iv, &rt, "fast");
    let tw = &d.thin_wall;
    if !tw.available || tw.thin_ratio < THIN_RATIO {
        return None;
    }
    let (mut total, mut thin) = (0.0, 0.0);
    for (i, t) in it.iter().enumerate() {
        let a = iv[t[0] as usize];
        let b = iv[t[1] as usize];
        let c = iv[t[2] as usize];
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let cr = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        let ar = 0.5 * (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt();
        total += ar;
        if d.face_mask
            .get(i)
            .map(|m| m & THIN_BIT != 0)
            .unwrap_or(false)
        {
            thin += ar;
        }
    }
    if total <= 0.0 {
        return None;
    }
    let frac = thin / total;
    if frac < THIN_AREA {
        return None;
    }
    let mut adv = json!({
        "code": "thin_wall", "message_key": "pr_thin_wall",
        "thin_ratio": r4(tw.thin_ratio), "thin_area_frac": r4(frac),
        "regions": tw.thin_faces, "min_norm": r4(tw.min_norm),
        "median_norm": r4(tw.median_norm), "threshold_norm": r4(tw.threshold_norm),
        "estimated": tw.estimated, "heuristic": true,
    });
    if !report
        .get("unit_warning")
        .map(|v| v.as_bool().unwrap_or(false))
        .unwrap_or(false)
    {
        adv["min_mm"] = Value::from(r4(tw.min));
        adv["median_mm"] = Value::from(r4(tw.median));
        adv["threshold_mm"] = Value::from(r4(tw.threshold_mm));
        adv["units"] = Value::from(tw.units);
    }
    Some(adv)
}

pub fn build_print_report_value(
    report: &Value,
    iv: Option<&[[f64; 3]]>,
    it: Option<&[[i32; 3]]>,
    ov: Option<&[[f64; 3]]>,
    ot: Option<&[[i32; 3]]>,
) -> Option<Value> {
    let s1 = report.get("stage1")?;
    if !s1.is_object() {
        return None;
    }
    let mut issues = Vec::new();
    let mut fixed = Vec::new();
    let h = int_of(s1.get("holes_remaining"));
    if h > 0 {
        issues.push(json!({"code":"holes_remaining","severity":BREAKS,"count":h,
        "message_key":"pr_holes_remaining","detail":{"two_manifold":s1.get("two_manifold").and_then(Value::as_bool).unwrap_or(false)}}));
    }
    let nm = int_of(s1.get("non_manifold_edges_remaining"));
    if nm > 0 {
        issues.push(
            json!({"code":"non_manifold_remaining","severity":BREAKS,"count":nm,
        "message_key":"pr_non_manifold_remaining"}),
        );
    }
    if let Some(si) = num(s1.get("self_intersections_remaining")) {
        if si > 0.0 {
            issues.push(
                json!({"code":"self_intersections_remaining","severity":COSMETIC,
            "count":si as i64,"message_key":"pr_self_intersections_remaining",
            "detail":{"note":"even_odd_double_wall_caveat"}}),
            );
        }
    }
    let comp = int_of(s1.get("components"));
    let cb = int_of(s1.get("components_before"));
    if comp > 1 {
        issues.push(json!({"code":"multiple_shells","severity":MAY,"count":comp,
        "message_key":"pr_multiple_shells"}));
    }
    let rs = (cb - comp).max(0);
    if rs > 0 {
        fixed.push(json!({"code":"shells_removed","count":rs}));
    }
    for (k, code) in [
        ("holes_closed", "holes_closed"),
        ("non_manifold_edges_fixed", "non_manifold_fixed"),
        ("faces_removed", "faces_removed"),
    ] {
        let n = int_of(s1.get(k));
        if n > 0 {
            fixed.push(json!({"code":code,"count":n}));
        }
    }
    if let (Some(a), Some(b)) = (num(s1.get("volume_before")), num(s1.get("volume_after"))) {
        if a < 0.0 && b > 0.0 {
            fixed.push(json!({"code":"orientation_corrected","count":1}));
        }
    }
    let vol = num(s1.get("volume_change_percent"));
    if let Some(v) = volume_issue(vol) {
        issues.push(v);
    }
    let mut na = Vec::new();
    if vol.is_none() {
        na.push(Value::from("volume"));
    }
    let dev = deviation(report, iv, it, ov, ot);
    if dev.is_none() {
        na.push(Value::from("deviation"));
    } else if let Some(di) = deviation_issue(
        dev.as_ref()
            .and_then(|d| d.get("max_pct"))
            .and_then(Value::as_f64),
    ) {
        issues.push(di);
    }
    issues.sort_by_key(|x| rank(x["severity"].as_str().unwrap_or("")));
    let geometry = json!({
        "volume_change_pct": vol,
        "surface_area_change_pct": s1.get("surface_area_change_percent").cloned().unwrap_or(Value::Null),
        "deviation_max_pct": dev.as_ref().and_then(|d| d.get("max_pct")).cloned().unwrap_or(Value::Null),
        "deviation_mean_pct": dev.as_ref().and_then(|d| d.get("mean_pct")).cloned().unwrap_or(Value::Null),
        "deviation_source": dev.as_ref().and_then(|d| d.get("source")).cloned().unwrap_or(Value::Null),
        "faces_before": s1.get("faces_before").cloned().unwrap_or(Value::Null),
        "faces_after": s1.get("faces_after").cloned().unwrap_or(Value::Null),
        "components_before": s1.get("components_before").cloned().unwrap_or(Value::Null),
        "components_after": s1.get("components").cloned().unwrap_or(Value::Null),
    });
    let mut advs = Vec::new();
    if let Some(a) = thin_advisory(report, iv, it) {
        advs.push(a);
    }
    Some(
        json!({"issues":issues,"fixed":fixed,"geometry":geometry,"advisories":advs,"not_assessed":na}),
    )
}

pub fn aggregate_print_reports_value(prs: &[Value]) -> Option<Value> {
    let prs: Vec<&Value> = prs.iter().filter(|p| p.is_object()).collect();
    if prs.is_empty() {
        return None;
    }
    let mut issues: Map<String, Value> = Map::new();
    let mut fixed: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    let mut advs: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    let mut na = std::collections::BTreeSet::new();
    for p in &prs {
        if let Some(a) = p.get("issues").and_then(Value::as_array) {
            for it in a {
                let code = it.get("code").and_then(Value::as_str).unwrap_or("");
                if let Some(cur) = issues.get_mut(code) {
                    let c = cur.get("count").and_then(Value::as_i64).unwrap_or(0)
                        + it.get("count").and_then(Value::as_i64).unwrap_or(0);
                    cur["count"] = Value::from(c);
                    if rank(it["severity"].as_str().unwrap_or(""))
                        < rank(cur["severity"].as_str().unwrap_or(""))
                    {
                        cur["severity"] = it["severity"].clone();
                    }
                } else {
                    issues.insert(code.into(), it.clone());
                }
            }
        }
        if let Some(a) = p.get("fixed").and_then(Value::as_array) {
            for f in a {
                if let Some(c) = f.get("code").and_then(Value::as_str) {
                    *fixed.entry(c.into()).or_insert(0) +=
                        f.get("count").and_then(Value::as_i64).unwrap_or(0);
                }
            }
        }
        if let Some(a) = p.get("advisories").and_then(Value::as_array) {
            for x in a {
                if let Some(c) = x.get("code").and_then(Value::as_str) {
                    let better = advs
                        .get(c)
                        .map(|cur| {
                            x.get("thin_ratio").and_then(Value::as_f64).unwrap_or(0.0)
                                > cur.get("thin_ratio").and_then(Value::as_f64).unwrap_or(0.0)
                        })
                        .unwrap_or(true);
                    if better {
                        advs.insert(c.into(), x.clone());
                    }
                }
            }
        }
        if let Some(a) = p.get("not_assessed").and_then(Value::as_array) {
            for x in a {
                if let Some(s) = x.as_str() {
                    na.insert(s.to_string());
                }
            }
        }
    }
    let mut worst = prs[0];
    let mut best = worst
        .get("geometry")
        .and_then(|g| g.get("volume_change_pct"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
        .abs();
    for p in &prs[1..] {
        let v = p
            .get("geometry")
            .and_then(|g| g.get("volume_change_pct"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .abs();
        if v > best {
            best = v;
            worst = p;
        }
    }
    let fv: Vec<Value> = fixed
        .into_iter()
        .map(|(k, v)| json!({"code":k,"count":v}))
        .collect();
    let av: Vec<Value> = advs.into_iter().map(|(_, v)| v).collect();
    let mut iv: Vec<Value> = issues.into_values().collect();
    iv.sort_by_key(|x| rank(x["severity"].as_str().unwrap_or("")));
    Some(json!({"issues":iv,"fixed":fv,
        "geometry":worst.get("geometry").cloned().unwrap_or(Value::Null),
        "advisories":av,
        "not_assessed":na.into_iter().map(Value::from).collect::<Vec<_>>()}))
}

pub fn build_print_report_json(
    report: &str,
    iv: Option<&[[f64; 3]]>,
    it: Option<&[[i32; 3]]>,
    ov: Option<&[[f64; 3]]>,
    ot: Option<&[[i32; 3]]>,
) -> Result<Option<String>, String> {
    let r: Value = serde_json::from_str(report).map_err(|e| e.to_string())?;
    Ok(build_print_report_value(&r, iv, it, ov, ot).map(|v| v.to_string()))
}
