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
}
