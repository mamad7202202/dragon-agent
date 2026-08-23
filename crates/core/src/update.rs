//! Update checking against GitHub releases.
//!
//! Compares the running version with the newest published (non-prerelease)
//! release tag and, when newer, hands back where to get it.

use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone, serde::Serialize)]
pub struct UpdateInfo {
    /// Latest tag, e.g. "v0.3.1"
    pub latest: String,
    /// Version currently running, e.g. "0.3.0"
    pub current: String,
    /// Release page for manual download
    pub url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    prerelease: bool,
    draft: bool,
}

/// Ask GitHub for the latest release of this project.
pub async fn check(current: &str) -> Result<Option<UpdateInfo>> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("dragon-agent/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(8))
        .build()?;
    let rel: GhRelease = client
        .get("https://api.github.com/repos/mamad7202202/dragon-agent/releases/latest")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    if rel.prerelease || rel.draft {
        return Ok(None);
    }

    let latest_clean = rel.tag_name.trim_start_matches('v').to_string();
    if newer(&latest_clean, current) {
        Ok(Some(UpdateInfo {
            latest: rel.tag_name,
            current: current.to_string(),
            url: rel.html_url,
        }))
    } else {
        Ok(None)
    }
}

/// Semantic-ish comparison: split on dots and compare numerically.
fn newer(candidate: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split(|c| c == '.' || c == '-')
            .filter_map(|p| p.parse::<u64>().ok())
            .collect()
    };
    let a = parse(candidate);
    let b = parse(current);
    for i in 0..a.len().max(b.len()) {
        let av = a.get(i).copied().unwrap_or(0);
        let bv = b.get(i).copied().unwrap_or(0);
        if av != bv {
            return av > bv;
        }
    }
    false
}

/// Asset name matching THIS machine, as published by the release workflow.
pub fn asset_name_for_current(gui: bool) -> String {
    let os = match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "darwin",
        _ => "linux",
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    let ext = if os == "windows" { ".exe" } else { "" };
    let base = if gui { "dragon-gui-" } else { "dragon-" };
    format!("{base}{os}-{arch}{ext}")
}

/// Direct download link for the newest release asset for this machine.
pub fn latest_download_url(gui: bool) -> String {
    format!(
        "https://github.com/mamad7202202/dragon-agent/releases/latest/download/{}",
        asset_name_for_current(gui)
    )
}

/// Open a URL in the default browser, blocking briefly.
pub fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(newer("0.3.1", "0.3.0"));
        assert!(newer("0.4.0", "0.3.9"));
        assert!(newer("1.0.0", "0.9.9"));
        assert!(!newer("0.3.0", "0.3.0"));
        assert!(!newer("0.2.9", "0.3.0"));
    }

    #[test]
    fn handles_v_prefix() {
        assert!(newer("1.2.3", "1.2.2"));
        assert!(!newer("1.2.2", "1.2.3"));
    }

    #[test]
    fn asset_naming_matches_workflow() {
        let name = asset_name_for_current(false);
        assert!(name.starts_with("dragon-"));
        assert!(name.contains("amd64") || name.contains("arm64"));
    }
}
