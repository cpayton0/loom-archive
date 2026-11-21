use crate::{
    AnalyzeError, VideoDescriptor,
    page::{PageInfo, inspect_page},
};
use reqwest::{Client, StatusCode, redirect};
use std::{future::Future, net::IpAddr, time::Duration};
use tokio::{net::lookup_host, time::timeout};
use url::Url;

const HOST: &str = "www.loom.com";
const MAX_PAGE: usize = 4 * 1024 * 1024;
pub(crate) const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// The recording ID in a `share/` or `embed/` path segment: the whole segment,
/// or the end of a title slug such as `Weekly-sync-<id>`.
fn recording_id(segment: &str) -> Option<&str> {
    // The raw path can hold any text; byte slicing below needs ASCII.
    if !segment.is_ascii() {
        return None;
    }
    let hex = |s: &str| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit());
    if hex(segment) {
        return Some(segment);
    }
    let (slug, id) = segment.split_at(segment.len().checked_sub(32)?);
    let slug = slug.strip_suffix('-')?;
    (hex(id)
        && !slug.is_empty()
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'%')))
    .then_some(id)
}

pub fn normalize_share_url(input: &str) -> Result<Url, AnalyzeError> {
    let invalid = AnalyzeError::InvalidUrl;
    // Shells escape `?` and `=` in pasted links (`\?sid\=…`); upstream strips
    // backslashes before reading the link, and so does this port.
    let cleaned: String = input.trim().chars().filter(|c| *c != '\\').collect();
    let input = cleaned.as_str();
    if input.len() > 8192 || input.chars().any(char::is_control) {
        return Err(invalid);
    }
    let url = Url::parse(input).map_err(|_| invalid)?;
    let (_, rest) = input.split_once("://").ok_or(invalid)?;
    let path = rest.split(['?', '#']).next().ok_or(invalid)?;
    let (authority, path) = path.split_once('/').ok_or(invalid)?;
    if url.scheme() != "https"
        || !matches!(url.host_str(), Some("loom.com" | "www.loom.com"))
        || url.port().is_some()
        || authority.contains('@')
    {
        return Err(invalid);
    }
    let segment = path
        .strip_prefix("share/")
        .or_else(|| path.strip_prefix("embed/"))
        .ok_or(invalid)?;
    let segment = segment.strip_suffix('/').unwrap_or(segment);
    let id = recording_id(segment).ok_or(invalid)?;
    Url::parse(&format!("https://{HOST}/share/{}", id.to_ascii_lowercase())).map_err(|_| invalid)
}

pub async fn analyze_url(input: &str) -> Result<VideoDescriptor, AnalyzeError> {
    let page = resolve_recording(input).await?;
    page.media?;
    Ok(page.video)
}

async fn analyze_with<F, Fut>(
    input: &str,
    fetch: F,
) -> Result<PageInfo, AnalyzeError>
where
    F: FnOnce(Url) -> Fut,
    Fut: Future<Output = Result<String, AnalyzeError>>,
{
    let url = normalize_share_url(input)?;
    let page = fetch(url.clone()).await?;
    inspect_page(&url, &page)
}

pub(crate) async fn resolve_recording(
    input: &str,
) -> Result<PageInfo, AnalyzeError> {
    analyze_with(input, fetch_page).await
}

fn check_status(status: StatusCode) -> Result<(), AnalyzeError> {
    match status.as_u16() {
        200 => Ok(()),
        300..=399 => Err(AnalyzeError::Redirect),
        401 => Err(AnalyzeError::AuthenticationRequired),
        403 => Err(AnalyzeError::PermissionDenied),
        404 | 410 => Err(AnalyzeError::Unavailable),
        429 => Err(AnalyzeError::RateLimited),
        _ => Err(AnalyzeError::Network),
    }
}

// Pin the checked DNS results into the connector so the request cannot resolve
// the same name a second time to a local address.
fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_documentation()
                || a == 0
                || a >= 224
                || (a == 100 && (64..128).contains(&b))
                || (a == 198 && matches!(b, 18 | 19))
                || (a == 192 && ((b == 0 && c == 0) || (b == 88 && c == 99))))
        }
        IpAddr::V6(v6) => {
            let [a, b, ..] = v6.segments();
            (a & 0xe000) == 0x2000
                && a != 0x2002
                && !(a == 0x2001 && (b < 0x0200 || b == 0x0db8))
                && !(a == 0x3fff && b < 0x1000)
        }
    }
}

pub(crate) async fn pinned_client(host: &'static str) -> Result<Client, AnalyzeError> {
    let network = AnalyzeError::Network;
    let addrs: Vec<_> = timeout(Duration::from_secs(5), lookup_host((host, 443)))
        .await
        .map_err(|_| network)?
        .map_err(|_| network)?
        .filter(|addr| public_address(addr.ip()))
        .collect();
    if addrs.is_empty() {
        return Err(AnalyzeError::InvalidMediaUrl);
    }
    Client::builder()
        .no_proxy()
        .resolve_to_addrs(host, &addrs)
        .https_only(true)
        .redirect(redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .user_agent("LoomArchive/0.1")
        .build()
        .map_err(|_| network)
}

pub(crate) fn checked_media_host(url: &Url) -> Result<&'static str, AnalyzeError> {
    if url.port().is_some()
        || url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(AnalyzeError::InvalidMediaUrl);
    }
    match url.host_str() {
        Some("cdn.loom.com") => Ok("cdn.loom.com"),
        Some("luna.loom.com") => Ok("luna.loom.com"),
        _ => Err(AnalyzeError::InvalidMediaUrl),
    }
}

pub(crate) async fn fetch_media(url: Url) -> Result<reqwest::Response, AnalyzeError> {
    let response = pinned_client(checked_media_host(&url)?)
        .await?
        .get(url)
        .send()
        .await
        .map_err(|_| AnalyzeError::Network)?;
    validate_media_response(response).await
}

/// GET a Loom media host with the Referer and browser User-Agent upstream sends
/// for transcripts and thumbnails.
pub(crate) async fn fetch_asset(url: Url) -> Result<reqwest::Response, AnalyzeError> {
    let response = pinned_client(checked_media_host(&url)?)
        .await?
        .get(url)
        .header("Referer", "https://www.loom.com/")
        .header("User-Agent", BROWSER_UA)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|_| AnalyzeError::Network)?;
    validate_media_response(response).await
}

pub(crate) async fn read_limited(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, AnalyzeError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| AnalyzeError::Network)? {
        if chunk.len() > limit - bytes.len() {
            return Err(AnalyzeError::PageTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(crate) async fn validate_media_response(
    mut response: reqwest::Response,
) -> Result<reqwest::Response, AnalyzeError> {
    if response.status() == StatusCode::FORBIDDEN {
        // Only an explicit expiry code justifies suggesting that a fresh export will help.
        let mut body = Vec::new();
        while let Ok(Some(chunk)) = response.chunk().await {
            if chunk.len() > 8192 - body.len() {
                break;
            }
            body.extend_from_slice(&chunk);
        }
        let text = std::str::from_utf8(&body).unwrap_or_default();
        if ["<Code>ExpiredToken</Code>", "<Code>RequestExpired</Code>"]
            .iter()
            .any(|code| text.contains(code))
        {
            return Err(AnalyzeError::MediaAuthorizationExpired);
        }
    }
    check_status(response.status())?;
    Ok(response)
}

pub(crate) async fn fetch_page(url: Url) -> Result<String, AnalyzeError> {
    let network = AnalyzeError::Network;
    let mut response = pinned_client(HOST)
        .await?
        .get(url)
        .timeout(Duration::from_secs(20))
        // Same request headers as upstream's share-page fetch.
        .header("User-Agent", BROWSER_UA)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8",
        )
        .send()
        .await
        .map_err(|_| network)?;
    check_status(response.status())?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| network)? {
        if chunk.len() > MAX_PAGE - bytes.len() {
            return Err(AnalyzeError::PageTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| AnalyzeError::PageFormat)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "00112233445566778899aabbccddeeff";

    #[tokio::test]
    async fn fetches_only_the_canonical_page_and_returns_safe_metadata() {
        let link = format!(
            " https://LOOM.com/share/{}/?sid=discard#time ",
            ID.to_uppercase()
        );
        let video = analyze_with(&link, |url| async move {
            assert_eq!(url.as_str(), format!("https://www.loom.com/share/{ID}"));
            Ok(include_str!("../tests/fixtures/hls.html").into())
        })
        .await
        .unwrap()
        .video;
        assert_eq!(video.id, ID);
        assert_eq!(video.source_type, crate::SourceType::Hls);
        let wire = serde_json::to_value(video).unwrap();
        assert_eq!(wire["sourceType"], "hls");
        assert_eq!(wire["assets"]["transcript"], true);
        assert!(!wire.to_string().contains("fixture-signature"));
    }

    #[tokio::test]
    async fn invalid_links_never_reach_transport() {
        for link in [
            format!("http://loom.com/share/{ID}"),
            format!("https://loom.com.evil.invalid/share/{ID}"),
            format!("https://127.0.0.1/share/{ID}"),
            format!("https://user@loom.com/share/{ID}"),
            format!("https://loom.com:8080/share/{ID}"),
            format!("https://loom.com/else/../share/{ID}"),
            "https://loom.com/share/xyz".into(),
            format!("https://loom.com/share/{ID}/extra"),
            format!("https://loom.com/embed/{ID}/extra"),
            format!("https://loom.com/share/-{ID}"),
            format!("https://loom.com/share/bad slug-{ID}"),
            format!("https://loom.com/share/a/b-{ID}"),
            format!("https://loom.com/share/{ID}0"),
            format!("https://loom.com/v/{ID}"),
            "https://loom.com/share/€€€€€€€€€€€".into(),
            format!("https://loom.com/share/é-{ID}"),
            format!("https:\\\\evil.invalid\\share\\{ID}"),
        ] {
            let result =
                analyze_with(&link, |_| async { panic!("invalid URL reached transport") }).await;
            assert_eq!(result.err(), Some(AnalyzeError::InvalidUrl), "{link}");
        }
    }

    #[test]
    fn embed_slug_and_shell_escaped_links_resolve_to_the_share_page() {
        let canonical = format!("https://www.loom.com/share/{ID}");
        for link in [
            format!("https://www.loom.com/embed/{ID}"),
            format!("https://loom.com/embed/{ID}?hide_owner=true"),
            format!("https://www.loom.com/share/Weekly-sync_v2.1-{ID}"),
            format!("https://www.loom.com/share/Caf%C3%A9-notes-{ID}/"),
            format!("https://www.loom.com/share/{ID}\\?sid\\=abc"),
        ] {
            assert_eq!(
                normalize_share_url(&link).map(|u| u.to_string()),
                Ok(canonical.clone()),
                "{link}"
            );
        }
    }

    #[test]
    fn http_errors_and_special_networks_are_rejected() {
        for (code, expected) in [
            (302, AnalyzeError::Redirect),
            (401, AnalyzeError::AuthenticationRequired),
            (403, AnalyzeError::PermissionDenied),
            (404, AnalyzeError::Unavailable),
            (429, AnalyzeError::RateLimited),
            (503, AnalyzeError::Network),
        ] {
            assert_eq!(
                check_status(StatusCode::from_u16(code).unwrap()),
                Err(expected)
            );
        }
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!public_address(address.parse().unwrap()), "{address}");
        }
    }

    #[tokio::test]
    async fn media_errors_preserve_explicit_expiry_and_refuse_redirects() {
        for (status, location, body, expected) in [
            (401, "", "", AnalyzeError::AuthenticationRequired),
            (
                403,
                "",
                "<Error><Code>AccessDenied</Code></Error>",
                AnalyzeError::PermissionDenied,
            ),
            (
                403,
                "",
                "<Error><Code>ExpiredToken</Code></Error>",
                AnalyzeError::MediaAuthorizationExpired,
            ),
            (302, "https://127.0.0.1/file", "", AnalyzeError::Redirect),
            (307, "https://10.0.0.4/file", "", AnalyzeError::Redirect),
        ] {
            let response = http::Response::builder()
                .status(status)
                .header("Location", location)
                .body(reqwest::Body::from(body))
                .unwrap()
                .into();
            assert_eq!(
                validate_media_response(response).await.unwrap_err(),
                expected
            );
        }
    }
}
