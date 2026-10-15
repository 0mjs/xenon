//! Checking GitHub for a newer release. Xenon only says one is out and links to
//! it; downloading and installing it is up to you.

use std::time::Duration;

use gpui::{AppContext as _, Context, SharedString};

use crate::workspace::Workspace;

const LATEST: &str = "https://api.github.com/repos/0mjs/xenon/releases/latest";
/// The launch check runs at most this often.
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);
const LAST_CHECK: &str = "update_checked_at";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Release {
    pub version: SharedString,
    /// The release page, with the download.
    pub url: SharedString,
}

/// What the status bar says about updates.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum UpdateState {
    #[default]
    Idle,
    Checking,
    Available(Release),
    /// After asking: you have the latest.
    Latest,
    Failed(SharedString),
}

/// The latest published release. Blocking: run it off the main thread.
fn latest() -> anyhow::Result<Release> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .new_agent();
    let body = agent
        .get(LATEST)
        .header("User-Agent", concat!("Xenon/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .call()?
        .body_mut()
        .read_to_string()?;
    let json: serde_json::Value = serde_json::from_str(&body)?;
    let field = |name: &str| {
        json[name]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("GitHub's reply has no {name}"))
    };
    Ok(Release {
        version: field("tag_name")?
            .trim_start_matches('v')
            .to_string()
            .into(),
        url: field("html_url")?.into(),
    })
}

/// Whether `candidate` is a later version than `current` (both `major.minor.patch`).
fn newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> Option<(u64, u64, u64)> {
        let mut it = v.trim_start_matches('v').split('.').map(|p| p.parse().ok());
        Some((it.next()??, it.next()??, it.next()??))
    };
    match (parts(candidate), parts(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

impl Workspace {
    /// At launch: check, if allowed and not done in the last day.
    pub(crate) fn check_for_updates_at_launch(&mut self, cx: &mut Context<Self>) {
        // Tests never reach the network.
        if !self.prefs.check_updates || cfg!(test) {
            return;
        }
        let now = unix_now();
        let last = self
            .store
            .as_ref()
            .and_then(|store| store.setting(LAST_CHECK).ok().flatten())
            .and_then(|value| value.parse::<u64>().ok());
        if last.is_some_and(|last| now.saturating_sub(last) < EVERY.as_secs()) {
            return;
        }
        self.check_for_updates(false, cx);
    }

    /// Ask GitHub for the latest release. `asked`: from the menu, so say when
    /// there's nothing new or the check failed; at launch, stay quiet unless
    /// there's an update.
    pub(crate) fn check_for_updates(&mut self, asked: bool, cx: &mut Context<Self>) {
        if self.update == UpdateState::Checking {
            return;
        }
        if asked {
            self.update = UpdateState::Checking;
            cx.notify();
        }
        let task = cx.background_spawn(async move { latest() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if let (Ok(_), Some(store)) = (&result, &this.store) {
                    let _ = store.set_setting(LAST_CHECK, &unix_now().to_string());
                }
                this.update = match result {
                    Ok(release) if newer(&release.version, CURRENT) => {
                        UpdateState::Available(release)
                    }
                    Ok(_) if asked => UpdateState::Latest,
                    Err(err) if asked => {
                        UpdateState::Failed(format!("Couldn't check for updates: {err}").into())
                    }
                    _ => UpdateState::Idle,
                };
                // "Up to date" and failures fade after a few seconds.
                if matches!(this.update, UpdateState::Latest | UpdateState::Failed(_)) {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_secs(6)).await;
                        this.update(cx, |this, cx| {
                            if matches!(this.update, UpdateState::Latest | UpdateState::Failed(_)) {
                                this.update = UpdateState::Idle;
                                cx.notify();
                            }
                        })
                        .ok();
                    })
                    .detach();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(newer("0.4.1", "0.4.0"));
        assert!(newer("v0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.0"));
        assert!(!newer("0.4.0", "0.4.0"));
        assert!(!newer("0.3.9", "0.4.0"));
        // Anything unexpected is never "newer".
        assert!(!newer("0.5.0-beta", "0.4.0"));
        assert!(!newer("nightly", "0.4.0"));
    }

    /// Talks to GitHub: run with `--ignored`.
    #[test]
    #[ignore]
    fn reads_the_latest_release() {
        let release = latest().expect("GitHub answers");
        assert!(newer(&release.version, "0.0.1"), "{release:?}");
        assert!(
            release
                .url
                .starts_with("https://github.com/0mjs/xenon/releases/")
        );
    }
}
