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
    let _ = CURRENT.try_with(|d| d.push(redact_text(&line())));
}

pub(crate) fn keep_page(html: &str) {
    let _ = CURRENT.try_with(|d| {
        if let Ok(mut inner) = d.0.lock() {
            inner.page = Some(html.to_owned());
        }
    });
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
            out.push_str(&line[..at + "cookie:".len()]);
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

fn redact_urls(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find("https://").or_else(|| rest.find("http://")) {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>'))
            .unwrap_or(tail.len());
        let (url, after) = tail.split_at(end);
        match Url::parse(url) {
            Ok(mut parsed) if parsed.query().is_some() => {
                parsed.set_query(None);
                out.push_str(parsed.as_str());
                out.push_str("?<redacted>");
            }
            _ => out.push_str(url),
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
        let calls = std::cell::Cell::new(0);
        scope(None, async { log(|| { calls.set(1); String::new() }) }).await;
        assert_eq!(calls.get(), 0);
    }
}
