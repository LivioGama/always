//! The "is there a newer release?" half of the updater (SPEC §12a Check).
//!
//! One anonymous GET of `releases/latest`, a semver comparison, and a
//! classification of every way that can go wrong. Nothing here installs
//! anything.

use std::io::Read as _;
use std::time::Duration;

use semver::Version;
use serde::Deserialize;

pub const REPO: &str = "LivioGama/always";
const API_LATEST: &str = "https://api.github.com/repos/LivioGama/always/releases/latest";
const DOWNLOAD_BASE: &str = "https://github.com/LivioGama/always/releases/download";
/// A release JSON is a few KB; this only bounds a hostile or broken server.
const MAX_API_BODY: u64 = 1 << 20;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Where the updater talks to. `github()` in production; tests point it at a
/// local mock server (`allow_http`).
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub api_latest: String,
    /// `<base>/<tag>/<asset>` is an asset URL.
    pub download_base: String,
    pub allow_http: bool,
}

impl Endpoints {
    pub fn github() -> Self {
        Self {
            api_latest: API_LATEST.to_string(),
            download_base: DOWNLOAD_BASE.to_string(),
            allow_http: false,
        }
    }
}

/// The running build's version.
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0))
}

/// A published, stable, semver-tagged release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The git tag exactly as published: `v` + semver (`v0.2.0`).
    pub tag: String,
    pub version: Version,
    /// The release page.
    pub html_url: String,
    /// Names of the files attached to the release.
    pub assets: Vec<String>,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
}

/// Why a release the API returned is not one we would offer.
#[derive(Debug, PartialEq, Eq)]
pub enum Skip {
    Draft,
    Prerelease,
    /// The tag is not `v` + semver, so it cannot be compared (or trusted as
    /// a path component).
    BadTag(String),
    Malformed(String),
}

impl std::fmt::Display for Skip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Draft => write!(f, "latest release is a draft"),
            Self::Prerelease => write!(f, "latest release is a prerelease"),
            Self::BadTag(t) => write!(f, "release tag '{t}' is not vMAJOR.MINOR.PATCH"),
            Self::Malformed(e) => write!(f, "unreadable release JSON ({e})"),
        }
    }
}

/// Parse the body of `releases/latest`.
pub fn parse_release(body: &[u8]) -> Result<Release, Skip> {
    let raw: ApiRelease =
        serde_json::from_slice(body).map_err(|e| Skip::Malformed(e.to_string()))?;
    if raw.draft {
        return Err(Skip::Draft);
    }
    if raw.prerelease {
        return Err(Skip::Prerelease);
    }
    let version = raw
        .tag_name
        .strip_prefix('v')
        .and_then(|v| Version::parse(v).ok())
        .ok_or_else(|| Skip::BadTag(raw.tag_name.clone()))?;
    // Only the canonical spelling: the tag becomes a directory name and a
    // URL path segment, so nothing but semver characters may pass.
    if format!("v{version}") != raw.tag_name {
        return Err(Skip::BadTag(raw.tag_name));
    }
    if !version.pre.is_empty() {
        return Err(Skip::Prerelease);
    }
    Ok(Release {
        tag: raw.tag_name,
        version,
        html_url: raw.html_url,
        assets: raw.assets.into_iter().map(|a| a.name).collect(),
    })
}

/// What one check learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// The newest published release is not newer than this build.
    UpToDate { latest: Version },
    /// A newer release is published.
    Available(Release),
    /// HTTP 404: no release has been published yet.
    NoRelease,
    /// The API returned something we deliberately ignore (draft, prerelease,
    /// non-semver tag, unreadable JSON).
    Skipped(String),
    /// Could not reach GitHub (DNS, connect, timeout).
    Offline(String),
    /// GitHub's anonymous rate limit.
    RateLimited { retry_after_secs: Option<u64> },
    /// Anything else (unexpected status, TLS failure, ...).
    Failed(String),
}

impl CheckOutcome {
    /// The wire / log value (`UpdateCheckDone.result`, `update_check_done`).
    pub fn result(&self) -> &'static str {
        match self {
            Self::UpToDate { .. } => "up_to_date",
            Self::Available(_) => "update_available",
            Self::NoRelease | Self::Skipped(_) => "no_release",
            Self::Offline(_) => "offline",
            Self::RateLimited { .. } => "rate_limited",
            Self::Failed(_) => "error",
        }
    }

    /// True when the answer is final for the next 24 h (not a transient
    /// failure that deserves an earlier retry).
    pub fn is_definitive(&self) -> bool {
        matches!(
            self,
            Self::UpToDate { .. } | Self::Available(_) | Self::NoRelease | Self::Skipped(_)
        )
    }

    /// The newest version this check learned, `""` if none.
    pub fn latest_string(&self) -> String {
        match self {
            Self::UpToDate { latest } => latest.to_string(),
            Self::Available(r) => r.version.to_string(),
            _ => String::new(),
        }
    }

    /// One human sentence for the CLI.
    pub fn describe(&self, current: &Version) -> String {
        match self {
            Self::UpToDate { .. } => format!("Always {current} is up to date."),
            Self::Available(r) => format!("Update available: {current} -> {}", r.version),
            Self::NoRelease => {
                format!("No published release yet; Always {current} is the newest there is.")
            }
            Self::Skipped(why) => format!("No usable release published ({why})."),
            Self::Offline(e) => format!("Could not reach GitHub: {e}"),
            Self::RateLimited { retry_after_secs } => match retry_after_secs {
                Some(s) => format!("GitHub rate limit reached; try again in about {s} s."),
                None => "GitHub rate limit reached; try again later.".to_string(),
            },
            Self::Failed(e) => format!("Update check failed: {e}"),
        }
    }
}

/// Turn an HTTP answer into a [`CheckOutcome`]. Pure, so every branch is
/// unit-tested without a network.
pub fn interpret(
    status: u16,
    rate_limit_exhausted: bool,
    retry_after_secs: Option<u64>,
    body: &[u8],
    current: &Version,
) -> CheckOutcome {
    match status {
        200 => match parse_release(body) {
            Ok(release) if release.version > *current => CheckOutcome::Available(release),
            Ok(release) => CheckOutcome::UpToDate {
                latest: release.version,
            },
            Err(skip) => CheckOutcome::Skipped(skip.to_string()),
        },
        404 => CheckOutcome::NoRelease,
        429 => CheckOutcome::RateLimited { retry_after_secs },
        403 if rate_limit_exhausted || retry_after_secs.is_some() => {
            CheckOutcome::RateLimited { retry_after_secs }
        }
        other => CheckOutcome::Failed(format!("GitHub answered HTTP {other}")),
    }
}

/// User-Agent for every request the updater makes. GitHub requires one; it
/// carries the app version and nothing about the user.
pub fn user_agent() -> String {
    format!(
        "always/{} (+https://github.com/{REPO})",
        env!("CARGO_PKG_VERSION")
    )
}

/// GET `releases/latest` once and classify the answer. Blocking: call it
/// from a plain thread, never from inside the tokio runtime.
pub fn fetch_latest(endpoints: &Endpoints, current: &Version) -> CheckOutcome {
    if !endpoints.allow_http && !endpoints.api_latest.starts_with("https://") {
        return CheckOutcome::Failed("refusing a non-https update endpoint".to_string());
    }
    let client = match reqwest::blocking::Client::builder()
        .user_agent(user_agent())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => return CheckOutcome::Failed(format!("cannot build HTTP client: {e}")),
    };
    let response = match client
        .get(&endpoints.api_latest)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
    {
        Ok(r) => r,
        Err(e) => return classify_transport_error(e),
    };
    let status = response.status().as_u16();
    let header_u64 = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    let exhausted = header_u64("x-ratelimit-remaining") == Some(0);
    let retry_after = header_u64("retry-after");
    let mut body = Vec::new();
    if response
        .take(MAX_API_BODY + 1)
        .read_to_end(&mut body)
        .is_err()
    {
        return CheckOutcome::Offline("connection lost while reading the answer".to_string());
    }
    if body.len() as u64 > MAX_API_BODY {
        return CheckOutcome::Failed("release answer is unreasonably large".to_string());
    }
    interpret(status, exhausted, retry_after, &body, current)
}

fn classify_transport_error(e: reqwest::Error) -> CheckOutcome {
    let offline = e.is_connect() || e.is_timeout();
    // The URL is the public API endpoint, but keep messages short.
    let detail = e.without_url().to_string();
    if offline {
        CheckOutcome::Offline(detail)
    } else {
        CheckOutcome::Failed(detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn body(tag: &str, draft: bool, prerelease: bool) -> Vec<u8> {
        serde_json::json!({
            "tag_name": tag,
            "draft": draft,
            "prerelease": prerelease,
            "html_url": format!("https://github.com/LivioGama/always/releases/tag/{tag}"),
            "assets": [{"name": "install.sh"}, {"name": "install.sh.minisig"}],
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn newer_release_is_available() {
        let out = interpret(200, false, None, &body("v0.2.0", false, false), &v("0.1.0"));
        let CheckOutcome::Available(r) = out else {
            panic!("expected Available, got {out:?}");
        };
        assert_eq!(r.tag, "v0.2.0");
        assert_eq!(r.version, v("0.2.0"));
        assert_eq!(r.assets, ["install.sh", "install.sh.minisig"]);
    }

    #[test]
    fn semver_ordering_not_string_ordering() {
        // 0.10.0 > 0.9.0 numerically although "0.10.0" < "0.9.0" as text.
        let out = interpret(
            200,
            false,
            None,
            &body("v0.10.0", false, false),
            &v("0.9.0"),
        );
        assert!(matches!(out, CheckOutcome::Available(_)));
        let out = interpret(
            200,
            false,
            None,
            &body("v0.9.0", false, false),
            &v("0.10.0"),
        );
        assert!(matches!(out, CheckOutcome::UpToDate { .. }));
    }

    #[test]
    fn equal_or_older_release_is_up_to_date_never_a_downgrade() {
        for tag in ["v0.1.0", "v0.0.9"] {
            let out = interpret(200, false, None, &body(tag, false, false), &v("0.1.0"));
            assert!(
                matches!(out, CheckOutcome::UpToDate { .. }),
                "{tag}: {out:?}"
            );
        }
    }

    #[test]
    fn drafts_prereleases_and_odd_tags_are_skipped() {
        for b in [
            body("v0.2.0", true, false),
            body("v0.2.0", false, true),
            body("v0.2.0-rc.1", false, false),
            body("0.2.0", false, false),
            body("v0.2", false, false),
            body("release-7", false, false),
            body("v0.2.0/../../x", false, false),
            b"{}".to_vec(),
            b"not json".to_vec(),
        ] {
            let out = interpret(200, false, None, &b, &v("0.1.0"));
            assert!(matches!(out, CheckOutcome::Skipped(_)), "{out:?}");
        }
    }

    #[test]
    fn http_statuses_are_classified() {
        let c = v("0.1.0");
        assert_eq!(
            interpret(404, false, None, b"", &c),
            CheckOutcome::NoRelease
        );
        assert_eq!(
            interpret(429, false, Some(30), b"", &c),
            CheckOutcome::RateLimited {
                retry_after_secs: Some(30)
            }
        );
        // 403 is a rate limit only when GitHub says so.
        assert!(matches!(
            interpret(403, true, None, b"", &c),
            CheckOutcome::RateLimited { .. }
        ));
        assert!(matches!(
            interpret(403, false, None, b"", &c),
            CheckOutcome::Failed(_)
        ));
        assert!(matches!(
            interpret(500, false, None, b"", &c),
            CheckOutcome::Failed(_)
        ));
    }

    #[test]
    fn result_strings_match_the_uds_contract() {
        let c = v("0.1.0");
        let r = |o: CheckOutcome| o.result();
        assert_eq!(
            r(CheckOutcome::UpToDate { latest: c.clone() }),
            "up_to_date"
        );
        assert_eq!(r(CheckOutcome::NoRelease), "no_release");
        assert_eq!(r(CheckOutcome::Offline(String::new())), "offline");
        assert_eq!(
            r(CheckOutcome::RateLimited {
                retry_after_secs: None
            }),
            "rate_limited"
        );
        assert_eq!(r(CheckOutcome::Failed(String::new())), "error");
        assert!(CheckOutcome::NoRelease.is_definitive());
        assert!(!CheckOutcome::Offline(String::new()).is_definitive());
    }

    /// The compiled-in version is itself valid semver (the comparison would
    /// otherwise silently treat every release as newer).
    #[test]
    fn current_version_is_real_semver() {
        assert_ne!(current_version(), Version::new(0, 0, 0));
    }
}
