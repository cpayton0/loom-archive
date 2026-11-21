# Loom Archive

Loom Archive is a Windows-first Tauri 2 desktop app for inspecting and saving Loom recordings. Paste a link and choose **Analyze link** to see the recording ID, title, media source and optional assets. The download page keeps the common choices in view: an optional file-name override, a destination folder (**Browse** changes it for the current download only), the format (video MP4, audio M4A or transcript JSON), video quality, and whether to also save the transcript, captions, thumbnail, GIF preview and seek preview. These per-download choices are layered over your defaults and never change them. The app shows progress (a percentage for FFmpeg downloads, based on the recording's length) and supports cancellation; the form is kept while you visit Settings.

**Settings** is a separate page with saved defaults, stored in `settings.json` in the app's config folder:

| Section | Settings | Upstream equivalent |
| --- | --- | --- |
| Downloads | Default folder (or ask each time) | `--out` |
| | File naming: recording title or video ID | `--use-title` / default |
| | Default format: video, audio only (new), transcript only | default / `--transcript-only` |
| | Default quality: auto, best, 1080p, 720p or 480p and lower | `--quality` (declared upstream but unused there) |
| | Also save transcript `<name>.transcript.json`, captions `<name>.captions.vtt`, thumbnail `<name>-thumbnail.jpg`, GIF preview `<name>-thumbnail.gif` and seek preview `<name>-seek-preview.jpg` + `.vtt` | `--transcript`, *new*, `--image`, `--gif`, `--seek-preview` |
| | If a file exists: stop, or add a number such as `name (2)` | — (upstream overwrites) |
| Advanced | FFmpeg location (automatic or a chosen `ffmpeg.exe`); batch downloads at once, pause between them and skipping already-downloaded recordings (`--list` concurrency, `--timeout`, `downloaded.log`); retries after an interrupted transfer, waiting 1 s, 2 s, 4 s … (up to 32 s) as upstream's backoff does | — |
| Appearance / About | Theme: dark (default), light, or match Windows; version and third-party licenses | — |

**Batch** (the second tab on the download page) mirrors upstream's `--list`: paste share links one per line or open a `.txt` list (read in Rust from a native file dialog; blank lines and `#` comments are ignored). Invalid lines and duplicates are flagged before starting. An optional numbering prefix names files `<prefix>-<n>-<name>` in list order, like `--prefix`. The batch uses the same format, quality, extras and destination as a single download; when no folder is set it asks once for the whole batch. Settings → Advanced controls how many downloads run at once (1–5, upstream uses 5), the pause after each one (upstream's `--timeout`) and whether to skip recordings already downloaded. Successful downloads are recorded by video ID in `downloaded.log` in the app's data folder, as upstream does. **Stop** cancels running downloads and the rest of the queue.

**Diagnostic logging** (Settings → Advanced, off by default) is the app's version of upstream's `--verbose` and `debug-page.html`. Each download writes `<UTC time>-<job>-<id>.log` to the app's log folder with the options, the media source, every FFmpeg command and its error output, retries, resume decisions and the outcome. CloudFront cookies and signed query strings are replaced with `<redacted>`, and the Windows profile folder with `%USERPROFILE%`. A copy of the share page is saved beside it as `-page.html` with the same redaction plus the Apollo `credentials` values (`Policy`, `Signature`, `KeyPairId`) blanked, so neither file holds playback credentials. Logs still name recordings, files and folders, so share them with care. The newest 40 logs are kept, and **Open log folder** opens the folder.

The interface uses React, Tailwind CSS v4 and shadcn/ui components with Lucide icons. Running `pnpm dev` in a regular browser substitutes sample data for the Tauri backend so layouts can be checked; production builds do not include it.

Auto quality passes Loom's playlist to FFmpeg unchanged, as upstream does. Any other quality reads the master playlist and passes the chosen video variant and its audio rendition to FFmpeg as separate inputs. Audio only maps the first audio stream and saves an M4A. The seek preview is read from the share page or, failing that, from Loom's `GetVideoSeekPreview` query, as upstream does. A transcript, captions file, thumbnail or preview that cannot be saved is reported as a note and does not undo a saved video. Existing files are never overwritten.

## Run locally

Install Node.js 22.12+ (or 24 LTS), pnpm 11.24, and stable Rust 1.94 or newer. On Windows, Cargo must be on PATH, and the MSVC C++ build tools, Windows SDK and WebView2 Runtime must be installed. [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)

```sh
pnpm install --frozen-lockfile
cargo test
pnpm tauri dev
```

To build a standalone desktop executable, run `pnpm tauri build --no-bundle`. On Windows it is written to `target/release/loom-archive.exe`. The frontend assets are embedded in the executable. Node.js and pnpm are development tools.

## Tests

`cargo test` runs the framework-independent core suite. To include the Tauri command and window permission checks, run `cargo test --workspace`. For a disconnected test run, first cache the pinned dependencies with `cargo fetch --locked`, then run `cargo test --workspace --locked --offline`. The share pages and HTTP responses in tests are synthetic. Windows CI also builds the desktop application.

## Share links

The analyzer accepts HTTPS links on `loom.com` or `www.loom.com` to `/share/<id>` or `/embed/<id>`, where `<id>` is the 32-digit hexadecimal ID, optionally after a title slug (`/share/Weekly-sync-<id>`). Backslashes that shells add to pasted links are removed first, as upstream does. It discards the query and fragment, lowercases the ID, and fetches the canonical page without credentials. A page that needs a login, redirects, or changes its data format may yield an error. Asset labels describe references found in page metadata.

Downloads follow [EcomGraduates/loom-downloader](https://github.com/EcomGraduates/loom-downloader). Rust fetches the share page and reads the HLS stream that Loom supplies to its player from `window.__APOLLO_STATE__`. That stream is the requested recording's `M3U8` field, its playlist URL and CloudFront credentials. FFmpeg receives the playlist URL with a `Referer: https://www.loom.com/` header and the credentials as `CloudFront-*` cookies. It runs with `-c copy -bsf:a aac_adtstoasc -movflags +faststart`, which selects the video rendition and the separate audio rendition and copies both into one MP4 without re-encoding. When the page reports no HLS stream, the app falls back to a direct `cdn.loom.com` MP4 for that recording. **Open in Loom** opens the canonical recording in your default browser.

HLS downloads need FFmpeg on PATH or beside the Loom Archive executable. Playlist hosts are limited to Loom's media hosts, and FFmpeg is restricted to the `https`, `tls`, `tcp` and `crypto` protocols. An FFmpeg HTTP 401, 403 or 404 response is reported as a typed error.

The app writes a temporary file beside the destination, checks its container and video samples, then saves it under the chosen name. Existing files are preserved. With **Resume interrupted downloads** on (Settings → Advanced, the default, like upstream's `--resume`), a direct MP4 whose connection drops is kept as `.loom-<id>.mp4.part`, with the file's size and ETag (or Last-Modified) in a `.meta` file beside it. The next attempt (an automatic retry or a later download of the same recording to the same folder within a week) asks for the remaining bytes with `Range` and `If-Range`, and appends them only when the server answers `206` with a `Content-Range` that continues the partial and matches the recorded size. A changed file, an unusable reply, a partial without metadata or one older than a week starts over. Canceling at any stage and files that fail verification delete the partial. HLS downloads always restart because FFmpeg reads the playlist from the beginning.

The offline tests include an original one-second MP4 fixture. To run the opt-in live check, put FFmpeg on PATH, set `LOOM_TEST_SHARE_URL` to a recording you control and run `cargo test -p loom-core --test live_download -- --ignored --nocapture`. Media from a successful test is saved in a temporary directory.

## Acknowledgements

The download flow is adapted from [EcomGraduates/loom-downloader](https://github.com/EcomGraduates/loom-downloader), and recording metadata handling draws on [robertoamoreno/loom_com_backup](https://github.com/robertoamoreno/loom_com_backup). Both are MIT licensed; their license texts are in [LICENSES](LICENSES/).
