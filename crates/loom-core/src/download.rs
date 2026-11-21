use crate::{
    AnalyzeError, CancellationToken, Collision, Content, FileNaming, SaveOptions,
    hls::find_ffmpeg, normalize_share_url,
    options::sanitize_filename,
    page::MediaSource,
    transport::{fetch_asset, fetch_media, read_limited, resolve_recording},
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
        }
    }

    fn taken(&self) -> Result<Option<String>, SaveError> {
        for path in [&self.media, &self.transcript, &self.thumbnail]
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
            crate::hls::save(&exe()?, stream, options.quality, audio_only, path, cancel, progress)
                .await?;
        }
        MediaSource::Mp4(url) if audio_only => {
            let job = crate::hls::Job::single(crate::hls::stream_options(None), url.as_str())
                .maps(&["-map", "0:a:0", "-vn"]);
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

    let mut saved = SavedFiles::default();
    if let Some(path) = plan.media {
        let media = page.media?;
        let mut attempt = 0;
        let note = loop {
            match save_media(request, &media, &path, cancel, &progress).await {
                Err(SaveError::Interrupted | SaveError::Page(AnalyzeError::Network))
                    if attempt < request.retries =>
                {
                    attempt += 1;
                }
                result => break result?,
            }
        };
        if attempt > 0 {
            saved.notes.push(format!(
                "The transfer was interrupted and retried {attempt} time{}.",
                if attempt == 1 { "" } else { "s" }
            ));
        }
        saved.notes.extend(note);
        saved.files.push(path);
    }
    let transcript_path = plan.transcript;
    let thumbnail_path = plan.thumbnail;

    let mut record = |label: &str, path: PathBuf, result: Result<(), SaveError>| {
        match result {
            Ok(()) => saved.files.push(path),
            Err(error) if transcript_only => return Err(error),
            Err(SaveError::Canceled) => saved
                .notes
                .push(format!("Canceled before the {label} was saved.")),
            Err(error) => saved
                .notes
                .push(format!("The {label} was not saved: {error}")),
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
    if let Some(path) = thumbnail_path {
        let result = cancelable(cancel, save_thumbnail(page.video.id.clone(), path.clone())).await;
        record("thumbnail", path, result)?;
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
