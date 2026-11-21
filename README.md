# Loom Archive

Loom Archive is a Windows-first Tauri 2 desktop app for inspecting and saving Loom recordings. Paste a link and choose **Analyze link** to see the recording ID, title, media source and optional assets. The download page keeps the common choices in view: an optional file-name override, a destination folder (**Browse** changes it for the current download only), the format (video MP4, audio M4A or transcript JSON), video quality, and whether to also save the transcript, captions and thumbnail. These per-download choices are layered over your defaults and never change them. The app shows progress (a percentage for FFmpeg downloads, based on the recording's length) and supports cancellation; the form is kept while you visit Settings.

**Settings** is a separate page with saved defaults, stored in `settings.json` in the app's config folder:

| Section | Settings | Upstream equivalent |
| --- | --- | --- |
| Downloads | Default folder (or ask each time) | `--out` |
| | File naming: recording title or video ID | `--use-title` / default |
| | Default format: video, audio only (new), transcript only | default / `--transcript-only` |
| | Default quality: auto, best, 1080p, 720p or 480p and lower | `--quality` (declared upstream but unused there) |
| | Also save transcript `<name>.transcript.json`, captions `<name>.captions.vtt` and thumbnail `<name>-thumbnail.jpg` | `--transcript`, *new*, `--image` |
| | If a file exists: stop, or add a number such as `name (2)` | — (upstream overwrites) |
| Advanced | FFmpeg location (automatic or a chosen `ffmpeg.exe`), retries after an interrupted transfer, waiting 1 s, 2 s, 4 s … (up to 32 s) as upstream's backoff does | — |
| Appearance / About | Theme: dark (default), light, or match Windows; version and third-party licenses | — |

Concurrent downloads and diagnostic logging are shown as unavailable: the app downloads one recording at a time.

The interface uses React, Tailwind CSS v4 and shadcn/ui components with Lucide icons. Running `pnpm dev` in a regular browser substitutes sample data for the Tauri backend so layouts can be checked; production builds do not include it.

Auto quality passes Loom's playlist to FFmpeg unchanged, as upstream does. Any other quality reads the master playlist and passes the chosen video variant and its audio rendition to FFmpeg as separate inputs. Audio only maps the first audio stream and saves an M4A. A transcript, captions file or thumbnail that cannot be saved is reported as a note and does not undo a saved video. Existing files are never overwritten.

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

The app writes a temporary file beside the destination, checks its container and video samples, then saves it as `<recording-id>.mp4`. Existing files are preserved. Canceled or failed downloads discard their temporary file. Retrying starts a new transfer.

The offline tests include an original one-second MP4 fixture. To run the opt-in live check, put FFmpeg on PATH, set `LOOM_TEST_SHARE_URL` to a recording you control and run `cargo test -p loom-core --test live_download -- --ignored --nocapture`. Media from a successful test is saved in a temporary directory.

## Acknowledgements

The download flow is adapted from [EcomGraduates/loom-downloader](https://github.com/EcomGraduates/loom-downloader), and recording metadata handling draws on [robertoamoreno/loom_com_backup](https://github.com/robertoamoreno/loom_com_backup). Both are MIT licensed; their license texts are in [LICENSES](LICENSES/).
