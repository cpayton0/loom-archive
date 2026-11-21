use crate::{
    AnalyzeError, CancellationToken, Collision, Content, FileNaming, SaveOptions,
    hls::find_ffmpeg, normalize_share_url,
    options::sanitize_filename,
    page::MediaSource,
    transport::{fetch_asset, fetch_media_from, read_limited, resolve_recording, seek_preview_vtt},
};
use mp4::{Mp4Reader, TrackType};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fs::File,
    future::Future,
    io::{self, BufReader, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use url::Url;

#[derive(Clone, Copy, Serialize)]
pub struct TransferProgress {
    pub bytes: u64,
    pub total: Option<u64>,
    /// 0–100 when the length is known in time rather than bytes (FFmpeg jobs).
    pub percent: Option<f64>,
    pub finalizing: bool,
}

#[derive(Debug, Error)]
pub enum SaveError {
    #[error(transparent)]
    Page(#[from] AnalyzeError),
    #[error("“{0}” already exists in this folder. Choose another folder or file-name setting.")]
    Exists(String),
    #[error("Loom did not list a transcript for this recording.")]
    NoTranscript,
    #[error("Loom did not list captions for this recording.")]
    NoCaptions,
    #[error("Loom did not provide a seek preview for this recording.")]
    NoSeekPreview,
    #[error("Enter a file name that contains letters or numbers and is allowed on Windows.")]
    InvalidFileName,
    #[error("The transfer was interrupted. Check your connection and try again.")]
    Interrupted,
    #[error("The file was incomplete or did not contain the expected audio or video track.")]
    InvalidMedia,
    #[error("Install FFmpeg on PATH or place it beside Loom Archive to save HLS recordings.")]
    MissingFfmpeg,
    #[error("FFmpeg could not combine the recording’s tracks into an MP4.")]
    Processing,
    #[error("The selected folder could not be written. Check free space and folder permissions.")]
    Storage,
    #[error("Save canceled.")]
    Canceled,
}

/// The track a finished media file must contain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Expect {
    Video,
    Audio,
}

#[derive(Debug, Default, Serialize)]
pub struct SavedFiles {
    /// The recording ID, for callers that keep a download history.
    pub id: String,
    pub files: Vec<PathBuf>,
    pub notes: Vec<String>,
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

async fn cancelable<T>(
    cancel: &CancellationToken,
    work: impl Future<Output = Result<T, SaveError>>,
) -> Result<T, SaveError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(SaveError::Canceled),
        result = work => result,
    }
}

/// Delay before retry `attempt + 1`: 1 s doubling to a 32 s cap, as upstream.
fn retry_delay(attempt: u8) -> Duration {
    Duration::from_secs(1 << attempt.min(5))
}

/// Runs `attempt` until it succeeds, fails for a reason other than a dropped
/// connection, or `retries` extra attempts are used. Waits per
/// [`retry_delay`] in between and stops at once when canceled. Returns the
/// result and the number of retries made.
async fn with_retries<T, F, Fut>(
    retries: u8,
    cancel: &CancellationToken,
    mut attempt: F,
) -> (Result<T, SaveError>, u8)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, SaveError>>,
{
    let mut made = 0;
    loop {
        match attempt().await {
            Err(SaveError::Interrupted | SaveError::Page(AnalyzeError::Network))
                if made < retries =>
            {
                let wait = retry_delay(made);
                made += 1;
                let slept = cancelable(cancel, async {
                    tokio::time::sleep(wait).await;
                    Ok(())
                })
                .await;
                if let Err(error) = slept {
                    return (Err(error), made);
                }
            }
            result => return (result, made),
        }
    }
}

/// What happens when an extra file (transcript, captions, thumbnail…) fails:
/// `None` means it was saved, `Some(note)` explains a skip. Only a transcript
/// failure in transcript-only mode fails the whole download.
fn extra_outcome(
    label: &str,
    transcript_only: bool,
    result: Result<(), SaveError>,
) -> Result<Option<String>, SaveError> {
    match result {
        Ok(()) => Ok(None),
        Err(error) if transcript_only && label == "transcript" => Err(error),
        Err(SaveError::Canceled) => Ok(Some(format!("Canceled before the {label} was saved."))),
        Err(error) => Ok(Some(format!("The {label} was not saved: {error}"))),
    }
}

/// One download: the effective options plus per-download and app overrides.
#[derive(Clone, Debug, Default)]
pub struct SaveRequest {
    pub options: SaveOptions,
    /// A one-off file name. Blank means the naming setting decides.
    pub file_name: Option<String>,
    /// A configured FFmpeg executable; otherwise it is looked up.
    pub ffmpeg: Option<PathBuf>,
    /// Extra attempts after an interrupted media transfer.
    pub retries: u8,
    /// Batch numbering, upstream's `--prefix`: files become `<prefix>-<n>-<name>`.
    pub prefix: Option<(String, usize)>,
    /// Keep an interrupted direct-MP4 download and continue it with an HTTP
    /// range request (upstream `--resume`). HLS downloads always restart.
    pub resume: bool,
}

fn custom_base(name: &str) -> Result<String, SaveError> {
    let name = name.trim();
    let lower = name.to_ascii_lowercase();
    let stem = [".mp4", ".m4a", ".transcript.json", ".json"]
        .iter()
        .find(|ext| lower.ends_with(*ext))
        .map_or(name, |ext| &name[..name.len() - ext.len()]);
    sanitize_filename(stem).ok_or(SaveError::InvalidFileName)
}

/// Longest base name, leaving room for the longest suffix (`-seek-preview.jpg`,
/// `.transcript.json`) and a ` (999)` collision number within NTFS's 255.
const MAX_BASE: usize = 200;
const MAX_PREFIX: usize = 60;

/// The name every file of a download starts with: the custom name, or the
/// title or ID per the naming setting, then the batch `<prefix>-<n>-` on top.
fn file_base(
    video: &crate::VideoDescriptor,
    custom: Option<String>,
    request: &SaveRequest,
) -> Result<String, SaveError> {
    let base = custom.unwrap_or_else(|| match request.options.naming {
        FileNaming::Title => video.title_file_name.clone().unwrap_or_else(|| video.id.clone()),
        FileNaming::Id => video.id.clone(),
    });
    let base = match &request.prefix {
        Some((prefix, n)) => {
            let prefix: String = prefix.chars().take(MAX_PREFIX).collect();
            let prefix = sanitize_filename(&prefix).ok_or(SaveError::InvalidFileName)?;
            format!("{prefix}-{n}-{base}")
        }
        None => base,
    };
    let capped: String = base.chars().take(MAX_BASE).collect();
    Ok(capped.trim_end_matches(['.', ' ']).to_string())
}

struct Plan {
    media: Option<PathBuf>,
    transcript: Option<PathBuf>,
    thumbnail: Option<PathBuf>,
    captions: Option<PathBuf>,
    gif: Option<PathBuf>,
    seek_image: Option<PathBuf>,
    seek_vtt: Option<PathBuf>,
}

impl Plan {
    fn new(folder: &Path, base: &str, options: &SaveOptions) -> Self {
        let transcript_only = options.content == Content::TranscriptOnly;
        Self {
            media: match options.content {
                Content::Video => Some(folder.join(format!("{base}.mp4"))),
                Content::AudioOnly => Some(folder.join(format!("{base}.m4a"))),
                Content::TranscriptOnly => None,
            },
            transcript: (options.transcript || transcript_only)
                .then(|| folder.join(format!("{base}.transcript.json"))),
            thumbnail: options
                .thumbnail
                .then(|| folder.join(format!("{base}-thumbnail.jpg"))),
            captions: options
                .captions
                .then(|| folder.join(format!("{base}.captions.vtt"))),
            gif: options
                .gif
                .then(|| folder.join(format!("{base}-thumbnail.gif"))),
            seek_image: options
                .seek_preview
                .then(|| folder.join(format!("{base}-seek-preview.jpg"))),
            seek_vtt: options
                .seek_preview
                .then(|| folder.join(format!("{base}-seek-preview.vtt"))),
        }
    }

    fn taken(&self) -> Result<Option<String>, SaveError> {
        for path in [
            &self.media,
            &self.transcript,
            &self.thumbnail,
            &self.captions,
            &self.gif,
            &self.seek_image,
            &self.seek_vtt,
        ]
        .into_iter()
        .flatten()
        {
            if path.try_exists().map_err(|_| SaveError::Storage)? {
                return Ok(Some(display_name(path)));
            }
        }
        Ok(None)
    }
}

async fn save_media(
    request: &SaveRequest,
    id: &str,
    duration: Option<f64>,
    media: &MediaSource,
    path: &Path,
    cancel: &CancellationToken,
    progress: &impl Fn(TransferProgress),
) -> Result<Option<String>, SaveError> {
    let options = &request.options;
    let audio_only = options.content == Content::AudioOnly;
    let exe = || find_ffmpeg(request.ffmpeg.as_deref()).ok_or(SaveError::MissingFfmpeg);
    match media {
        MediaSource::Hls(stream) => {
            crate::hls::save(
                &exe()?,
                stream,
                options.quality,
                audio_only,
                duration,
                path,
                cancel,
                progress,
            )
            .await?;
        }
        MediaSource::Mp4(url) if audio_only => {
            let job = crate::hls::Job::single(crate::hls::stream_options(None), url.as_str())
                .maps(&["-map", "0:a:0", "-vn"])
                .duration(duration);
            crate::hls::save_with(&exe()?, &job, Expect::Audio, path, cancel, progress).await?;
        }
        MediaSource::Mp4(url) => {
            save_direct(url, path, id, request.resume, cancel, progress).await?;
            if options.quality.max_height().is_some() {
                return Ok(Some(
                    "Quality applies to HLS streams. This page offered a single MP4, which was saved as is."
                        .into(),
                ));
            }
        }
    }
    Ok(None)
}

/// Upstream flow: read the share page, hand its HLS stream to FFmpeg (or fetch
/// the direct MP4 when there is no HLS stream), then save the requested extras
/// next to it using upstream's file names.
pub async fn save_recording(
    input: &str,
    folder: &Path,
    request: &SaveRequest,
    cancel: &CancellationToken,
    progress: impl Fn(TransferProgress),
) -> Result<SavedFiles, SaveError> {
    let options = &request.options;
    let custom = match request.file_name.as_deref() {
        Some(name) if !name.trim().is_empty() => Some(custom_base(name)?),
        _ => None,
    };
    let canonical = normalize_share_url(input)?;
    let page = cancelable(cancel, async {
        Ok(resolve_recording(canonical.as_str()).await?)
    })
    .await?;
    let base = file_base(&page.video, custom, request)?;
    // Captions the page does not list are dropped up front, so a leftover
    // `.captions.vtt` cannot cause a collision for a file that won't be written.
    let mut early_notes = Vec::new();
    let mut adjusted;
    let options = if options.captions && page.captions.is_none() {
        early_notes.push("Loom did not list captions for this recording.".to_string());
        adjusted = options.clone();
        adjusted.captions = false;
        &adjusted
    } else {
        options
    };
    let mut plan = Plan::new(folder, &base, options);
    if let Some(name) = plan.taken()? {
        if options.collision == Collision::Stop {
            return Err(SaveError::Exists(name));
        }
        plan = (2..1000)
            .map(|n| Plan::new(folder, &format!("{base} ({n})"), options))
            .find(|candidate| matches!(candidate.taken(), Ok(None)))
            .ok_or(SaveError::Exists(name))?;
    }
    let transcript_only = options.content == Content::TranscriptOnly;
    if transcript_only && page.transcript.is_none() {
        return Err(SaveError::NoTranscript);
    }

    let mut saved = SavedFiles {
        notes: early_notes,
        id: page.video.id.clone(),
        ..SavedFiles::default()
    };
    if let Some(path) = plan.media.take() {
        let media = page.media?;
        let duration = page.video.duration_seconds;
        let (result, attempt) = with_retries(request.retries, cancel, || {
            save_media(request, &page.video.id, duration, &media, &path, cancel, &progress)
        })
        .await;
        let note = result?;
        if attempt > 0 {
            saved.notes.push(format!(
                "The transfer was interrupted and retried {attempt} time{}.",
                if attempt == 1 { "" } else { "s" }
            ));
        }
        saved.notes.extend(note);
        saved.files.push(path);
    }
    let transcript_path = plan.transcript.take();
    let thumbnail_path = plan.thumbnail.take();
    let captions_path = plan.captions.take();

    let mut record = |label: &str, path: PathBuf, result: Result<(), SaveError>| -> Result<(), SaveError> {
        match extra_outcome(label, transcript_only, result)? {
            None => saved.files.push(path),
            Some(note) => saved.notes.push(note),
        }
        Ok(())
    };
    if let Some(path) = transcript_path {
        match page.transcript {
            Some(url) => {
                let result = cancelable(cancel, save_transcript(url, path.clone())).await;
                record("transcript", path, result)?;
            }
            None => record("transcript", path, Err(SaveError::NoTranscript))?,
        }
    }
    if let Some(path) = captions_path {
        match page.captions {
            Some(url) => {
                let result = cancelable(cancel, save_captions(url, path.clone())).await;
                record("captions file", path, result)?;
            }
            None => record("captions file", path, Err(SaveError::NoCaptions))?,
        }
    }
    if let Some(path) = thumbnail_path {
        let result = cancelable(cancel, save_thumbnail(page.video.id.clone(), path.clone())).await;
        record("thumbnail", path, result)?;
    }
    if let Some(path) = plan.gif {
        let result = cancelable(cancel, save_gif(page.video.id.clone(), path.clone())).await;
        record("GIF thumbnail", path, result)?;
    }
    if let (Some(image_path), Some(vtt_path)) = (plan.seek_image, plan.seek_vtt) {
        let urls = cancelable(cancel, async {
            seek_preview_urls(&page.video.id, page.seek_preview).await
        })
        .await;
        match urls {
            Ok((image, vtt)) => {
                let result = match image {
                    Some(url) => cancelable(cancel, save_jpeg(url, image_path.clone())).await,
                    None => Err(SaveError::NoSeekPreview),
                };
                record("seek preview image", image_path, result)?;
                let result = match vtt {
                    Some(url) => cancelable(cancel, save_webvtt(url, vtt_path.clone())).await,
                    None => Err(SaveError::NoSeekPreview),
                };
                record("seek preview index", vtt_path, result)?;
            }
            Err(error) => record("seek preview", image_path, Err(error))?,
        }
    }
    Ok(saved)
}

/// Upstream `downloadTranscript`: adds `plainText` when the JSON has sentences.
fn format_transcript(data: Value) -> Value {
    if data.get("sentences").is_none() && data.get("words").is_none() {
        return data;
    }
    let sentences = data["sentences"].as_array().cloned().unwrap_or_default();
    let plain = sentences
        .iter()
        .map(|s| s["text"].as_str().or(s["value"].as_str()).unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ");
    json!({
        "plainText": plain,
        "sentences": sentences,
        "words": data["words"].as_array().cloned().unwrap_or_default(),
        "raw": data,
    })
}

async fn save_transcript(url: Url, path: PathBuf) -> Result<(), SaveError> {
    let body = read_limited(fetch_asset(url).await?, 32 * 1024 * 1024).await?;
    let data: Value = serde_json::from_slice(&body).map_err(|_| AnalyzeError::PageFormat)?;
    let text = serde_json::to_vec_pretty(&format_transcript(data)).map_err(|_| SaveError::Storage)?;
    write_new(path, text).await
}

/// WebVTT captions from the page's `captions_source_url`.
async fn save_captions(url: Url, path: PathBuf) -> Result<(), SaveError> {
    save_webvtt(url, path).await
}

async fn save_webvtt(url: Url, path: PathBuf) -> Result<(), SaveError> {
    let body = read_limited(fetch_asset(url).await?, 8 * 1024 * 1024).await?;
    if !is_webvtt(&body) {
        return Err(SaveError::InvalidMedia);
    }
    write_new(path, body).await
}

fn is_jpeg(body: &[u8]) -> bool {
    body.starts_with(&[0xFF, 0xD8, 0xFF])
}

fn is_gif(body: &[u8]) -> bool {
    body.starts_with(b"GIF87a") || body.starts_with(b"GIF89a")
}

/// Upstream's `--gif` thumbnail path.
fn gif_url(id: &str) -> Result<Url, SaveError> {
    Url::parse(&format!("https://cdn.loom.com/sessions/thumbnails/{id}-00001.gif"))
        .map_err(|_| AnalyzeError::InvalidMediaUrl.into())
}

async fn save_jpeg(url: Url, path: PathBuf) -> Result<(), SaveError> {
    let body = read_limited(fetch_asset(url).await?, 50 * 1024 * 1024).await?;
    if !is_jpeg(&body) {
        return Err(SaveError::InvalidMedia);
    }
    write_new(path, body).await
}

async fn save_gif(id: String, path: PathBuf) -> Result<(), SaveError> {
    let body = read_limited(fetch_asset(gif_url(&id)?).await?, 50 * 1024 * 1024).await?;
    if !is_gif(&body) {
        return Err(SaveError::InvalidMedia);
    }
    write_new(path, body).await
}

/// Page data first; otherwise upstream's GraphQL fallback, whose sprite sits
/// beside the index with a `.jpg` extension.
async fn seek_preview_urls(
    id: &str,
    found: crate::page::SeekPreview,
) -> Result<(Option<Url>, Option<Url>), SaveError> {
    let fallback = match found.vtt {
        Some(_) => Ok(None),
        None => seek_preview_vtt(id).await.map_err(SaveError::from),
    };
    pick_seek_urls(found, fallback)
}

/// Combines page data with the GraphQL fallback result. A failed fallback
/// only matters when the page provided nothing at all.
fn pick_seek_urls(
    found: crate::page::SeekPreview,
    fallback: Result<Option<Url>, SaveError>,
) -> Result<(Option<Url>, Option<Url>), SaveError> {
    let vtt = match (found.vtt, fallback) {
        (Some(vtt), _) => Some(vtt),
        (None, Ok(vtt)) => vtt,
        (None, Err(error)) if found.image.is_none() => return Err(error),
        (None, Err(_)) => None,
    };
    let image = found.image.or_else(|| vtt.as_ref().and_then(sprite_beside));
    Ok((image, vtt))
}

fn sprite_beside(vtt: &Url) -> Option<Url> {
    let mut image = vtt.clone();
    let path = vtt.path().strip_suffix(".vtt")?;
    image.set_path(&format!("{path}.jpg"));
    Some(image)
}

fn is_webvtt(body: &[u8]) -> bool {
    let body = body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(body);
    body.starts_with(b"WEBVTT")
        && body
            .get(6)
            .is_none_or(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
}

/// Upstream's thumbnail path for `--image`.
async fn save_thumbnail(id: String, path: PathBuf) -> Result<(), SaveError> {
    let url = Url::parse(&format!(
        "https://cdn.loom.com/sessions/thumbnails/{id}-00001.jpg"
    ))
    .map_err(|_| AnalyzeError::InvalidMediaUrl)?;
    let body = read_limited(fetch_asset(url).await?, 20 * 1024 * 1024).await?;
    if !body.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Err(SaveError::InvalidMedia);
    }
    write_new(path, body).await
}

async fn write_new(path: PathBuf, bytes: Vec<u8>) -> Result<(), SaveError> {
    tokio::task::spawn_blocking(move || {
        let dir = path.parent().ok_or(SaveError::Storage)?;
        let mut temp = tempfile::Builder::new()
            .prefix(".loom-")
            .suffix(".part")
            .tempfile_in(dir)
            .map_err(|_| SaveError::Storage)?;
        temp.write_all(&bytes).map_err(|_| SaveError::Storage)?;
        temp.as_file().sync_all().map_err(|_| SaveError::Storage)?;
        persist(temp.into_temp_path(), &path)
    })
    .await
    .map_err(|_| SaveError::Storage)?
}

fn persist(temp: tempfile::TempPath, dest: &Path) -> Result<(), SaveError> {
    temp.persist_noclobber(dest)
        .map_err(|e| match e.error.kind() {
            io::ErrorKind::AlreadyExists => SaveError::Exists(display_name(dest)),
            _ => SaveError::Storage,
        })
}

/// A direct download's on-disk bytes: where they go, where this response
/// starts, and whether to keep them when the connection drops.
struct Partial {
    path: PathBuf,
    start: u64,
    keep: bool,
}

/// The hidden file an interrupted direct download is kept in. It is named
/// after the recording, so a later attempt finds it under any file name.
fn partial_path(dest: &Path, id: &str) -> PathBuf {
    dest.with_file_name(format!(".loom-{id}.mp4.part"))
}

/// `Content-Range: bytes <start>-<end>/<total>` → (start, end, total).
fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end, total) = (start.parse().ok()?, end.parse().ok()?, total.parse().ok()?);
    (start <= end && end < total).then_some((start, end, total))
}

/// Where a response continues the partial file: `Some((start, total))` to
/// append, `Some((0, length))` for a full body, `None` when it cannot be used
/// and the download must start over.
fn resume_point(
    have: u64,
    status: reqwest::StatusCode,
    range: Option<&str>,
    length: Option<u64>,
) -> Option<(u64, Option<u64>)> {
    match status.as_u16() {
        206 => {
            let (start, end, total) = content_range(range?)?;
            (have > 0 && start == have && end + 1 == total).then_some((start, Some(total)))
        }
        200 => Some((0, length)),
        _ => None,
    }
}

/// Direct MP4 with upstream-style resume: continue `.loom-<id>.mp4.part`
/// from its size when the server honours the range, otherwise start over.
async fn save_direct(
    url: &Url,
    dest: &Path,
    id: &str,
    resume: bool,
    cancel: &CancellationToken,
    progress: &impl Fn(TransferProgress),
) -> Result<(), SaveError> {
    let part = partial_path(dest, id);
    let have = match resume {
        true => tokio::fs::metadata(&part).await.map_or(0, |m| m.len()),
        false => {
            let _ = tokio::fs::remove_file(&part).await;
            0
        }
    };
    let fetch = |offset| cancelable(cancel, async move { Ok(fetch_media_from(url.clone(), offset).await?) });
    let response = fetch(have).await?;
    let header = |r: &reqwest::Response, name| {
        r.headers().get(name).and_then(|v| v.to_str().ok()).map(String::from)
    };
    let range = header(&response, reqwest::header::CONTENT_RANGE);
    let decision = resume_point(have, response.status(), range.as_deref(), response.content_length());
    let (response, start, total) = match decision {
        Some((start, total)) => (response, start, total),
        None => {
            // Unusable range reply (for example 416 for a stale file): start over.
            drop(response);
            let _ = tokio::fs::remove_file(&part).await;
            let response = fetch(0).await?;
            let total = response.content_length();
            (response, 0, total)
        }
    };
    let partial = Partial {
        path: part,
        start,
        keep: resume,
    };
    save_response(response, dest, &partial, total, cancel, progress).await
}

async fn save_response(
    mut response: reqwest::Response,
    dest: &Path,
    partial: &Partial,
    total: Option<u64>,
    cancel: &CancellationToken,
    progress: &impl Fn(TransferProgress),
) -> Result<(), SaveError> {
    let discard = || async { let _ = tokio::fs::remove_file(&partial.path).await; };
    let opened = if partial.start == 0 {
        tokio::fs::File::create(&partial.path).await
    } else {
        tokio::fs::OpenOptions::new().append(true).open(&partial.path).await
    };
    let mut out = opened.map_err(|_| SaveError::Storage)?;
    let mut bytes = partial.start;
    let mut last = Instant::now();
    progress(TransferProgress {
        bytes,
        total,
        percent: None,
        finalizing: false,
    });
    loop {
        let chunk = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                drop(out);
                discard().await;
                return Err(SaveError::Canceled);
            }
            result = response.chunk() => match result {
                Ok(chunk) => chunk,
                Err(_) => {
                    // Keep what arrived so the next attempt can continue it.
                    let _ = out.flush().await;
                    drop(out);
                    if !partial.keep {
                        discard().await;
                    }
                    return Err(SaveError::Interrupted);
                }
            },
        };
        let Some(chunk) = chunk else { break };
        if out.write_all(&chunk).await.is_err() {
            drop(out);
            discard().await;
            return Err(SaveError::Storage);
        }
        bytes += chunk.len() as u64;
        if last.elapsed() >= Duration::from_millis(150) {
            progress(TransferProgress {
                bytes,
                total,
                percent: None,
                finalizing: false,
            });
            last = Instant::now();
        }
    }
    let synced = async {
        out.flush().await?;
        out.sync_all().await
    }
    .await;
    drop(out);
    if synced.is_err() {
        discard().await;
        return Err(SaveError::Storage);
    }
    if bytes == 0 || total.is_some_and(|expected| expected != bytes) {
        discard().await;
        return Err(SaveError::InvalidMedia);
    }
    progress(TransferProgress {
        bytes,
        total,
        percent: None,
        finalizing: true,
    });
    // From here the partial is complete: verification failure or a name
    // collision deletes it (TempPath), success moves it into place.
    let complete = tempfile::TempPath::try_from_path(&partial.path).map_err(|_| SaveError::Storage)?;
    finish(
        complete,
        dest,
        bytes,
        cancel,
        Expect::Video,
    )
    .await
}

pub(crate) async fn finish(
    temp_path: tempfile::TempPath,
    dest: &Path,
    bytes: u64,
    cancel: &CancellationToken,
    expect: Expect,
) -> Result<(), SaveError> {
    let verify_path = temp_path.to_path_buf();
    tokio::task::spawn_blocking(move || verify_media(&verify_path, bytes, expect))
        .await
        .map_err(|_| SaveError::InvalidMedia)??;
    if cancel.is_cancelled() {
        return Err(SaveError::Canceled);
    }
    persist(temp_path, dest)
}

fn verify_media(path: &Path, size: u64, expect: Expect) -> Result<(), SaveError> {
    let file = File::open(path).map_err(|_| SaveError::Storage)?;
    let mut reader =
        Mp4Reader::read_header(BufReader::new(file), size).map_err(|_| SaveError::InvalidMedia)?;
    if reader.size() != size {
        return Err(SaveError::InvalidMedia);
    }
    let tracks: Vec<_> = reader
        .tracks()
        .iter()
        .filter_map(|(&id, track)| {
            let kind = track.track_type().ok()?;
            matches!(kind, TrackType::Video | TrackType::Audio)
                .then(|| (kind, id, track.sample_count()))
        })
        .collect();
    let required = match expect {
        Expect::Video => TrackType::Video,
        Expect::Audio => TrackType::Audio,
    };
    if !tracks.iter().any(|(kind, _, _)| *kind == required) {
        return Err(SaveError::InvalidMedia);
    }
    for (_, id, samples) in tracks {
        if samples == 0 {
            return Err(SaveError::InvalidMedia);
        }
        for index in [1, samples] {
            let sample = reader
                .read_sample(id, index)
                .map_err(|_| SaveError::InvalidMedia)?;
            if sample.is_none_or(|s| s.bytes.is_empty()) {
                return Err(SaveError::InvalidMedia);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_names_drop_known_extensions_and_unsafe_characters() {
        assert_eq!(custom_base(" Demo: v2.mp4 ").unwrap(), "Demo- v2");
        assert_eq!(custom_base("notes.transcript.json").unwrap(), "notes");
        assert_eq!(custom_base("clip.final").unwrap(), "clip.final");
        assert!(matches!(custom_base("CON.mp4"), Err(SaveError::InvalidFileName)));
        assert_eq!(custom_base("?.m4a").unwrap(), "-");
    }

    #[test]
    fn collision_rename_finds_a_free_set_of_names() {
        let dir = tempfile::tempdir().unwrap();
        let options = SaveOptions {
            transcript: true,
            ..SaveOptions::default()
        };
        std::fs::write(dir.path().join("rec.mp4"), b"x").unwrap();
        std::fs::write(dir.path().join("rec (2).transcript.json"), b"x").unwrap();
        let with_captions = SaveOptions {
            captions: true,
            ..SaveOptions::default()
        };
        std::fs::write(dir.path().join("clip.captions.vtt"), b"x").unwrap();
        assert_eq!(
            Plan::new(dir.path(), "clip", &with_captions).taken().unwrap().as_deref(),
            Some("clip.captions.vtt")
        );
        assert_eq!(
            Plan::new(dir.path(), "rec", &options).taken().unwrap().as_deref(),
            Some("rec.mp4")
        );
        let free = (2..10)
            .map(|n| Plan::new(dir.path(), &format!("rec ({n})"), &options))
            .find(|p| matches!(p.taken(), Ok(None)))
            .unwrap();
        assert_eq!(free.media.unwrap(), dir.path().join("rec (3).mp4"));
    }

    #[test]
    fn the_seek_sprite_sits_beside_its_index() {
        let vtt = Url::parse("https://cdn.loom.com/mediametadata/seekpreview/a/index.vtt?Policy=p").unwrap();
        let image = sprite_beside(&vtt).unwrap();
        assert_eq!(image.as_str(), "https://cdn.loom.com/mediametadata/seekpreview/a/index.jpg?Policy=p");
        assert!(sprite_beside(&Url::parse("https://cdn.loom.com/a.json").unwrap()).is_none());
    }

    #[test]
    fn extras_join_the_collision_check() {
        let options = SaveOptions {
            gif: true,
            seek_preview: true,
            ..SaveOptions::default()
        };
        for name in ["rec-thumbnail.gif", "rec-seek-preview.jpg", "rec-seek-preview.vtt"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join(name), b"x").unwrap();
            assert_eq!(
                Plan::new(dir.path(), "rec", &options).taken().unwrap().as_deref(),
                Some(name)
            );
        }
    }

    #[test]
    fn seek_urls_combine_page_data_and_the_fallback() {
        use crate::page::SeekPreview;
        let u = |s: &str| Url::parse(&format!("https://cdn.loom.com/mediametadata/seekpreview/a/{s}")).unwrap();
        let both = SeekPreview { image: Some(u("s.jpg")), vtt: Some(u("i.vtt")) };
        assert_eq!(pick_seek_urls(both, Err(SaveError::Interrupted)).unwrap(), (Some(u("s.jpg")), Some(u("i.vtt"))));
        let vtt_only = SeekPreview { image: None, vtt: Some(u("i.vtt")) };
        assert_eq!(pick_seek_urls(vtt_only, Ok(None)).unwrap(), (Some(u("i.jpg")), Some(u("i.vtt"))));
        assert_eq!(pick_seek_urls(SeekPreview::default(), Ok(Some(u("g.vtt")))).unwrap(), (Some(u("g.jpg")), Some(u("g.vtt"))));
        assert_eq!(pick_seek_urls(SeekPreview::default(), Ok(None)).unwrap(), (None, None));
        // A failed fallback keeps a sprite the page already gave …
        let image_only = SeekPreview { image: Some(u("s.jpg")), vtt: None };
        assert_eq!(pick_seek_urls(image_only, Err(SaveError::Interrupted)).unwrap(), (Some(u("s.jpg")), None));
        // … and is reported only when there is nothing else.
        assert!(matches!(pick_seek_urls(SeekPreview::default(), Err(SaveError::Interrupted)), Err(SaveError::Interrupted)));
    }

    #[test]
    fn image_checks_and_the_gif_path_match_upstream() {
        assert!(is_jpeg(&[0xFF, 0xD8, 0xFF, 0xE0]) && !is_jpeg(b"<html>"));
        assert!(is_gif(b"GIF89a....") && is_gif(b"GIF87a") && !is_gif(b"GIF90a") && !is_gif(b"<html>"));
        assert_eq!(
            gif_url("0123456789abcdef0123456789abcdef").unwrap().as_str(),
            "https://cdn.loom.com/sessions/thumbnails/0123456789abcdef0123456789abcdef-00001.gif"
        );
    }

    #[test]
    fn captions_must_be_webvtt() {
        assert!(is_webvtt(b"WEBVTT\n\n00:00.000 --> 00:01.000\nHi"));
        assert!(is_webvtt(b"\xEF\xBB\xBFWEBVTT - Loom\r\n"));
        assert!(is_webvtt(b"WEBVTT"));
        assert!(!is_webvtt(b"WEBVTTX"));
        assert!(!is_webvtt(b"<Error><Code>AccessDenied</Code></Error>"));
    }

    #[test]
    fn only_a_transcript_failure_fails_a_transcript_only_download() {
        let fail = || Err(SaveError::InvalidMedia);
        assert!(matches!(extra_outcome("transcript", true, fail()), Err(SaveError::InvalidMedia)));
        for label in ["thumbnail", "captions file"] {
            let note = extra_outcome(label, true, fail()).unwrap().unwrap();
            assert!(note.starts_with(&format!("The {label} was not saved")), "{note}");
        }
        assert!(extra_outcome("transcript", false, fail()).unwrap().is_some());
        assert_eq!(extra_outcome("thumbnail", true, Ok(())).unwrap(), None);
        assert!(extra_outcome("thumbnail", false, Err(SaveError::Canceled))
            .unwrap()
            .unwrap()
            .starts_with("Canceled before"));
    }

    #[tokio::test(start_paused = true)]
    async fn retries_wait_between_attempts_and_stop_when_canceled() {
        let cancel = CancellationToken::new();
        let calls = std::cell::Cell::new(0);
        let start = tokio::time::Instant::now();
        let (result, made) = with_retries(3, &cancel, || {
            calls.set(calls.get() + 1);
            let n = calls.get();
            async move { if n < 3 { Err(SaveError::Interrupted) } else { Ok(n) } }
        })
        .await;
        assert_eq!((result.unwrap(), made, calls.get()), (3, 2, 3));
        assert_eq!(start.elapsed(), Duration::from_secs(1 + 2));

        // Other failures are not retried.
        let (result, made) =
            with_retries(3, &cancel, || async { Err::<(), _>(SaveError::InvalidMedia) }).await;
        assert!(matches!(result, Err(SaveError::InvalidMedia)) && made == 0);

        // Canceling during the wait stops without another attempt.
        let calls = std::cell::Cell::new(0);
        let canceler = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            canceler.cancel();
        });
        let (result, _) = with_retries(3, &cancel, || {
            calls.set(calls.get() + 1);
            async { Err::<(), _>(SaveError::Interrupted) }
        })
        .await;
        assert!(matches!(result, Err(SaveError::Canceled)));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn retries_back_off_exponentially_with_a_cap() {
        let waits: Vec<_> = (0..8).map(|a| retry_delay(a).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 32, 32]);
    }

    #[test]
    fn batch_prefix_names_files_and_stays_within_windows_limits() {
        let video = crate::VideoDescriptor {
            id: "0123456789abcdef0123456789abcdef".into(),
            title: Some("Demo".into()),
            title_file_name: Some("Demo".into()),
            duration_seconds: None,
            source_url: String::new(),
            source_type: crate::SourceType::Hls,
            assets: Default::default(),
        };
        let mut request = SaveRequest {
            options: SaveOptions { naming: FileNaming::Title, ..SaveOptions::default() },
            prefix: Some(("onboarding".into(), 3)),
            ..SaveRequest::default()
        };
        assert_eq!(file_base(&video, None, &request).unwrap(), "onboarding-3-Demo");
        assert_eq!(file_base(&video, Some("Custom".into()), &request).unwrap(), "onboarding-3-Custom");
        request.prefix = Some(("a:b".into(), 1));
        assert_eq!(file_base(&video, None, &request).unwrap(), "a-b-1-Demo");
        request.prefix = Some(("CON".into(), 1));
        assert!(matches!(file_base(&video, None, &request), Err(SaveError::InvalidFileName)));
        request.prefix = None;
        request.options.naming = FileNaming::Id;
        assert_eq!(file_base(&video, None, &request).unwrap(), video.id);
        // A 60+ character prefix and a 200-character title stay within the cap.
        let long = crate::VideoDescriptor { title_file_name: Some("t".repeat(200)), ..video };
        request.options.naming = FileNaming::Title;
        request.prefix = Some(("p".repeat(300), 1000));
        let base = file_base(&long, None, &request).unwrap();
        assert_eq!(base.chars().count(), MAX_BASE);
        assert!(base.starts_with(&format!("{}-1000-", "p".repeat(MAX_PREFIX))));
        assert!(base.len() + "-seek-preview.jpg".len() + " (999)".len() <= 255);
    }

    #[test]
    fn transcripts_gain_plain_text_like_upstream() {
        let data = json!({"sentences":[{"text":"Hello"},{"value":"world"}]});
        let out = format_transcript(data.clone());
        assert_eq!(out["plainText"], "Hello world");
        assert_eq!(out["raw"], data);
        assert_eq!(out["words"], json!([]));
        assert_eq!(format_transcript(json!({"x":1})), json!({"x":1}));
    }
    const VIDEO: &[u8] = include_bytes!("../tests/fixtures/orange.mp4");

    fn response(data: &'static [u8]) -> reqwest::Response {
        http::Response::builder()
            .body(reqwest::Body::from(data))
            .unwrap()
            .into()
    }

    /// Writes a whole body as a fresh, non-resumable download.
    async fn save_response(
        response: reqwest::Response,
        dest: &Path,
        cancel: &CancellationToken,
        progress: &impl Fn(TransferProgress),
    ) -> Result<(), SaveError> {
        let total = response.content_length();
        let partial = Partial {
            path: dest.with_file_name(".test.part"),
            start: 0,
            keep: false,
        };
        super::save_response(response, dest, &partial, total, cancel, progress).await
    }

    /// A body that yields `data` and then fails like a dropped connection.
    fn cut_off(data: &'static [u8]) -> reqwest::Response {
        let chunks: Vec<Result<&'static [u8], std::io::Error>> =
            vec![Ok(data), Err(std::io::Error::other("connection reset"))];
        http::Response::builder()
            .body(reqwest::Body::wrap_stream(futures_util::stream::iter(chunks)))
            .unwrap()
            .into()
    }

    #[test]
    fn range_replies_continue_only_where_the_file_stopped() {
        use reqwest::StatusCode as S;
        let ok = Some("bytes 100-199/200");
        assert_eq!(resume_point(100, S::PARTIAL_CONTENT, ok, Some(100)), Some((100, Some(200))));
        assert_eq!(resume_point(100, S::OK, None, Some(200)), Some((0, Some(200))));
        for (have, range) in [
            (90, ok),                              // starts elsewhere
            (100, Some("bytes 100-150/200")),      // does not reach the end
            (100, Some("bytes 100-199/*")),        // unknown total
            (100, Some("items 100-199/200")),      // wrong unit
            (100, None),                           // missing header
            (0, Some("bytes 0-199/200")),          // nothing to continue
        ] {
            assert_eq!(resume_point(have, S::PARTIAL_CONTENT, range, None), None, "{range:?}");
        }
        assert_eq!(resume_point(100, S::RANGE_NOT_SATISFIABLE, Some("bytes */50"), None), None);
        assert_eq!(content_range("bytes 0-0/1"), Some((0, 0, 1)));
        assert_eq!(content_range("bytes 5-4/10"), None);
    }

    #[tokio::test]
    async fn an_interrupted_download_continues_from_its_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("rec.mp4");
        let token = CancellationToken::new();
        let id = "0123456789abcdef0123456789abcdef";
        let part = partial_path(&dest, id);
        let half = VIDEO.len() / 2;
        let keep = Partial { path: part.clone(), start: 0, keep: true };
        let first = super::save_response(cut_off(&VIDEO[..half]), &dest, &keep, Some(VIDEO.len() as u64), &token, &|_| {}).await;
        assert!(matches!(first, Err(SaveError::Interrupted)));
        assert_eq!(std::fs::read(&part).unwrap(), &VIDEO[..half]);
        assert!(!dest.exists());

        let rest = Partial { path: part.clone(), start: half as u64, keep: true };
        let seen = std::cell::Cell::new(0);
        super::save_response(response(&VIDEO[half..]), &dest, &rest, Some(VIDEO.len() as u64), &token,
            &|p: TransferProgress| seen.set(p.bytes)).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), VIDEO);
        assert_eq!(seen.get(), VIDEO.len() as u64);
        assert!(!part.exists());
    }

    #[tokio::test]
    async fn partial_files_are_dropped_when_resume_is_off_or_on_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("rec.mp4");
        let token = CancellationToken::new();
        let part = partial_path(&dest, "a");
        let off = Partial { path: part.clone(), start: 0, keep: false };
        let result = super::save_response(cut_off(&VIDEO[..10]), &dest, &off, None, &token, &|_| {}).await;
        assert!(matches!(result, Err(SaveError::Interrupted)) && !part.exists());

        let on = Partial { path: part.clone(), start: 0, keep: true };
        let result = super::save_response(response(VIDEO), &dest, &on, None, &token, &|_| token.cancel()).await;
        assert!(matches!(result, Err(SaveError::Canceled)) && !part.exists());
        assert_eq!(dir.path().read_dir().unwrap().count(), 0);
    }
    #[tokio::test]
    async fn saves_verified_video_and_never_replaces_a_destination() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("movie.mp4");
        let token = CancellationToken::new();
        let observed = std::cell::Cell::new(0);
        save_response(response(VIDEO), &path, &token, &|p: TransferProgress| observed.set(p.bytes))
            .await
            .unwrap();
        assert_eq!(observed.get(), VIDEO.len() as u64);
        assert_eq!(std::fs::read(&path).unwrap(), VIDEO);
        std::fs::write(&path, b"previous file").unwrap();
        assert!(matches!(
            save_response(response(VIDEO), &path, &token, &|_| {}).await,
            Err(SaveError::Exists(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"previous file");
        assert_eq!(folder.path().read_dir().unwrap().count(), 1);
    }
    #[tokio::test]
    async fn fresh_retry_preserves_distinct_audio_and_video_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("retry.mp4");
        let token = CancellationToken::new();
        assert!(
            save_response(response(b"interrupted"), &path, &token, &|_| {})
                .await
                .is_err()
        );
        assert_eq!(dir.path().read_dir().unwrap().count(), 0);
        let media = include_bytes!("../tests/fixtures/orange-av.mp4");
        save_response(response(media), &path, &token, &|_| {})
            .await
            .unwrap();
        let reader =
            Mp4Reader::read_header(File::open(&path).unwrap(), media.len() as u64).unwrap();
        for kind in [TrackType::Video, TrackType::Audio] {
            assert!(
                reader
                    .tracks()
                    .values()
                    .any(|track| track.track_type().ok().as_ref() == Some(&kind)
                        && track.sample_count() > 0)
            );
        }
        assert_eq!(std::fs::read(path).unwrap(), media.as_slice());
    }

    #[tokio::test]
    async fn invalid_and_canceled_transfers_leave_no_files() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("movie.mp4");
        let token = CancellationToken::new();
        for body in [b"<html>login</html>".as_slice(), &VIDEO[..VIDEO.len() / 2]] {
            assert!(matches!(
                save_response(response(body), &path, &token, &|_| {}).await,
                Err(SaveError::InvalidMedia)
            ));
        }
        assert!(matches!(
            save_response(response(VIDEO), &path, &token, &|_| token.cancel()).await,
            Err(SaveError::Canceled)
        ));
        assert_eq!(folder.path().read_dir().unwrap().count(), 0);
    }
}
