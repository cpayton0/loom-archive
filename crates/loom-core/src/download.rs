use crate::{
    AnalyzeError, CancellationToken, Collision, Content, FileNaming, SaveOptions,
    hls::find_ffmpeg, normalize_share_url,
    options::sanitize_filename,
    page::MediaSource,
    transport::{fetch_asset, fetch_media, read_limited, resolve_recording, seek_preview_vtt},
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
            let response =
                cancelable(cancel, async { Ok(fetch_media(url.clone()).await?) }).await?;
            save_response(response, path, cancel, progress).await?;
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
    let base = custom.unwrap_or_else(|| match options.naming {
        FileNaming::Title => page
            .video
            .title_file_name
            .clone()
            .unwrap_or_else(|| page.video.id.clone()),
        FileNaming::Id => page.video.id.clone(),
    });
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
        ..SavedFiles::default()
    };
    if let Some(path) = plan.media.take() {
        let media = page.media?;
        let duration = page.video.duration_seconds;
        let (result, attempt) = with_retries(request.retries, cancel, || {
            save_media(request, duration, &media, &path, cancel, &progress)
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

async fn save_jpeg(url: Url, path: PathBuf) -> Result<(), SaveError> {
    let body = read_limited(fetch_asset(url).await?, 50 * 1024 * 1024).await?;
    if !body.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Err(SaveError::InvalidMedia);
    }
    write_new(path, body).await
}

/// Upstream's `--gif` thumbnail path.
async fn save_gif(id: String, path: PathBuf) -> Result<(), SaveError> {
    let url = Url::parse(&format!(
        "https://cdn.loom.com/sessions/thumbnails/{id}-00001.gif"
    ))
    .map_err(|_| AnalyzeError::InvalidMediaUrl)?;
    let body = read_limited(fetch_asset(url).await?, 50 * 1024 * 1024).await?;
    if !(body.starts_with(b"GIF87a") || body.starts_with(b"GIF89a")) {
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
    let vtt = match found.vtt {
        Some(vtt) => Some(vtt),
        None => seek_preview_vtt(id).await?,
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

async fn save_response(
    mut response: reqwest::Response,
    dest: &Path,
    cancel: &CancellationToken,
    progress: &impl Fn(TransferProgress),
) -> Result<(), SaveError> {
    let dir = dest.parent().ok_or(SaveError::Storage)?;
    let temp = tempfile::Builder::new()
        .prefix(".loom-")
        .suffix(".part")
        .tempfile_in(dir)
        .map_err(|_| SaveError::Storage)?;
    let (file, temp_path) = temp.into_parts();
    let mut out = tokio::fs::File::from_std(file);
    let total = response.content_length();
    let mut bytes = 0;
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
            _ = cancel.cancelled() => return Err(SaveError::Canceled),
            result = response.chunk() => result.map_err(|_| SaveError::Interrupted)?,
        };
        let Some(chunk) = chunk else { break };
        out.write_all(&chunk)
            .await
            .map_err(|_| SaveError::Storage)?;
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
    if bytes == 0 || total.is_some_and(|expected| expected != bytes) {
        return Err(SaveError::InvalidMedia);
    }
    out.flush().await.map_err(|_| SaveError::Storage)?;
    out.sync_all().await.map_err(|_| SaveError::Storage)?;
    drop(out);
    progress(TransferProgress {
        bytes,
        total,
        percent: None,
        finalizing: true,
    });
    finish(temp_path, dest, bytes, cancel, Expect::Video).await
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
        let dir = tempfile::tempdir().unwrap();
        let options = SaveOptions {
            gif: true,
            seek_preview: true,
            ..SaveOptions::default()
        };
        std::fs::write(dir.path().join("rec-seek-preview.vtt"), b"x").unwrap();
        assert_eq!(
            Plan::new(dir.path(), "rec", &options).taken().unwrap().as_deref(),
            Some("rec-seek-preview.vtt")
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
