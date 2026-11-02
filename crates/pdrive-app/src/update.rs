// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Update check: asks GitHub for the latest published release at launch, every 15 minutes and on
//! request, and reports whether it is newer than the running version. Nothing is downloaded or installed; the user is sent to the
//! release page.

use std::time::Duration;

use serde::Deserialize;

pub const RELEASES_URL: &str = "https://github.com/bpresles/PDrive-for-Linux/releases/latest";
const API_URL: &str = "https://api.github.com/repos/bpresles/PDrive-for-Linux/releases/latest";
const CHECK_EVERY: Duration = Duration::from_secs(15 * 60);
/// Let the application settle before the first request.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(3);

#[derive(Deserialize)]
struct Release {
    tag_name: String,
}

/// "v1.2.3" or "1.2.3" to (1, 2, 3).
fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().trim_start_matches('v').split('.');
    let triple = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    parts.next().is_none().then_some(triple)
}

/// The version to announce (without the "v"), if `latest` is newer than `current`.
fn newer(latest: &str, current: &str) -> Option<String> {
    let (latest_parsed, current_parsed) = (parse(latest)?, parse(current)?);
    (latest_parsed > current_parsed).then(|| latest.trim().trim_start_matches('v').to_owned())
}

async fn latest_release(client: &reqwest::Client) -> reqwest::Result<Release> {
    client.get(API_URL).header("Accept", "application/vnd.github+json").send().await?.error_for_status()?.json().await
}

/// Why a check ran.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// The first successful scheduled check after the start: a newer release is announced.
    Launch,
    /// Periodic check: silent.
    Background,
    /// Asked by the user ("Check for update…"): always answered.
    Manual,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// A newer release exists (version without the "v").
    Newer(String),
    UpToDate,
    Failed(String),
}

#[derive(Debug)]
pub struct Report {
    pub kind: Kind,
    pub outcome: Outcome,
}

/// Asks the check thread for an immediate check.
#[derive(Clone)]
pub struct Checker(async_channel::Sender<()>);

impl Checker {
    pub fn check_now(&self) {
        let _ = self.0.try_send(());
    }
}

/// Starts the checks on their own thread: shortly after the start, then every
/// [`CHECK_EVERY`], and on request. Returns the handle to request a check and the reports.
pub fn spawn() -> (Checker, async_channel::Receiver<Report>) {
    let (requests, requested) = async_channel::unbounded::<()>();
    let (sender, reports) = async_channel::unbounded();
    let started = std::thread::Builder::new().name("update-check".into()).spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(e) => return eprintln!("update check: cannot start the runtime: {e}"),
        };
        runtime.block_on(async move {
            let client = match reqwest::Client::builder().user_agent(concat!("pdrive-app/", env!("CARGO_PKG_VERSION"))).timeout(Duration::from_secs(20)).build()
            {
                Ok(client) => client,
                Err(e) => return eprintln!("update check: cannot create the HTTP client: {e}"),
            };
            // Until one check succeeded, the scheduled ones are "launch" checks (the network may
            // not be up yet at login); a manual check that succeeds counts as well.
            let mut announced = false;
            let mut wait = FIRST_CHECK_DELAY;
            loop {
                let kind = tokio::select! {
                    () = tokio::time::sleep(wait) => if announced { Kind::Background } else { Kind::Launch },
                    request = requested.recv() => if request.is_ok() { Kind::Manual } else { return },
                };
                let outcome = match latest_release(&client).await {
                    Ok(release) => {
                        announced = true;
                        newer(&release.tag_name, env!("CARGO_PKG_VERSION")).map_or(Outcome::UpToDate, Outcome::Newer)
                    }
                    Err(e) => {
                        eprintln!("update check: {e}");
                        Outcome::Failed(e.without_url().to_string())
                    }
                };
                if sender.send(Report { kind, outcome }).await.is_err() {
                    return;
                }
                wait = CHECK_EVERY;
            }
        });
    });
    if let Err(e) = started {
        eprintln!("update check: cannot start the thread: {e}");
    }
    (Checker(requests), reports)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announces_only_newer_versions() {
        assert_eq!(newer("v0.1.2", "0.1.1").as_deref(), Some("0.1.2"));
        assert_eq!(newer("v0.2.0", "0.1.9").as_deref(), Some("0.2.0"));
        assert_eq!(newer("v1.0.0", "0.9.9").as_deref(), Some("1.0.0"));
        assert_eq!(newer("v0.10.0", "0.9.0").as_deref(), Some("0.10.0"));
        assert_eq!(newer("v0.1.1", "0.1.1"), None);
        assert_eq!(newer("v0.1.0", "0.1.1"), None);
    }

    #[test]
    fn ignores_unusable_tags() {
        assert_eq!(newer("nightly", "0.1.1"), None);
        assert_eq!(newer("v1.2", "0.1.1"), None);
        assert_eq!(newer("v1.2.3-rc1", "0.1.1"), None);
    }
}
