use crate::{AnalyzeError, AssetHints, SourceType, VideoDescriptor};
use scraper::{Html, Selector};
use serde_json::Value;
use url::Url;

fn apollo_state(page: &Html) -> Result<Value, AnalyzeError> {
    let scripts = Selector::parse("script").expect("static CSS selector");
    for script in page.select(&scripts) {
        for text in script.text() {
            let Some((_, tail)) = text.split_once("window.__APOLLO_STATE__") else {
                continue;
            };
            let Some(tail) = tail.trim_start().strip_prefix('=') else {
                continue;
            };
            // Consume one JSON value even if other script statements follow.
            let mut stream =
                serde_json::Deserializer::from_str(tail.trim_start()).into_iter::<Value>();
            if let Some(Ok(state)) = stream.next()
                && state.is_object()
            {
                return Ok(state);
            }
        }
    }
    Err(AnalyzeError::PageFormat)
}

fn resolve<'a>(state: &'a Value, value: &'a Value) -> Option<&'a Value> {
    match value.get("__ref").and_then(Value::as_str) {
        Some(key) => state.get(key),
        None => Some(value),
    }
}

fn video_record(value: &Value) -> bool {
    value["__typename"]
        .as_str()
        .is_some_and(|name| name.ends_with("Video"))
}

fn matches_id(value: &Value, id: &str) -> bool {
    value["id"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case(id))
}

fn field_name(key: &str) -> &str {
    key.split('(').next().unwrap_or(key)
}

fn https_url(value: &Value) -> Option<Url> {
    let url = Url::parse(value.as_str()?).ok()?;
    (url.scheme() == "https"
        && url.host().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}

fn direct_file(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("cdn.loom.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && ["/sessions/transcoded/", "/sessions/raw/"]
            .iter()
            .any(|prefix| url.path().starts_with(prefix))
        && url.path().ends_with(".mp4")
}

fn names_recording(url: &Url, id: &str) -> bool {
    url.path()
        .split(|c: char| !c.is_ascii_hexdigit())
        .any(|part| part.eq_ignore_ascii_case(id))
}

fn scoped_mp4_in_html(html: &str, id: &str) -> Option<Url> {
    // Upstream falls back to the first direct CDN file in the page markup. This
    // port additionally requires the file path to carry the requested ID.
    html.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '<' | '>'))
        .filter(|token| token.starts_with("https://cdn.loom.com/sessions/"))
        .filter_map(|token| Url::parse(token).ok())
        .find(|url| direct_file(url) && names_recording(url, id))
}

/// Scrubber thumbnails: a sprite image and the WebVTT file that indexes it.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SeekPreview {
    pub image: Option<Url>,
    pub vtt: Option<Url>,
}

impl SeekPreview {
    fn add(&mut self, url: Url) {
        let path = url.path().to_ascii_lowercase();
        if !path.contains("seekpreview") {
            return;
        }
        if path.ends_with(".jpg") {
            self.image.get_or_insert(url);
        } else if path.ends_with(".vtt") {
            self.vtt.get_or_insert(url);
        }
    }

    fn complete(&self) -> bool {
        self.image.is_some() && self.vtt.is_some()
    }
}

/// Every string reachable from `value`, following Apollo `__ref`s a few levels deep.
fn strings<'a>(state: &'a Value, value: &'a Value, depth: u8, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(items) if depth > 0 => {
            items.iter().for_each(|v| strings(state, v, depth - 1, out));
        }
        Value::Object(map) if depth > 0 => match map.get("__ref").and_then(Value::as_str) {
            Some(key) => {
                if let Some(target) = state.get(key) {
                    strings(state, target, depth - 1, out);
                }
            }
            None => map.values().for_each(|v| strings(state, v, depth - 1, out)),
        },
        _ => {}
    }
}

/// Upstream's seek-preview search: Apollo data first, then the page markup,
/// where JSON escapes `&` as `\u0026`.
fn seek_preview(state: &Value, video: &Value, html: &str, one_recording: bool) -> SeekPreview {
    let mut found = SeekPreview::default();
    let add_all = |values: Vec<&str>, found: &mut SeekPreview| {
        values
            .into_iter()
            .filter_map(|s| https_url(&Value::from(s)))
            .for_each(|u| found.add(u));
    };
    let mut values = Vec::new();
    strings(state, video, 4, &mut values);
    add_all(values, &mut found);
    if one_recording && !found.complete() {
        let mut values = Vec::new();
        strings(state, state, 4, &mut values);
        add_all(values, &mut found);
    }
    if one_recording && !found.complete() {
        let unescaped = html.replace("\\u0026", "&");
        unescaped
            .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '<' | '>'))
            .filter(|t| t.starts_with("https://") && t.contains("seekpreview"))
            .filter_map(|t| Url::parse(t).ok())
            .for_each(|u| found.add(u));
    }
    found
}

/// The playback stream reported by the share page, plus the CloudFront cookies
/// Loom's player sends with the playlist and segment requests.
#[derive(Debug)]
pub(crate) struct HlsSource {
    pub url: Url,
    pub cookies: Option<String>,
}

#[derive(Debug)]
pub(crate) enum MediaSource {
    Hls(HlsSource),
    Mp4(Url),
}

fn cookie_value(value: &str) -> bool {
    // RFC 6265 cookie-octet. This also rules out header injection via CR/LF.
    !value.is_empty()
        && value
            .bytes()
            .all(|b| matches!(b, 0x21 | 0x23..=0x2B | 0x2D..=0x3A | 0x3C..=0x5B | 0x5D..=0x7E))
}

fn cloudfront_cookies(credentials: &Value) -> Result<Option<String>, AnalyzeError> {
    let mut pairs = Vec::new();
    for (cookie, field) in [
        ("CloudFront-Policy", "Policy"),
        ("CloudFront-Signature", "Signature"),
        ("CloudFront-Key-Pair-Id", "KeyPairId"),
    ] {
        match &credentials[field] {
            Value::Null => {}
            Value::String(value) if cookie_value(value) => {
                pairs.push(format!("{cookie}={value}"));
            }
            _ => return Err(AnalyzeError::InvalidMediaUrl),
        }
    }
    Ok((!pairs.is_empty()).then(|| pairs.join("; ")))
}

/// Upstream takes the first field whose key contains `M3U8` and has a `url`.
fn m3u8_field<'a>(state: &'a Value, key: &str, value: &'a Value) -> Option<&'a Value> {
    let value = resolve(state, value)?;
    let url = value["url"].as_str()?;
    (key.contains("M3U8")
        || matches!(field_name(key), "nullableRawCdnUrl" | "rawCdnUrl")
            && Url::parse(url).is_ok_and(|u| u.path().ends_with(".m3u8")))
    .then_some(value)
}

fn hls_source(value: &Value) -> Result<HlsSource, AnalyzeError> {
    let url = https_url(&value["url"]).ok_or(AnalyzeError::InvalidMediaUrl)?;
    crate::transport::checked_media_host(&url)?;
    Ok(HlsSource {
        url,
        cookies: cloudfront_cookies(&value["credentials"])?,
    })
}

pub fn parse_share_page(share: &Url, html: &str) -> Result<VideoDescriptor, AnalyzeError> {
    let page = inspect_page(share, html)?;
    page.media?;
    Ok(page.video)
}

/// Everything a save needs from one share page. Media is kept separately so a
/// transcript-only save still works when no playable source is reported.
#[derive(Debug)]
pub(crate) struct PageInfo {
    pub video: VideoDescriptor,
    pub media: Result<MediaSource, AnalyzeError>,
    pub transcript: Option<Url>,
    pub captions: Option<Url>,
    pub seek_preview: SeekPreview,
}

pub(crate) fn inspect_page(share: &Url, html: &str) -> Result<PageInfo, AnalyzeError> {
    let canonical = crate::normalize_share_url(share.as_str())?;
    let id = canonical
        .path()
        .strip_prefix("/share/")
        .ok_or(AnalyzeError::InvalidUrl)?;
    let page = Html::parse_document(html);
    let state = apollo_state(&page)?;
    let records = state.as_object().ok_or(AnalyzeError::PageFormat)?;
    let root = state["ROOT_QUERY"].as_object();
    let video = records
        .iter()
        .find_map(|(key, value)| {
            let key_matches = key
                .rsplit_once(':')
                .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case(id));
            (video_record(value)
                && (matches_id(value, id) || (value.get("id").is_none() && key_matches)))
                .then_some(value)
        })
        .or_else(|| {
            root?
                .iter()
                .filter(|(key, _)| field_name(key) == "getVideo")
                .filter_map(|(_, value)| resolve(&state, value))
                .find(|value| video_record(value) && matches_id(value, id))
        })
        .ok_or(AnalyzeError::PageFormat)?;

    let mut hls = None;
    let mut mp4 = None;
    let mut assets = AssetHints::default();
    let mut transcript = None;
    let mut captions = None;
    let fields = video.as_object().ok_or(AnalyzeError::PageFormat)?;
    for (key, value) in fields {
        if hls.is_none()
            && let Some(stream) = m3u8_field(&state, key, value)
        {
            hls = Some(hls_source(stream));
        }
        let Some(value) = resolve(&state, value) else {
            continue;
        };
        match field_name(key) {
            "transcodedUrl" | "getVideoTranscodedUrl" | "mp4Url" if mp4.is_none() => {
                mp4 = https_url(value)
                    .or_else(|| https_url(&value["url"]))
                    .filter(direct_file);
            }
            "signedDefaultThumbnails" => {
                assets.thumbnail |= ["static", "default"]
                    .iter()
                    .any(|key| https_url(&value[*key]).is_some())
            }
            _ => {}
        }
        if value["__typename"] == "VideoTranscriptDetails" {
            transcript = transcript.or_else(|| https_url(&value["source_url"]));
            captions = captions.or_else(|| https_url(&value["captions_source_url"]));
        }
    }

    // Some flat caches contain one detached transcript record. With multiple
    // recordings or transcripts the relationship must be explicit.
    let mut standalone = records
        .values()
        .filter(|v| v["__typename"] == "VideoTranscriptDetails");
    let is_selected = |v: &Value| std::ptr::eq(v, video) || matches_id(v, id);
    let one_recording = records
        .values()
        .filter(|v| video_record(v))
        .all(is_selected)
        && root
            .into_iter()
            .flatten()
            .filter(|(k, _)| field_name(k) == "getVideo")
            .all(|(_, v)| resolve(&state, v).is_some_and(is_selected));
    if one_recording {
        if let (Some(v), None) = (standalone.next(), standalone.next()) {
            transcript = transcript.or_else(|| https_url(&v["source_url"]));
            captions = captions.or_else(|| https_url(&v["captions_source_url"]));
        }
        if mp4.is_none() {
            let sources =
                Selector::parse("video[src], video source[src]").expect("static CSS selector");
            mp4 = page
                .select(&sources)
                .filter_map(|elem| Url::parse(elem.value().attr("src")?).ok())
                .find(direct_file);
        }
    }

    let seek_preview = seek_preview(&state, video, html, one_recording);

    // Upstream order: the HLS stream the player uses, then a direct MP4 only
    // when the page reports no usable HLS URL.
    let media = match (hls, mp4.or_else(|| scoped_mp4_in_html(html, id))) {
        (Some(Ok(stream)), _) => Ok(MediaSource::Hls(stream)),
        (_, Some(file)) => Ok(MediaSource::Mp4(file)),
        (Some(Err(error)), None) => Err(error),
        (None, None) => Err(AnalyzeError::NoMedia),
    };
    assets.transcript = transcript.is_some();
    assets.captions = captions.is_some();
    let title = video["name"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    Ok(PageInfo {
        video: VideoDescriptor {
            id: id.into(),
            title_file_name: title.as_deref().and_then(crate::options::sanitize_filename),
            title,
            duration_seconds: video["playable_duration"]
                .as_f64()
                .filter(|d| d.is_finite() && *d >= 0.0),
            source_url: canonical.to_string(),
            source_type: match media {
                Ok(MediaSource::Mp4(_)) => SourceType::Mp4,
                _ => SourceType::Hls,
            },
            assets,
        },
        media,
        transcript,
        captions,
        seek_preview,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captions_and_transcript_urls_come_from_their_own_fields() {
        let share = Url::parse("https://www.loom.com/share/00112233445566778899aabbccddeeff").unwrap();
        let info = inspect_page(&share, include_str!("../tests/fixtures/hls.html")).unwrap();
        assert_eq!(
            info.captions.unwrap().as_str(),
            "https://cdn.loom.com/example/captions.vtt"
        );
        assert_eq!(
            info.transcript.unwrap().as_str(),
            "https://cdn.loom.com/example/transcript.json"
        );
    }

    #[test]
    fn seek_preview_comes_from_the_recording_then_the_page() {
        let state = serde_json::json!({
            "RegularUserVideo:a": {"id":"a","__typename":"RegularUserVideo","media":{"__ref":"Media:1"}},
            "Media:1": {"seek": "https://cdn.loom.com/mediametadata/seekpreview/a/sprite.jpg?Policy=x"}
        });
        let video = &state["RegularUserVideo:a"];
        let html = r#"<script>x="https://cdn.loom.com/mediametadata/seekpreview/a/index.vtt?Policy=p\u0026Signature=s"</script>"#;
        let found = seek_preview(&state, video, html, true);
        assert_eq!(found.image.unwrap().path(), "/mediametadata/seekpreview/a/sprite.jpg");
        let vtt = found.vtt.unwrap();
        assert_eq!(vtt.query(), Some("Policy=p&Signature=s"));
        // With several recordings on the page, only the selected record counts.
        let scoped = seek_preview(&state, video, html, false);
        assert!(scoped.image.is_some() && scoped.vtt.is_none());
        // Plain http and unrelated files are ignored.
        let other = r#""http://cdn.loom.com/mediametadata/seekpreview/a/x.vtt" "https://cdn.loom.com/a.vtt""#;
        assert_eq!(seek_preview(&Value::Null, &Value::Null, other, true), SeekPreview::default());
    }

    #[test]
    fn credentials_become_cloudfront_cookies_and_reject_injection() {
        let creds = serde_json::json!({"Policy":"eyJ-p_","Signature":"s~ig","KeyPairId":"K2"});
        assert_eq!(
            cloudfront_cookies(&creds).unwrap().as_deref(),
            Some("CloudFront-Policy=eyJ-p_; CloudFront-Signature=s~ig; CloudFront-Key-Pair-Id=K2")
        );
        assert_eq!(cloudfront_cookies(&Value::Null).unwrap(), None);
        for bad in ["a\r\nX-Evil: 1", "a;b", "a b", ""] {
            let creds = serde_json::json!({ "Signature": bad });
            assert_eq!(
                cloudfront_cookies(&creds),
                Err(AnalyzeError::InvalidMediaUrl),
                "{bad:?}"
            );
        }
    }
}
