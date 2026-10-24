use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

/// Typed failure surface for release discovery and version comparison.
///
/// Every public fallible function in this crate returns this enum so
/// consumers (the CLI updater, `codesmith doctor`) can match on the failure
/// kind instead of re-parsing message strings; `anyhow` stays confined to
/// the consuming binaries.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReleaseError {
    /// A release mirror was selected via environment override, but
    /// `CODESMITH_VERSION` was not pinned, so the latest release cannot be
    /// derived from a mirror directory.
    #[error(
        "release mirror selected (CODESMITH_RELEASE_BASE_URL / \
         CODESMITH_USE_CNB_MIRROR) but CODESMITH_VERSION is not pinned; \
         cannot determine the latest release from a mirror directory"
    )]
    MirrorVersionNotPinned,
    /// The metadata HTTP client could not be built.
    #[error("failed to build release check HTTP client")]
    HttpClientBuild,
    /// The metadata request itself failed (network, DNS, TLS).
    #[error("failed to fetch {description} from {url}")]
    Http {
        url: String,
        description: String,
        #[source]
        source: reqwest::Error,
    },
    /// The metadata response body could not be read.
    #[error("failed to read {description} response from {url}")]
    HttpBody {
        url: String,
        description: String,
        #[source]
        source: reqwest::Error,
    },
    /// GitHub answered a non-success status; `snippet` is a bounded excerpt
    /// of the failure body (a proxy's HTML error page can be megabytes).
    #[error("GitHub release request failed with HTTP {status}: {snippet}")]
    HttpStatus {
        status: reqwest::StatusCode,
        snippet: String,
    },
    /// A release JSON payload failed to decode; `snippet` is a bounded
    /// excerpt of the offending body.
    #[error("failed to parse release JSON from GitHub API. Response: {snippet}")]
    Parse {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },
    /// The release list carried no beta-tagged entry.
    #[error("no beta release found in GitHub releases")]
    NoBetaRelease,
    /// The current version string is not valid semver.
    #[error("failed to parse current version {value:?}")]
    InvalidCurrentVersion {
        value: String,
        #[source]
        source: semver::Error,
    },
    /// The latest release tag is not valid semver.
    #[error("failed to parse latest release tag {value:?}")]
    InvalidLatestTag {
        value: String,
        #[source]
        source: semver::Error,
    },
    /// A version string is not valid semver.
    #[error("invalid semver: {value:?}")]
    InvalidVersion {
        value: String,
        #[source]
        source: semver::Error,
    },
}

/// Result alias for this crate's public surface: [`ReleaseError`] by default.
pub type Result<T, E = ReleaseError> = std::result::Result<T, E>;

/// Filename of the SHA-256 checksum manifest included in every release.
///
/// Mirror directories must contain this file alongside platform binaries so
/// that download integrity can be verified.
pub const CHECKSUM_MANIFEST_ASSET: &str = "codesmith-artifacts-sha256.txt";

/// GitHub API URL for the single latest stable release.
pub const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/camilesing/CodeSmith/releases/latest";

/// GitHub API URL listing recent releases (up to 100), used to find beta tags.
pub const RELEASES_URL: &str =
    "https://api.github.com/repos/camilesing/CodeSmith/releases?per_page=100";

/// Base URL of the CodeSmith repository on the CNB mirror platform.
pub const CNB_REPO_URL: &str = "https://cnb.cool/codesmith.net/codesmith";

/// Environment variable that overrides the base URL for release asset downloads.
pub const RELEASE_BASE_URL_ENV: &str = "CODESMITH_RELEASE_BASE_URL";

/// Environment variable that, when set, enables the CNB mirror for downloads.
pub const CNB_MIRROR_ENV: &str = "CODESMITH_USE_CNB_MIRROR";

/// Environment variable that pins the update target version.
pub const UPDATE_VERSION_ENV: &str = "CODESMITH_VERSION";

/// User-Agent header sent with release metadata requests.
pub const UPDATE_USER_AGENT: &str = "codesmith-updater";

const CNB_RELEASE_ASSET_BASE: &str = "https://cnb.cool/camilesing/CodeSmith/-/releases";
const RELEASE_METADATA_TIMEOUT: Duration = Duration::from_secs(5);

/// The release channel to query for updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseChannel {
    /// Official stable releases only.
    Stable,
    /// Pre-release / beta versions.
    Beta,
}

impl ReleaseChannel {
    /// Creates a channel from a boolean flag (`true` → [`Beta`](Self::Beta)).
    pub fn from_beta_flag(beta: bool) -> Self {
        if beta { Self::Beta } else { Self::Stable }
    }

    /// Returns a lowercase human-readable label (`"stable"` or `"beta"`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
        }
    }
}

/// Describes where to fetch release metadata from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseQuery {
    /// Use a custom mirror base URL and a pinned version.
    Mirror { base_url: String, version: String },
    /// Query the GitHub single-latest-release endpoint.
    GitHubLatest { url: &'static str },
    /// Query the GitHub release-list endpoint (used for beta discovery).
    GitHubReleaseList { url: &'static str },
}

/// Determines the appropriate [`ReleaseQuery`] for the given channel, taking
/// environment-variable overrides (mirror URL, pinned version) into account.
/// Fails when a mirror is selected but `CODESMITH_VERSION` is not pinned:
/// falling back to the running version would make `latest` == `current` and
/// the update check a permanent silent no-op.
pub fn resolve_release_query(channel: ReleaseChannel) -> Result<ReleaseQuery> {
    // The base URL is probed with the running version only to satisfy the
    // signature; the pinned version below is what the Mirror query carries.
    if let Some(base_url) = release_base_url_from_env(env!("CARGO_PKG_VERSION")) {
        let version = update_version_from_env().ok_or(ReleaseError::MirrorVersionNotPinned)?;
        return Ok(ReleaseQuery::Mirror { base_url, version });
    }

    Ok(match channel {
        ReleaseChannel::Stable => ReleaseQuery::GitHubLatest {
            url: LATEST_RELEASE_URL,
        },
        ReleaseChannel::Beta => ReleaseQuery::GitHubReleaseList { url: RELEASES_URL },
    })
}

/// Reads the release base URL from environment variables, falling back to the
/// CNB mirror if `CODESMITH_USE_CNB_MIRROR` is set. Returns `None` when no
/// override is configured.
pub fn release_base_url_from_env(version: &str) -> Option<String> {
    for env_name in [RELEASE_BASE_URL_ENV] {
        if let Ok(value) = std::env::var(env_name) {
            let trimmed = value.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }

    // Empty counts as unset (script templates export `VAR=` unconditionally);
    // non-Unicode values are treated as unset, matching the base-URL handling.
    if let Ok(value) = std::env::var(CNB_MIRROR_ENV)
        && !value.trim().is_empty()
    {
        return Some(cnb_release_base_url(version));
    }
    None
}

/// Constructs the CNB mirror asset URL for a given version tag. Only a
/// single optional leading `v` is stripped (`vv1.2.3` keeps its second `v`).
pub fn cnb_release_base_url(version: &str) -> String {
    format!(
        "{}/v{}",
        CNB_RELEASE_ASSET_BASE.trim_end_matches('/'),
        version.strip_prefix('v').unwrap_or(version)
    )
}

/// Returns the pinned update version from the `CODESMITH_VERSION` environment
/// variable, or `None` when unset. Only a single optional leading `v` is
/// stripped, so `vv1.2.3` stays `v1.2.3` and surfaces as an invalid pin
/// downstream instead of silently becoming `1.2.3`.
pub fn update_version_from_env() -> Option<String> {
    std::env::var(UPDATE_VERSION_ENV)
        .ok()
        .map(|value| {
            let trimmed = value.trim();
            trimmed.strip_prefix('v').unwrap_or(trimmed).to_string()
        })
        .filter(|value| !value.is_empty())
}

/// Joins a mirror base URL with an asset filename to produce a full download URL.
pub fn mirror_asset_url(base_url: &str, asset_name: &str) -> String {
    format!("{}/{}", base_url.trim_end_matches('/'), asset_name)
}

/// Returns a human-readable hint explaining how to use a mirror when GitHub
/// downloads are blocked or slow (e.g. on mainland China networks).
pub fn update_network_fallback_hint() -> String {
    format!(
        "GitHub release downloads may be blocked or slow on this network.\n\
         For mainland China, use one of these fallback paths:\n\
           1. Source build from the CNB mirror (the single shipped binary):\n\
              cargo install --git {CNB_REPO_URL} --tag vX.Y.Z codesmith-cli --locked --force\n\
           2. Use a binary asset mirror:\n\
              {RELEASE_BASE_URL_ENV}=https://<mirror>/<release-assets>/ {UPDATE_VERSION_ENV}=X.Y.Z codesmith update\n\
         The mirror directory must contain {CHECKSUM_MANIFEST_ASSET} and the platform binaries."
    )
}

/// Cached blocking metadata client (fresh TLS config + connection pool are
/// expensive to rebuild per call). `None` inside the `OnceLock` records a
/// build failure so the same error surfaces on every call.
#[cfg(feature = "blocking")]
fn blocking_metadata_client() -> Result<&'static reqwest::blocking::Client> {
    static CLIENT: OnceLock<Option<reqwest::blocking::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .user_agent(UPDATE_USER_AGENT)
                .timeout(RELEASE_METADATA_TIMEOUT)
                .build()
                .ok()
        })
        .as_ref()
        .ok_or(ReleaseError::HttpClientBuild)
}

/// Cached async metadata client — the async twin of
/// [`blocking_metadata_client`].
fn async_metadata_client() -> Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(UPDATE_USER_AGENT)
                .timeout(RELEASE_METADATA_TIMEOUT)
                .build()
                .ok()
        })
        .as_ref()
        .ok_or(ReleaseError::HttpClientBuild)
}

/// Fetches a release JSON payload from `url` using a blocking HTTP client.
///
/// `description` is included in error messages to identify the request
/// purpose. Only available with the `blocking` crate feature.
#[cfg(feature = "blocking")]
pub fn fetch_release_json_blocking(url: &str, description: &str) -> Result<String> {
    let client = blocking_metadata_client()?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .map_err(|source| ReleaseError::Http {
            url: url.to_string(),
            description: description.to_string(),
            source,
        })?;
    let status = response.status();
    let body = response.text().map_err(|source| ReleaseError::HttpBody {
        url: url.to_string(),
        description: description.to_string(),
        source,
    });
    release_response_body(status, body)
}

/// Async counterpart of [`fetch_release_json_blocking`].
pub async fn fetch_release_json_async(url: &str, description: &str) -> Result<String> {
    let client = async_metadata_client()?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|source| ReleaseError::Http {
            url: url.to_string(),
            description: description.to_string(),
            source,
        })?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|source| ReleaseError::HttpBody {
            url: url.to_string(),
            description: description.to_string(),
            source,
        });
    release_response_body(status, body)
}

fn release_response_body(status: reqwest::StatusCode, body: Result<String>) -> Result<String> {
    let body = body?;
    if !status.is_success() {
        // Bounded snippet: the failure body is unbounded (a proxy's HTML
        // error page can be megabytes); chars() avoids slicing mid-UTF-8.
        let snippet: String = body.chars().take(512).collect();
        return Err(ReleaseError::HttpStatus { status, snippet });
    }
    Ok(body)
}

#[derive(Deserialize)]
struct ReleaseTag {
    tag_name: String,
}

#[derive(Deserialize)]
struct ReleaseListEntry {
    tag_name: String,
}

/// Extracts the `tag_name` field from a GitHub single-release JSON response.
pub fn latest_tag_from_release_json(body: &str) -> Result<String> {
    let release: ReleaseTag = serde_json::from_str(body).map_err(|source| ReleaseError::Parse {
        snippet: body.chars().take(512).collect(),
        source,
    })?;
    Ok(release.tag_name)
}

/// Scans a GitHub release-list JSON response and returns the tag of the first
/// entry whose name contains `"beta"`.
pub fn latest_beta_tag_from_release_list_json(body: &str) -> Result<String> {
    let releases: Vec<ReleaseListEntry> =
        serde_json::from_str(body).map_err(|source| ReleaseError::Parse {
            snippet: body.chars().take(512).collect(),
            source,
        })?;
    releases
        .into_iter()
        .filter_map(|release| {
            // Parse-gated: a tag like `v1.5.0+beta` (substring match, empty
            // pre-release) or a malformed `beta-2024.10` must not be
            // selected here only to disagree with (or fail inside)
            // `update_is_needed` later.
            let tag = release.tag_name;
            let version = parse_release_version(&tag).ok()?;
            version_is_beta(&version).then_some(tag)
        })
        .next()
        .ok_or(ReleaseError::NoBetaRelease)
}

/// Async helper that resolves the latest release tag for the given channel.
///
/// For mirrors the version is derived from the pinned environment variable;
/// for GitHub channels the appropriate API endpoint is queried.
pub async fn latest_release_tag_async(channel: ReleaseChannel) -> Result<String> {
    resolve_latest_tag(channel, |url, description| {
        fetch_release_json_async(url, description)
    })
    .await
}

/// Blocking counterpart of [`latest_release_tag_async`]. Only available with
/// the `blocking` crate feature.
#[cfg(feature = "blocking")]
pub fn latest_release_tag_blocking(channel: ReleaseChannel) -> Result<String> {
    block_on(resolve_latest_tag(channel, |url, description| {
        std::future::ready(fetch_release_json_blocking(url, description))
    }))
}

/// Shared dispatch + parse chain behind the two `latest_release_tag_*`
/// wrappers, parameterized over the fetch mechanism so fixes to query
/// handling land in exactly one place.
async fn resolve_latest_tag<F, Fut>(channel: ReleaseChannel, fetch: F) -> Result<String>
where
    F: Fn(&'static str, &'static str) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    match resolve_release_query(channel)? {
        ReleaseQuery::Mirror { version, .. } => Ok(format!("v{version}")),
        ReleaseQuery::GitHubLatest { url } => {
            latest_tag_from_release_json(&fetch(url, "latest release").await?)
        }
        ReleaseQuery::GitHubReleaseList { url } => {
            latest_beta_tag_from_release_list_json(&fetch(url, "release list").await?)
        }
    }
}

/// Drives `fut` to completion on the current thread for the blocking
/// wrapper. The futures passed in wrap fully blocking calls in
/// `std::future::ready`, so a single poll completes them; the yield loop
/// only guards against that ever changing.
#[cfg(feature = "blocking")]
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    let mut fut = std::pin::pin!(fut);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        match fut.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(out) => return out,
            std::task::Poll::Pending => std::thread::yield_now(),
        }
    }
}

/// Compares a current version string against a release tag using semver
/// ordering. Both `v` prefixes and trailing build metadata (e.g. `(abc123)`)
/// are stripped before comparison.
pub fn compare_release_versions(
    current_version: &str,
    latest_tag: &str,
) -> Result<std::cmp::Ordering> {
    let current =
        parse_version(current_version).map_err(|source| ReleaseError::InvalidCurrentVersion {
            value: current_version.to_string(),
            source,
        })?;
    let latest = parse_version(latest_tag).map_err(|source| ReleaseError::InvalidLatestTag {
        value: latest_tag.to_string(),
        source,
    })?;
    Ok(current.cmp(&latest))
}

/// Determines whether an update is needed for the given channel.
///
/// For [`Stable`](ReleaseChannel::Stable) an update is needed when the latest
/// release is strictly newer — including when the latest tag carries a
/// pre-release. That is safe for the GitHub flow because the
/// `/releases/latest` endpoint never returns pre-releases; on the Stable
/// channel a pre-release tag can otherwise only arrive through an explicitly
/// pinned mirror version, which is a deliberate user choice. For
/// [`Beta`](ReleaseChannel::Beta) the logic also allows switching from a
/// stable release to a beta on the same release line; a beta user is never
/// auto-upgraded to the final stable release of that same line (the beta
/// channel tracks betas).
pub fn update_is_needed(
    channel: ReleaseChannel,
    current_version: &str,
    latest_tag: &str,
) -> Result<bool> {
    let current =
        parse_version(current_version).map_err(|source| ReleaseError::InvalidCurrentVersion {
            value: current_version.to_string(),
            source,
        })?;
    let latest = parse_version(latest_tag).map_err(|source| ReleaseError::InvalidLatestTag {
        value: latest_tag.to_string(),
        source,
    })?;

    match channel {
        ReleaseChannel::Stable => Ok(current < latest),
        ReleaseChannel::Beta => {
            if current == latest {
                return Ok(false);
            }
            let latest_is_beta = version_is_beta(&latest);
            let current_is_stable = current.pre.is_empty();
            let same_release_line = current.major == latest.major
                && current.minor == latest.minor
                && current.patch == latest.patch;
            if current > latest && !(current_is_stable && same_release_line) {
                return Ok(false);
            }
            Ok(latest_is_beta)
        }
    }
}

/// Parses a version string (with optional `v` prefix and trailing build info)
/// into a [`semver::Version`].
pub fn parse_release_version(value: &str) -> Result<semver::Version> {
    parse_version(value).map_err(|source| ReleaseError::InvalidVersion {
        value: value.to_string(),
        source,
    })
}

/// Version core parser shared by the public parse and the side-tagging
/// variants above. Strips at most ONE leading `v`: `vv1.2.3` must surface
/// as an invalid pin downstream (the documented contract of
/// `update_version_from_env` and `cnb_release_base_url`), not silently
/// parse as `1.2.3`.
fn parse_version(value: &str) -> std::result::Result<semver::Version, semver::Error> {
    let trimmed = value.trim();
    let version = trimmed
        .strip_prefix('v')
        .unwrap_or(trimmed)
        .split_whitespace()
        .next()
        .unwrap_or("");
    semver::Version::parse(version)
}

/// Returns `true` if the tag name contains `"beta"` (case-insensitive).
pub fn is_beta_tag(tag_name: &str) -> bool {
    tag_name.to_ascii_lowercase().contains("beta")
}

/// Returns `true` if the version's pre-release segment contains `"beta"`
/// (case-insensitive) — the semver-grounded form of [`is_beta_tag`].
pub fn version_is_beta(version: &semver::Version) -> bool {
    version.pre.as_str().to_ascii_lowercase().contains("beta")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cnb_release_base_url_includes_tag_directory() {
        assert_eq!(
            cnb_release_base_url("0.8.47"),
            "https://cnb.cool/camilesing/CodeSmith/-/releases/v0.8.47"
        );
        assert_eq!(
            cnb_release_base_url("v0.8.47"),
            "https://cnb.cool/camilesing/CodeSmith/-/releases/v0.8.47"
        );
        // Only ONE leading `v` is stripped: `vv…` keeps its second `v`
        // (an invalid pin stays visibly invalid).
        assert_eq!(
            cnb_release_base_url("vv0.8.47"),
            "https://cnb.cool/camilesing/CodeSmith/-/releases/vv0.8.47"
        );
    }

    #[test]
    fn stable_update_is_needed_only_when_latest_is_newer() {
        assert!(update_is_needed(ReleaseChannel::Stable, "0.8.45", "v0.8.46").unwrap());
        assert!(update_is_needed(ReleaseChannel::Stable, "0.8.45", "v0.9.0-beta.1").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Stable, "0.8.45", "v0.8.45").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Stable, "0.9.0", "v0.9.0-beta.1").unwrap());
        assert!(
            !update_is_needed(ReleaseChannel::Stable, "0.9.0-beta.2", "v0.9.0-beta.1").unwrap()
        );
    }

    #[test]
    fn beta_update_allows_switching_from_same_stable_to_beta() {
        assert!(update_is_needed(ReleaseChannel::Beta, "1.0.0", "v1.0.0-beta.2").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Beta, "1.0.0-beta.2", "v1.0.0-beta.2").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Beta, "1.0.0-beta.3", "v1.0.0-beta.2").unwrap());
        assert!(update_is_needed(ReleaseChannel::Beta, "1.0.0-beta.2", "v1.0.0-beta.3").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Beta, "2.0.0", "v1.0.0-beta.3").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Beta, "1.0.0-rc.1", "v1.0.0-beta.3").unwrap());
    }

    #[test]
    fn parse_release_version_accepts_tags_and_build_suffixes() {
        assert_eq!(
            parse_release_version("v0.9.0-beta.1").unwrap(),
            semver::Version::parse("0.9.0-beta.1").unwrap()
        );
        assert_eq!(
            parse_release_version("0.8.45 (abcdef123456)").unwrap(),
            semver::Version::parse("0.8.45").unwrap()
        );
    }

    #[test]
    fn release_version_compare_ignores_v_prefix_and_build_sha() {
        assert_eq!(
            compare_release_versions("0.8.39 (eeccf7d)", "v0.8.39").unwrap(),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            compare_release_versions("0.8.39", "v0.8.40").unwrap(),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_release_versions("0.8.40", "v0.8.39").unwrap(),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn latest_beta_tag_selects_first_beta_release() {
        let body = r#"[
          { "tag_name": "v0.9.0" },
          { "tag_name": "v0.9.0-rc.1" },
          { "tag_name": "v0.9.0-beta.2" },
          { "tag_name": "v0.9.0-beta.1" }
        ]"#;
        assert_eq!(
            latest_beta_tag_from_release_list_json(body).unwrap(),
            "v0.9.0-beta.2"
        );
    }

    #[test]
    fn latest_beta_tag_reports_missing_beta() {
        let body = r#"[{ "tag_name": "v0.9.0" }]"#;
        let err = latest_beta_tag_from_release_list_json(body).expect_err("missing beta");
        assert!(
            err.to_string().contains("no beta release found"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn release_errors_are_matchable_by_kind() {
        assert!(matches!(
            parse_release_version("not-a-version"),
            Err(ReleaseError::InvalidVersion { .. })
        ));
        assert!(matches!(
            latest_tag_from_release_json("<html>not json</html>"),
            Err(ReleaseError::Parse { .. })
        ));
        assert!(matches!(
            latest_beta_tag_from_release_list_json(r#"[{ "tag_name": "v0.9.0" }]"#),
            Err(ReleaseError::NoBetaRelease)
        ));
        assert!(matches!(
            compare_release_versions("oops", "v0.9.0"),
            Err(ReleaseError::InvalidCurrentVersion { .. })
        ));
        assert!(matches!(
            compare_release_versions("0.9.0", "oops"),
            Err(ReleaseError::InvalidLatestTag { .. })
        ));
    }
}
