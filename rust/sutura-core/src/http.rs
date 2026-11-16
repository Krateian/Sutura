// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! The one HTTP client the updater and the changelog share.
//!
//! Deliberately small: GET plus a timeout, over `rustls` so no system OpenSSL is
//! linked (the release binaries must stay self-contained). There is exactly one
//! HTTP path in the program — a second one would mean two sets of TLS settings,
//! two timeout stories and two ways to fail offline.
//!
//! **Offline is not an error.** Python's updater wraps every network call in
//! `except: return None`, so a machine with no network simply reports "no
//! update" and the repair carries on. Every function here returns `None` on any
//! failure for the same reason.

use std::time::Duration;

/// Default per-request timeout, in seconds.
pub const TIMEOUT_SECS: u64 = 15;

/// User agent the GitHub API expects to identify (a bare request is accepted,
/// but identifying the client keeps rate-limit diagnostics readable).
pub const USER_AGENT: &str = "sutura-updater";

/// Fetch a URL as text. `None` on any failure (offline, DNS, TLS, non-2xx).
pub fn get_text(url: &str) -> Option<String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
        .user_agent(USER_AGENT)
        .build()
        .new_agent();
    match agent.get(url).call() {
        Ok(mut resp) => resp.body_mut().read_to_string().ok(),
        Err(_) => None,
    }
}

/// Fetch a URL as bytes. `None` on any failure.
pub fn get_bytes(url: &str) -> Option<Vec<u8>> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
        .user_agent(USER_AGENT)
        .build()
        .new_agent();
    match agent.get(url).call() {
        Ok(mut resp) => {
            let mut buf = Vec::new();
            use std::io::Read as _;
            resp.body_mut()
                .as_reader()
                .read_to_end(&mut buf)
                .ok()
                .map(|_| buf)
        }
        Err(_) => None,
    }
}

/// Fetch a URL as parsed JSON. `None` on any failure, including bad JSON.
pub fn get_json(url: &str) -> Option<serde_json::Value> {
    let text = get_text(url)?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unreachable host must yield `None`, never a panic — this is the
    /// offline path every caller relies on.
    #[test]
    fn unreachable_host_is_none_not_a_panic() {
        // 127.0.0.1 on a closed port: connection refused, fast.
        assert!(get_text("http://127.0.0.1:1/nope").is_none());
        assert!(get_json("http://127.0.0.1:1/nope").is_none());
        assert!(get_bytes("http://127.0.0.1:1/nope").is_none());
    }

    /// A malformed URL is also just `None`.
    #[test]
    fn malformed_url_is_none() {
        assert!(get_text("not-a-url").is_none());
    }
}
