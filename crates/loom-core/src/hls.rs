//! HLS export, modeled on upstream `downloadHlsVideo`: FFmpeg reads the
//! playlist the share page reported (sending Loom's Referer and the CloudFront
//! cookies from the page), selects the video and audio renditions, and copies
//! both into one MP4 without re-encoding.

use crate::{
    AnalyzeError, CancellationToken, Quality, SaveError, TransferProgress,
    download::{Expect, finish},
    page::HlsSource,
    transport::{checked_media_host, pinned_client, read_limited, validate_media_response},
};
use m3u8_rs::{AlternativeMediaType, MasterPlaylist, Playlist, VariantStream};
use url::Url;
use std::{
    env,
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
};

/// A configured executable wins; otherwise look beside the app, then on PATH.
pub fn find_ffmpeg(custom: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = custom.filter(|p| p.is_file()) {
        return Some(path.to_owned());
    }
    ffmpeg_path().ok()
}

pub(crate) fn ffmpeg_path() -> Result<PathBuf, SaveError> {
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    let sibling = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join(name)));
    sibling
        .into_iter()
        .chain(
            env::split_paths(&env::var_os("PATH").unwrap_or_default())
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join(name)),
        )
        .find(|path| path.is_file())
        .ok_or(SaveError::MissingFfmpeg)
}

/// Input options for a Loom playback stream. The header block matches
/// upstream; the protocol whitelist keeps FFmpeg on HTTPS for every playlist,
/// key and segment it follows.
pub(crate) fn stream_options(cookies: Option<&str>) -> Vec<String> {
    let mut headers = String::from("Referer: https://www.loom.com/\r\n");
    if let Some(cookies) = cookies {
        headers.push_str("Cookie: ");
        headers.push_str(cookies);
        headers.push_str("\r\n");
    }
    [
        "-protocol_whitelist",
        "https,tls,tcp,crypto",
        "-rw_timeout",
        "30000000",
        "-headers",
        &headers,
    ]
    .map(String::from)
    .into()
}

fn classify_failure(stderr: &str) -> SaveError {
    let has = |needle: &str| stderr.contains(needle);
    if has("401 Unauthorized") {
        AnalyzeError::AuthenticationRequired.into()
    } else if has("403 Forbidden") {
        AnalyzeError::PermissionDenied.into()
    } else if has("404 Not Found") || has("410 Gone") {
        AnalyzeError::Unavailable.into()
    } else if has("429 Too Many Requests") {
        AnalyzeError::RateLimited.into()
    } else if ["Connection", "timed out", "resolve", "I/O error", "End of file"]
        .iter()
        .any(|needle| has(needle))
    {
        SaveError::Interrupted
    } else {
        SaveError::Processing
    }
}

/// One FFmpeg invocation: inputs (each with its own options) and stream maps.
/// No maps means FFmpeg's default selection, which is what upstream relies on.
#[derive(Debug, Default)]
pub(crate) struct Job {
    pub inputs: Vec<(Vec<String>, String)>,
    pub maps: Vec<String>,
    /// Recording length from the share page, used to report a percentage.
    pub duration: Option<f64>,
}

impl Job {
    pub(crate) fn single(options: Vec<String>, input: &str) -> Self {
        Self {
            inputs: vec![(options, input.into())],
            maps: Vec::new(),
            duration: None,
        }
    }

    pub(crate) fn maps(mut self, maps: &[&str]) -> Self {
        self.maps = maps.iter().map(|m| m.to_string()).collect();
        self
    }

    pub(crate) fn duration(mut self, seconds: Option<f64>) -> Self {
        self.duration = seconds.filter(|d| d.is_finite() && *d > 0.0);
        self
    }
}

/// Accumulates one `-progress` block (key=value lines ending in `progress=`).
#[derive(Default)]
struct ProgressBlock {
    bytes: u64,
    out_us: Option<f64>,
}

impl ProgressBlock {
    fn line(&mut self, line: &str, duration: Option<f64>) -> Option<TransferProgress> {
        let (key, value) = line.split_once('=')?;
        match key {
            "total_size" => self.bytes = value.parse().unwrap_or(self.bytes),
            // FFmpeg's `out_time_ms` is also microseconds; prefer the explicit key.
            "out_time_us" => self.out_us = value.parse().ok().or(self.out_us),
            "progress" => {
                let percent = duration.zip(self.out_us).map(|(total, us)| {
                    (us / 1_000_000.0 / total * 100.0).clamp(0.0, 100.0)
                });
                return Some(TransferProgress {
                    bytes: self.bytes,
                    total: None,
                    percent,
                    finalizing: false,
                });
            }
            _ => {}
        }
        None
    }
}

async fn run_ffmpeg(
    exe: &Path,
    job: &Job,
    out: &Path,
    cancel: &CancellationToken,
    progress: &impl Fn(TransferProgress),
) -> Result<(), SaveError> {
    let mut cmd = Command::new(exe);
    cmd.kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(["-nostdin", "-hide_banner", "-nostats", "-v", "error"])
        .args(["-progress", "pipe:1"]);
    for (options, input) in &job.inputs {
        cmd.args(options).args(["-i", input]);
    }
    cmd.args(&job.maps)
        // Same output options as upstream.
        .args(["-c", "copy", "-bsf:a", "aac_adtstoasc", "-movflags", "+faststart"])
        .args(["-f", "mp4", "-y"])
        .arg(out);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let mut child = cmd.spawn().map_err(|_| SaveError::MissingFfmpeg)?;
    let (Some(stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(SaveError::Processing);
    };
    let outcome = {
        let report = async {
            let mut lines = BufReader::new(stdout).lines();
            let mut state = ProgressBlock::default();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(update) = state.line(&line, job.duration) {
                    progress(update);
                }
            }
        };
        let errors = async {
            let mut text = Vec::new();
            let _ = stderr.read_to_end(&mut text).await;
            text
        };
        let run = async {
            let (_, errors, status) = tokio::join!(report, errors, child.wait());
            (errors, status)
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            result = run => Some(result),
        }
    };
    let Some((errors, status)) = outcome else {
        let _ = child.kill().await;
        return Err(SaveError::Canceled);
    };
    match status {
        Ok(status) if status.success() => Ok(()),
        _ => Err(classify_failure(&String::from_utf8_lossy(&errors))),
    }
}

pub(crate) async fn save_with(
    exe: &Path,
    job: &Job,
    expect: Expect,
    dest: &Path,
    cancel: &CancellationToken,
    progress: impl Fn(TransferProgress),
) -> Result<(), SaveError> {
    let dir = tempfile::Builder::new()
        .prefix(".loom-hls-")
        .tempdir_in(dest.parent().ok_or(SaveError::Storage)?)
        .map_err(|_| SaveError::Storage)?;
    let output = tempfile::Builder::new()
        .suffix(".mp4")
        .tempfile_in(dir.path())
        .map_err(|_| SaveError::Storage)?
        .into_temp_path();
    progress(TransferProgress {
        bytes: 0,
        total: None,
        percent: None,
        finalizing: false,
    });
    run_ffmpeg(exe, job, &output, cancel, &progress).await?;
    let size = output.metadata().map_err(|_| SaveError::Storage)?.len();
    progress(TransferProgress {
        bytes: size,
        total: Some(size),
        percent: None,
        finalizing: true,
    });
    finish(output, dest, size, cancel, expect).await
}

fn height(variant: &VariantStream) -> u64 {
    variant.resolution.map_or(0, |r| r.height)
}

/// Highest variant at or below the requested height; the smallest one when
/// every variant is taller. A `None` limit means the highest available.
fn pick_variant(master: &MasterPlaylist, limit: Option<u64>) -> Option<&VariantStream> {
    let variants = || master.variants.iter().filter(|v| !v.is_i_frame);
    let rank = |v: &&VariantStream| (height(v), v.bandwidth);
    match limit {
        None => variants().max_by_key(rank),
        Some(limit) => variants()
            .filter(|v| height(v) <= limit)
            .max_by_key(rank)
            .or_else(|| variants().min_by_key(rank)),
    }
}

fn child(base: &Url, uri: &str) -> Result<Url, SaveError> {
    let target = base.join(uri).map_err(|_| AnalyzeError::InvalidMediaUrl)?;
    checked_media_host(&target)?;
    Ok(target)
}

/// Turns a master playlist into explicit inputs for the chosen rendition.
fn variant_job(
    base: &Url,
    master: &MasterPlaylist,
    quality: Quality,
    options: &[String],
) -> Result<Option<Job>, SaveError> {
    let Some(variant) = pick_variant(master, quality.max_height()) else {
        return Ok(None);
    };
    let video = child(base, &variant.uri)?;
    let audio = variant.audio.as_ref().and_then(|group| {
        master
            .alternatives
            .iter()
            .filter(|r| &r.group_id == group && r.media_type == AlternativeMediaType::Audio)
            .filter_map(|r| Some((r.default, r.autoselect, r.uri.as_deref()?)))
            .max_by_key(|(default, auto, _)| (*default, *auto))
            .map(|(_, _, uri)| uri)
    });
    let mut job = Job::single(options.to_vec(), video.as_str());
    match audio {
        Some(uri) => {
            job.inputs
                .push((options.to_vec(), child(base, uri)?.to_string()));
            Ok(Some(job.maps(&["-map", "0:v:0", "-map", "1:a:0"])))
        }
        None => Ok(Some(job.maps(&["-map", "0:v:0", "-map", "0:a:0?"]))),
    }
}

async fn fetch_master(source: &HlsSource) -> Result<Playlist, SaveError> {
    let mut request = pinned_client(checked_media_host(&source.url)?)
        .await?
        .get(source.url.clone())
        .header("Referer", "https://www.loom.com/");
    if let Some(cookies) = &source.cookies {
        request = request.header("Cookie", cookies);
    }
    let response = request.send().await.map_err(|_| SaveError::Interrupted)?;
    let body = read_limited(validate_media_response(response).await?, 2 * 1024 * 1024).await?;
    m3u8_rs::parse_playlist_res(&body).map_err(|_| SaveError::Processing)
}

pub(crate) async fn save(
    exe: &Path,
    source: &HlsSource,
    quality: Quality,
    audio_only: bool,
    duration: Option<f64>,
    dest: &Path,
    cancel: &CancellationToken,
    progress: impl Fn(TransferProgress),
) -> Result<(), SaveError> {
    let options = stream_options(source.cookies.as_deref());
    let upstream = || Job::single(options.clone(), source.url.as_str());
    let (job, expect) = if audio_only {
        (upstream().maps(&["-map", "0:a:0", "-vn"]), Expect::Audio)
    } else if quality == Quality::Auto {
        (upstream(), Expect::Video)
    } else {
        let master = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(SaveError::Canceled),
            playlist = fetch_master(source) => playlist?,
        };
        let job = match master {
            Playlist::MasterPlaylist(master) => {
                variant_job(&source.url, &master, quality, &options)?
            }
            Playlist::MediaPlaylist(_) => None,
        };
        (job.unwrap_or_else(upstream), Expect::Video)
    };
    save_with(exe, &job.duration(duration), expect, dest, cancel, progress).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    #[test]
    fn requests_carry_upstream_headers_and_stay_on_https() {
        let source = HlsSource {
            url: Url::parse("https://luna.loom.com/id/playlist.m3u8").unwrap(),
            cookies: Some("CloudFront-Signature=abc; CloudFront-Key-Pair-Id=K".into()),
        };
        let options = stream_options(source.cookies.as_deref());
        let value = |flag: &str| {
            let i = options.iter().position(|o| o == flag).unwrap();
            options[i + 1].as_str()
        };
        assert_eq!(
            value("-headers"),
            "Referer: https://www.loom.com/\r\nCookie: CloudFront-Signature=abc; CloudFront-Key-Pair-Id=K\r\n"
        );
        assert_eq!(value("-protocol_whitelist"), "https,tls,tcp,crypto");
        let options = stream_options(None);
        assert!(options.contains(&"Referer: https://www.loom.com/\r\n".to_string()));
    }

    #[test]
    fn progress_blocks_report_bytes_and_a_clamped_percentage() {
        let mut block = ProgressBlock::default();
        let feed = |block: &mut ProgressBlock, lines: &[&str], duration| {
            lines.iter().filter_map(|l| block.line(l, duration)).last()
        };
        let early = feed(&mut block, &["out_time_us=N/A", "total_size=48", "progress=continue"], Some(10.0)).unwrap();
        assert_eq!((early.bytes, early.percent), (48, None));
        let half = feed(&mut block, &["out_time_us=5000000", "total_size=900", "progress=continue"], Some(10.0)).unwrap();
        assert_eq!((half.bytes, half.percent), (900, Some(50.0)));
        let over = feed(&mut block, &["out_time_us=12000000", "progress=end"], Some(10.0)).unwrap();
        assert_eq!(over.percent, Some(100.0));
        let unknown = feed(&mut block, &["out_time_us=1000000", "progress=continue"], None).unwrap();
        assert_eq!(unknown.percent, None);
        assert!(Job::single(Vec::new(), "x").duration(Some(f64::NAN)).duration.is_none());
    }

    #[test]
    fn ffmpeg_http_failures_map_to_typed_errors() {
        let case = |text| classify_failure(text).to_string();
        assert_eq!(
            case("[https @ 0x1] HTTP error 403 Forbidden"),
            SaveError::from(AnalyzeError::PermissionDenied).to_string()
        );
        assert_eq!(
            case("Server returned 404 Not Found"),
            SaveError::from(AnalyzeError::Unavailable).to_string()
        );
        assert!(matches!(
            classify_failure("Connection refused"),
            SaveError::Interrupted
        ));
        assert!(matches!(
            classify_failure("Invalid data found"),
            SaveError::Processing
        ));
    }

    /// Builds a master playlist with separate video and audio renditions (the
    /// layout Loom's player uses) and checks FFmpeg combines both into the MP4.
    #[tokio::test]
    #[ignore = "requires FFmpeg on PATH"]
    async fn combines_separate_hls_renditions_into_one_verified_mp4() {
        let exe = ffmpeg_path().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let hls = dir.path().join("hls");
        std::fs::create_dir(&hls).unwrap();
        let ffmpeg = |args: &[&str]| {
            let status = std::process::Command::new(&exe)
                .current_dir(&hls)
                .args(["-nostdin", "-v", "error"])
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "{args:?}");
        };
        ffmpeg(&[
            "-f", "lavfi", "-i", "testsrc=size=160x90:rate=10:duration=3",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=3",
            "-c:v", "libx264", "-g", "10", "-c:a", "aac", "-shortest", "src.mp4",
        ]);
        for track in ["v", "a"] {
            let segments = format!("{track}%03d.ts");
            ffmpeg(&[
                "-i", "src.mp4", "-map", &format!("0:{track}:0"), "-c", "copy",
                "-f", "hls", "-hls_time", "1", "-hls_playlist_type", "vod",
                "-hls_segment_filename", &segments, &format!("{track}.m3u8"),
            ]);
        }
        std::fs::write(
            hls.join("master.m3u8"),
            "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"main\",DEFAULT=YES,URI=\"a.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=20000,AUDIO=\"aud\"\nv.m3u8\n",
        )
        .unwrap();

        let local = ["-protocol_whitelist", "file,crypto"]
            .map(String::from)
            .to_vec();
        let playlist = hls.join("master.m3u8").display().to_string();
        let job = Job::single(local.clone(), &playlist).duration(Some(3.0));
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let dest = out.join("rec.mp4");

        let canceled = CancellationToken::new();
        let result = save_with(&exe, &job, Expect::Video, &dest, &canceled, |_| canceled.cancel()).await;
        assert!(matches!(result, Err(SaveError::Canceled)));
        assert_eq!(out.read_dir().unwrap().count(), 0);

        let last = std::cell::Cell::new(None);
        let peak = std::cell::Cell::new(0.0_f64);
        save_with(&exe, &job, Expect::Video, &dest, &CancellationToken::new(), |p| {
            peak.set(peak.get().max(p.percent.unwrap_or(0.0)));
            last.set(Some(p))
        })
        .await
        .unwrap();
        assert!(last.get().is_some_and(|p| p.finalizing));
        assert!(peak.get() > 50.0 && peak.get() <= 100.0, "peak {}", peak.get());
        assert_eq!(out.read_dir().unwrap().count(), 1);
        let probe = std::process::Command::new(&exe)
            .args(["-v", "error", "-xerror", "-i"])
            .arg(&dest)
            .args(["-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"])
            .status()
            .unwrap();
        assert!(probe.success());

        // Audio only: the same master playlist, audio rendition mapped alone.
        let audio = out.join("rec.m4a");
        let job = Job::single(local.clone(), &playlist).maps(&["-map", "0:a:0", "-vn"]);
        save_with(&exe, &job, Expect::Audio, &audio, &CancellationToken::new(), |_| {})
            .await
            .unwrap();
        let streams = std::process::Command::new(&exe)
            .args(["-hide_banner", "-i"])
            .arg(&audio)
            .output()
            .unwrap();
        let info = String::from_utf8_lossy(&streams.stderr);
        assert!(info.contains("Audio: aac") && !info.contains("Video:"), "{info}");

        // An explicit quality resolves the variant and its audio rendition.
        let base = Url::parse("https://luna.loom.com/rec/master.m3u8").unwrap();
        let Ok(Playlist::MasterPlaylist(master)) =
            m3u8_rs::parse_playlist_res(&std::fs::read(hls.join("master.m3u8")).unwrap())
        else {
            panic!("master playlist")
        };
        let job = variant_job(&base, &master, Quality::P720, &local)
            .unwrap()
            .unwrap();
        assert_eq!(job.inputs[0].1, "https://luna.loom.com/rec/v.m3u8");
        assert_eq!(job.inputs[1].1, "https://luna.loom.com/rec/a.m3u8");
    }

    #[test]
    fn quality_picks_the_closest_variant_at_or_below_the_limit() {
        let master = concat!(
            "#EXTM3U\n",
            "#EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=854x480\nlow.m3u8\n",
            "#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720\nmid.m3u8\n",
            "#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080\nhigh.m3u8\n",
        );
        let Ok(Playlist::MasterPlaylist(master)) = m3u8_rs::parse_playlist_res(master.as_bytes())
        else {
            panic!("master playlist")
        };
        let pick = |q: Quality| pick_variant(&master, q.max_height()).unwrap().uri.clone();
        assert_eq!(pick(Quality::Best), "high.m3u8");
        assert_eq!(pick(Quality::P1080), "high.m3u8");
        assert_eq!(pick(Quality::P720), "mid.m3u8");
        assert_eq!(pick(Quality::P480), "low.m3u8");
        let base = Url::parse("https://luna.loom.com/a/master.m3u8").unwrap();
        assert!(child(&base, "https://127.0.0.1/x.m3u8").is_err());
        let job = variant_job(&base, &master, Quality::P480, &[])
            .unwrap()
            .unwrap();
        assert_eq!(job.maps, ["-map", "0:v:0", "-map", "0:a:0?"]);
    }
}
