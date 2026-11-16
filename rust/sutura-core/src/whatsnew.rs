// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! "What's new": a one-slot snapshot of the release notes, taken before the new
//! version is installed so it can be shown with no network afterwards.
//!
//! **One copy, fixed path** — `<data>/whatsnew/current/` with a `tag` file, not
//! a directory per version. A new update replaces it, but **only after the
//! replacement is fully written**: notes are staged in `whatsnew/.staging-<pid>/`
//! and swapped in with a `rename`, so a failed fetch leaves the previous
//! snapshot intact rather than deleting it and ending up with nothing.
//!
//! Markdown image links are rewritten to the local `img-N.png` files, so the
//! document can be rendered offline with no remote requests at all.

use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::http;

/// The fixed snapshot directory name.
pub const CURRENT: &str = "current";

/// `<data>/whatsnew/`.
pub fn whatsnew_root() -> PathBuf {
    crate::config::learning::data_dir().join("whatsnew")
}

/// `<data>/whatsnew/current/`.
pub fn current_dir() -> PathBuf {
    whatsnew_root().join(CURRENT)
}

/// The snapshot's tag, when one exists.
pub fn current_tag() -> Option<String> {
    std::fs::read_to_string(current_dir().join("tag"))
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// The snapshot's markdown body, when one exists.
pub fn current_body() -> Option<String> {
    std::fs::read_to_string(current_dir().join("body.md")).ok()
}

/// Findings from rewriting the body's image links.
#[derive(Debug, Default, PartialEq)]
pub struct RewriteResult {
    /// The rewritten markdown.
    pub body: String,
    /// Remote image URLs, in first-seen order, as `img-N` sources.
    pub images: Vec<String>,
}

/// Rewrite markdown image links to local files.
///
/// Handles `![alt](url)` and bare `![alt](url "title")`. URLs that are already
/// relative are left alone. The same URL appearing twice maps to one file.
pub fn rewrite_images(body: &str) -> RewriteResult {
    let mut out = String::with_capacity(body.len());
    let mut images: Vec<String> = Vec::new();
    let mut rest = body;

    while let Some(start) = rest.find("![") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(close_alt) = after.find(']') else {
            out.push_str(&rest[start..]);
            return RewriteResult {
                body: out,
                images,
            };
        };
        let alt = &after[..close_alt];
        let after_alt = &after[close_alt + 1..];
        if !after_alt.starts_with('(') {
            out.push_str(&rest[start..start + 2 + close_alt + 1]);
            rest = after_alt;
            continue;
        }
        let Some(close_paren) = after_alt.find(')') else {
            out.push_str(&rest[start..]);
            return RewriteResult {
                body: out,
                images,
            };
        };
        let target = after_alt[1..close_paren].trim();
        // Split off an optional quoted title.
        let url = target.split_whitespace().next().unwrap_or(target);

        if url.starts_with("http://") || url.starts_with("https://") {
            let idx = match images.iter().position(|u| u == url) {
                Some(i) => i,
                None => {
                    images.push(url.to_string());
                    images.len() - 1
                }
            };
            out.push_str(&format!("![{alt}](img-{}.png)", idx + 1));
        } else {
            out.push_str(&format!("![{alt}]({target})"));
        }
        rest = &after_alt[close_paren + 1..];
    }
    out.push_str(rest);
    RewriteResult { body: out, images }
}

/// Write a snapshot for `tag`, replacing `current/` atomically.
///
/// `fetch` resolves a URL to bytes; injectable so a gate can run offline. A
/// failed image download is skipped — the snapshot is still written, because the
/// text is the important part.
pub fn write_snapshot(tag: &str, release_body: &str, fetch: &dyn Fn(&str) -> Option<Vec<u8>>) -> Result<(), String> {
    let root = whatsnew_root();
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let staging = root.join(format!(".staging-{}", std::process::id()));
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;

    let rewritten = rewrite_images(release_body);
    std::fs::write(staging.join("tag"), tag.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::write(staging.join("body.md"), rewritten.body.as_bytes())
        .map_err(|e| e.to_string())?;
    for (i, url) in rewritten.images.iter().enumerate() {
        // Best effort: one unreachable image must not cost the whole snapshot.
        if let Some(bytes) = fetch(url) {
            std::fs::write(staging.join(format!("img-{}.png", i + 1)), bytes)
                .map_err(|e| e.to_string())?;
        }
    }

    // Swap: move the old `current/` aside, move staging in, then drop the old.
    let current = root.join(CURRENT);
    let old = root.join(format!(".old-{}", std::process::id()));
    if old.exists() {
        std::fs::remove_dir_all(&old).map_err(|e| e.to_string())?;
    }
    if current.exists() {
        std::fs::rename(&current, &old).map_err(|e| e.to_string())?;
    }
    match std::fs::rename(&staging, &current) {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&old);
            Ok(())
        }
        Err(e) => {
            // Put the previous snapshot back before reporting the failure.
            if old.exists() {
                let _ = std::fs::rename(&old, &current);
            }
            let _ = std::fs::remove_dir_all(&staging);
            Err(e.to_string())
        }
    }
}

/// Snapshot the notes for `tag` from the GitHub release, ignoring every failure.
///
/// Called *before* installing, so a rolled-back update still leaves the notes
/// (harmless: they describe a version that is not installed, and the next
/// attempt overwrites them).
pub fn snapshot_release_notes(tag: &str) -> Result<(), String> {
    let release = crate::updater::fetch_latest_release_full().ok_or("no release data")?;
    let body = release
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    write_snapshot(tag, &body, &http::get_bytes)
}

/// Render the snapshot for a terminal: markdown reduced to plain text.
pub fn plain_text(tag: &str, body: &str, dir: &Path) -> String {
    let mut lines = vec![format!("What's new in {tag}"), String::new()];
    for line in body.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('#') {
            lines.push(rest.trim_start_matches('#').trim().to_string());
            continue;
        }
        if trimmed.starts_with("![") {
            // Keep the image as a local path so the user can open it.
            if let Some(name) = trimmed
                .split("](")
                .nth(1)
                .and_then(|s| s.split(')').next())
            {
                lines.push(format!("  [image: {}]", dir.join(name).display()));
            }
            continue;
        }
        lines.push(line.to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_remote_images_to_local_files() {
        let body = "Intro\n![shot](https://example.com/a.png)\nmore\n![b](https://example.com/b.jpg)";
        let r = rewrite_images(body);
        assert!(r.body.contains("![shot](img-1.png)"), "{}", r.body);
        assert!(r.body.contains("![b](img-2.png)"), "{}", r.body);
        assert_eq!(
            r.images,
            vec![
                "https://example.com/a.png".to_string(),
                "https://example.com/b.jpg".to_string()
            ]
        );
    }

    /// The same URL twice is one download, and both links point at one file.
    #[test]
    fn repeated_url_maps_to_one_file() {
        let body = "![a](https://x/y.png) ![b](https://x/y.png)";
        let r = rewrite_images(body);
        assert_eq!(r.images.len(), 1);
        assert!(r.body.contains("![a](img-1.png)"));
        assert!(r.body.contains("![b](img-1.png)"));
    }

    /// Relative links and plain text are untouched.
    #[test]
    fn relative_links_are_left_alone() {
        let body = "![a](img-1.png) and no images here";
        let r = rewrite_images(body);
        assert_eq!(r.body, body);
        assert!(r.images.is_empty());
    }

    /// Titles inside the parens must not become part of the path.
    #[test]
    fn title_in_link_is_dropped() {
        let r = rewrite_images(r#"![a](https://x/y.png "caption")"#);
        assert_eq!(r.body, "![a](img-1.png)");
    }

    /// Malformed markdown is passed through rather than panicking.
    #[test]
    fn malformed_markdown_is_passed_through() {
        for body in ["![unclosed", "![a]", "![a](unclosed", "plain"] {
            let r = rewrite_images(body);
            assert_eq!(r.body, body, "body {body:?} changed");
        }
    }

    /// A snapshot written twice keeps exactly one `current/`, with the newer tag.
    #[test]
    fn second_snapshot_replaces_the_first_in_place() {
        let dir = std::env::temp_dir().join(format!("sutura-wn-{}", std::process::id()));
        safe_remove(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Point the data dir at the temp location for this test only.
        std::env::set_var("SUTURA_DATA_DIR", dir.join("data"));
        let fetch = |_u: &str| Some(b"png".to_vec());
        write_snapshot("v0.10.0", "![x](https://e/1.png)", &fetch).unwrap();
        assert_eq!(current_tag().as_deref(), Some("v0.10.0"));
        write_snapshot("v0.10.1", "![x](https://e/2.png)", &fetch).unwrap();
        assert_eq!(current_tag().as_deref(), Some("v0.10.1"));
        // Exactly one directory under whatsnew/ (no per-version accumulation).
        let dirs: Vec<_> = std::fs::read_dir(whatsnew_root())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(dirs.len(), 1, "expected only current/, got {dirs:?}");
        std::env::remove_var("SUTURA_DATA_DIR");
        safe_remove(&dir);
    }

    fn safe_remove(p: &Path) {
        let _ = std::fs::remove_dir_all(p);
    }
}
