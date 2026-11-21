//! Optional per-download diagnostics, the app's version of upstream's
//! `--verbose` (FFmpeg command and output) and `debug-page.html`.
//!
//! A [`Diagnostics`] collector is installed for one `save_recording` call as
//! a task-local, so the code that runs FFmpeg or retries can add lines
//! without threading a logger through every signature. Secrets are redacted
//! before they are stored: CloudFront cookies and signed query strings.

use std::{
    fmt,
    sync::{Arc, Mutex},
};
use url::Url;

const MAX_LINES: usize = 2000;
const MAX_LINE: usize = 64 * 1024;

#[derive(Default)]
struct Inner {
    lines: Vec<String>,
    page: Option<String>,
}

/// Collects one download's diagnostic lines and the share page it read.
#[derive(Clone, Default)]
pub struct Diagnostics(Arc<Mutex<Inner>>);

impl fmt::Debug for Diagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Diagnostics")
    }
}

impl Diagnostics {
    /// The redacted log lines, oldest first.
    pub fn lines(&self) -> Vec<String> {
        self.0.lock().map(|i| i.lines.clone()).unwrap_or_default()
    }

    /// The share page HTML, like upstream's `debug-page.html`. It can contain
    /// short-lived signed media links, so callers should keep it local.
    pub fn page(&self) -> Option<String> {
        self.0.lock().ok().and_then(|i| i.page.clone())
    }

    fn push(&self, mut line: String) {
        if let Ok(mut inner) = self.0.lock() {
            if inner.lines.len() >= MAX_LINES {
                return;
            }
            if line.len() > MAX_LINE {
                let mut cut = MAX_LINE;
                while !line.is_char_boundary(cut) {
                    cut -= 1;
                }
                line.truncate(cut);
                line.push_str(" …[truncated]");
            }
            inner.lines.push(line);
        }
    }
}

tokio::task_local! {
    static CURRENT: Diagnostics;
}

/// Runs `work` with `diagnostics` collecting lines, or plainly when `None`.
pub(crate) async fn scope<T>(
    diagnostics: Option<Diagnostics>,
    work: impl std::future::Future<Output = T>,
) -> T {
    match diagnostics {
        Some(d) => CURRENT.scope(d, work).await,
        None => work.await,
    }
}

/// Adds a line when diagnostics are on. The closure only runs in that case.
pub(crate) fn log(line: impl FnOnce() -> String) {
    let _ = CURRENT.try_with(|d| d.push(without_profile(&redact_text(&line()))));
}

/// For text already redacted piece by piece, such as [`command_line`];
/// a second whole-line pass would cut everything after `Cookie:`.
pub(crate) fn log_redacted(line: impl FnOnce() -> String) {
    let _ = CURRENT.try_with(|d| d.push(without_profile(&line())));
}

/// Keeps the share page like upstream's `debug-page.html`, minus the
/// CloudFront credentials and signed query strings it carries.
pub(crate) fn keep_page(html: &str) {
    let _ = CURRENT.try_with(|d| {
        if let Ok(mut inner) = d.0.lock() {
            inner.page = Some(redact_page(html));
        }
    });
}

/// Replaces the Windows profile folder (which names the account) in log text.
fn without_profile(text: &str) -> String {
    let profile = std::env::var("USERPROFILE").unwrap_or_default();
    replace_profile(text, &profile)
}

fn replace_profile(text: &str, profile: &str) -> String {
    if profile.len() < 4 {
        return text.to_owned();
    }
    text.replace(profile, "%USERPROFILE%")
}

/// The share page with the Apollo `credentials` values (which become the
/// CloudFront cookies) blanked and every URL query and fragment removed.
pub(crate) fn redact_page(html: &str) -> String {
    ["Policy", "Signature", "KeyPairId"]
        .iter()
        .fold(redact_urls(html), |text, key| blank_json_value(&text, key))
}

/// Replaces the string value of every `"key": "…"` pair with `<redacted>`.
fn blank_json_value(text: &str, key: &str) -> String {
    let needle = format!("\"{key}\"");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(&needle) {
        let after_key = at + needle.len();
        out.push_str(&rest[..after_key]);
        let tail = &rest[after_key..];
        let value = tail
            .trim_start()
            .strip_prefix(':')
            .map(str::trim_start)
            .and_then(|v| v.strip_prefix('"'));
        let closing = value.and_then(|v| {
            let mut escaped = false;
            v.char_indices().find_map(|(i, c)| match (escaped, c) {
                (true, _) => {
                    escaped = false;
                    None
                }
                (false, '\\') => {
                    escaped = true;
                    None
                }
                (false, '"') => Some(i),
                _ => None,
            })
        });
        match (value, closing) {
            (Some(v), Some(end)) => {
                out.push_str(&tail[..tail.len() - v.len()]);
                out.push_str("<redacted>");
                rest = &v[end..];
            }
            _ => rest = tail,
        }
    }
    out.push_str(rest);
    out
}

/// Removes signed query strings from URLs and values from Cookie headers.
pub(crate) fn redact_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let lower = line.to_ascii_lowercase();
        if let Some(at) = lower.find("cookie:") {
            out.push_str(&redact_urls(&line[..at]));
            out.push_str(&line[at..at + "cookie:".len()]);
            out.push_str(" <redacted>");
            if line.ends_with('\r') {
                out.push('\r');
            }
            continue;
        }
        out.push_str(&redact_urls(line));
    }
    out
}

/// The first `http://` or `https://`, in any letter case.
fn next_url(text: &str) -> Option<usize> {
    // ASCII lowercasing keeps byte offsets unchanged.
    let lower = text.to_ascii_lowercase();
    [lower.find("https://"), lower.find("http://")]
        .into_iter()
        .flatten()
        .min()
}

fn redact_urls(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = next_url(rest) {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>'))
            .unwrap_or(tail.len());
        let (url, after) = tail.split_at(end);
        match Url::parse(url) {
            Ok(mut parsed) if parsed.query().is_some() || parsed.fragment().is_some() => {
                parsed.set_query(None);
                parsed.set_fragment(None);
                out.push_str(parsed.as_str());
                out.push_str("?<redacted>");
            }
            Ok(_) => out.push_str(url),
            // Unparsable: still drop anything that could be a signature.
            Err(_) => match url.find(['?', '#']) {
                Some(cut) => {
                    out.push_str(&url[..cut]);
                    out.push_str("?<redacted>");
                }
                None => out.push_str(url),
            },
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// A command line for the log, with each argument redacted.
pub(crate) fn command_line(program: &str, args: &[String]) -> String {
    std::iter::once(program.to_owned())
        .chain(args.iter().map(|a| {
            let a = redact_text(a).replace("\r\n", "\\r\\n");
            if a.contains(' ') { format!("\"{a}\"") } else { a }
        }))
        .collect::<Vec<_>>()
        .join(" ")
}

/// For log lines that mention a URL without exposing its signature.
pub(crate) fn safe_url(url: &Url) -> String {
    redact_urls(url.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_and_signed_queries_never_reach_the_log() {
        let text = "Referer: https://www.loom.com/\r\nCookie: CloudFront-Signature=abc; CloudFront-Key-Pair-Id=K\r\n";
        let red = redact_text(text);
        assert!(!red.contains("abc") && !red.contains("Key-Pair-Id=K"), "{red}");
        assert!(red.contains("Referer: https://www.loom.com/"));
        let url = "open https://luna.loom.com/a/master.m3u8?Policy=p&Signature=s now";
        assert_eq!(redact_text(url), "open https://luna.loom.com/a/master.m3u8?<redacted> now");
        let args = ["-headers".to_string(), text.to_string(), "-i".into(), "https://cdn.loom.com/x.mp4?sig=1".into()];
        let line = command_line("ffmpeg", &args);
        assert!(!line.contains("abc") && !line.contains("sig=1"), "{line}");
    }

    #[test]
    fn url_shapes_that_used_to_slip_through_are_redacted() {
        for (text, secret) in [
            ("a http://x.loom.com/a?Signature=S1 b https://y.loom.com/b?Signature=ok", "S1"),
            ("HTTPS://cdn.loom.com/a?Signature=S2", "S2"),
            ("https://cdn.loom.com/b.mp4#Signature=S3", "S3"),
            ("https://[bad/a?Signature=S4", "S4"),
            ("see https://cdn.loom.com/a?Signature=S5 then Cookie: x", "S5"),
        ] {
            let red = redact_text(text);
            assert!(!red.contains(secret) && red.contains("<redacted>"), "{text} -> {red}");
        }
        assert_eq!(redact_text("plain https://www.loom.com/share/x"), "plain https://www.loom.com/share/x");
    }

    #[test]
    fn the_page_copy_loses_credentials_and_signed_queries() {
        let html = r#"<script>window.__APOLLO_STATE__ = {"v": {"url": "https://luna.loom.com/a/m.m3u8?Policy=P1\u0026Signature=S1",
            "credentials": {"Policy": "P2-secret", "Signature" : "S2\"x", "KeyPairId":"K2"}}, "name": "Keep me"};</script>"#;
        let red = redact_page(html);
        for secret in ["P1", "S1", "P2-secret", "S2", "K2"] {
            assert!(!red.contains(secret), "{secret} in {red}");
        }
        assert!(red.contains(r#""Policy": "<redacted>""#) && red.contains(r#""KeyPairId":"<redacted>""#));
        assert!(red.contains("Keep me") && red.contains("https://luna.loom.com/a/m.m3u8?<redacted>"));
    }

    #[test]
    fn profile_folders_are_replaced() {
        let line = r"Saved C:\Users\alex\Videos\rec.mp4";
        assert_eq!(replace_profile(line, r"C:\Users\alex"), r"Saved %USERPROFILE%\Videos\rec.mp4");
        assert_eq!(replace_profile(line, ""), line);
    }

    #[test]
    fn line_count_and_length_are_capped_without_splitting_characters() {
        let d = Diagnostics::default();
        for n in 0..MAX_LINES + 5 {
            d.push(format!("line {n}"));
        }
        assert_eq!(d.lines().len(), MAX_LINES);
        let d = Diagnostics::default();
        d.push(format!("{}é…", "a".repeat(MAX_LINE - 1)));
        let line = &d.lines()[0];
        assert!(line.ends_with("…[truncated]") && line.len() <= MAX_LINE + " …[truncated]".len());
    }

    #[tokio::test]
    async fn lines_are_collected_only_inside_a_scope() {
        log(|| "outside".into());
        let d = Diagnostics::default();
        scope(Some(d.clone()), async {
            log(|| "inside https://cdn.loom.com/a.mp4?sig=x".into());
            keep_page("<html></html>");
        })
        .await;
        assert_eq!(d.lines(), ["inside https://cdn.loom.com/a.mp4?<redacted>"]);
        assert_eq!(d.page().as_deref(), Some("<html></html>"));
        scope(Some(d.clone()), async { log_redacted(|| "ffmpeg -headers \"Cookie: <redacted>\" -i x".into()) }).await;
        assert!(d.lines()[1].ends_with("-i x"), "a pre-redacted line is kept whole");
        let calls = std::cell::Cell::new(0);
        scope(None, async { log(|| { calls.set(1); String::new() }) }).await;
        assert_eq!(calls.get(), 0);
    }
}
