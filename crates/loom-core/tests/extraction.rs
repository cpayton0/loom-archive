use loom_core::{AnalyzeError, SourceType, normalize_share_url, parse_share_page};

const URL: &str = "https://www.loom.com/share/00112233445566778899aabbccddeeff";
const DIRECT: &str = include_str!("fixtures/direct.html");
const HLS: &str = include_str!("fixtures/hls.html");

#[test]
fn hls_playback_stream_is_used_regardless_of_download_flags() {
    let share = normalize_share_url(URL).unwrap();
    let disabled = HLS.replace(
        "\"name\": \"Orientation\"",
        "\"download_enabled\":false,\"downloadable\":false,\"name\":\"Orientation\"",
    );
    for page in [HLS, disabled.as_str()] {
        assert_eq!(
            parse_share_page(&share, page).unwrap().source_type,
            SourceType::Hls
        );
    }
}

#[test]
fn hls_is_preferred_over_a_direct_mp4_like_upstream() {
    let share = normalize_share_url(URL).unwrap();
    let mp4 = "<a href=\"https://cdn.loom.com/sessions/transcoded/00112233445566778899aabbccddeeff.mp4\">x</a>";
    assert_eq!(
        parse_share_page(&share, &format!("{HLS}{mp4}"))
            .unwrap()
            .source_type,
        SourceType::Hls
    );
}

#[test]
fn malformed_credentials_are_rejected() {
    let share = normalize_share_url(URL).unwrap();
    let html = HLS.replace("fixture-signature", r"bad\r\nX-Injected: 1");
    assert_eq!(
        parse_share_page(&share, &html).unwrap_err(),
        AnalyzeError::InvalidMediaUrl
    );
}

#[test]
fn direct_fixture_keeps_optional_fields_unknown() {
    let video = parse_share_page(&normalize_share_url(URL).unwrap(), DIRECT).unwrap();
    assert_eq!(video.title.as_deref(), Some("Design review"));
    assert_eq!(video.source_type, SourceType::Hls);
    assert_eq!(video.duration_seconds, None);
    assert!(!video.assets.transcript && !video.assets.captions && !video.assets.thumbnail);
}

#[test]
fn loopback_player_addresses_are_never_valid_media_sources() {
    let share = normalize_share_url(URL).unwrap();
    for host in [
        "localhost",
        "127.0.0.2",
        "[::1]",
        "10.1.2.3",
        "169.254.169.254",
    ] {
        let html = HLS.replace(
            "https://cdn.loom.com/sessions/placeholder",
            &format!("https://{host}/placeholder"),
        );
        assert_eq!(
            parse_share_page(&share, &html).unwrap_err(),
            AnalyzeError::InvalidMediaUrl
        );
    }
}

#[test]
fn reads_flat_hls_and_detached_optional_assets() {
    let video = parse_share_page(&normalize_share_url(URL).unwrap(), HLS).unwrap();
    assert_eq!(video.duration_seconds, Some(83.5));
    assert_eq!(video.source_type, SourceType::Hls);
    assert!(video.assets.transcript && video.assets.captions && video.assets.thumbnail);
}

#[test]
fn unrelated_mp4_does_not_mask_the_recordings_hls_source() {
    let html = DIRECT.replace(
        "00112233445566778899aabbccddeeff.mp4",
        "11111111111111111111111111111111.mp4",
    );
    let video = parse_share_page(&normalize_share_url(URL).unwrap(), &html).unwrap();
    assert_eq!(video.source_type, SourceType::Hls);
}

#[test]
fn distinguishes_missing_page_data_from_unknown_media() {
    let url = normalize_share_url(URL).unwrap();
    for html in [
        "<html></html>",
        "<script>window.__APOLLO_STATE__ = nope;</script>",
        "<script>window.__APOLLO_STATE__ = {};</script>",
    ] {
        assert_eq!(
            parse_share_page(&url, html).unwrap_err(),
            AnalyzeError::PageFormat
        );
    }
    let html = HLS
        .replace("nullableRawCdnUrl", "unknown")
        .replace("M3U8", "unknown");
    assert_eq!(
        parse_share_page(&url, &html).unwrap_err(),
        AnalyzeError::NoMedia
    );
}

#[test]
fn falls_back_to_the_recordings_direct_mp4_only_without_hls() {
    let share = normalize_share_url(URL).unwrap();
    let no_hls = DIRECT.replace("index.m3u8", "index.json");
    assert_eq!(
        parse_share_page(&share, &no_hls).unwrap().source_type,
        SourceType::Mp4
    );
    let unrelated = no_hls.replace(
        "00112233445566778899aabbccddeeff.mp4",
        "11111111111111111111111111111111.mp4",
    );
    assert_eq!(
        parse_share_page(&share, &unrelated).unwrap_err(),
        AnalyzeError::NoMedia
    );
}
