# Third-party notices

Loom Archive ships with or is derived from the following software. The license texts are in the `LICENSES` folder of the source repository. The installer also places them in the `licenses` folder next to the app.

## FFmpeg (bundled)

- **What:** the `ffmpeg.exe` program installed next to `loom-archive.exe`. Loom Archive runs it as a separate program to download HLS streams and save MP4/M4A files. It is not linked into Loom Archive.
- **Version:** FFmpeg `n9.0.2-61-gd59078794d`, built 2026-11-20 by [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds) (release `autobuild-2026-11-20-12-40`, file `ffmpeg-n9.0.2-61-gd59078794d-win64-lgpl-9.0.zip`, SHA-256 `814c62c35ca7e2b15d85b0a2a5b79bbf21783176520154f26a6df1e9df72761e`).
- **License:** GNU Lesser General Public License version 3 or later (`LICENSES/FFmpeg-LGPL-3.0.txt`). LGPL v3 adds permissions on top of the GNU General Public License version 3, which is included as `LICENSES/GPL-3.0.txt`. The build uses no GPL-only or non-free components (no `--enable-gpl`, no `--enable-nonfree`).
- **Source code:** FFmpeg is © the FFmpeg developers, <https://ffmpeg.org>.
  - **FFmpeg source:** commit `d59078794d` of <https://git.ffmpeg.org/ffmpeg.git>, also mirrored at <https://github.com/FFmpeg/FFmpeg/commit/d59078794d>.
  - **Build scripts:** BtbN's scripts that produced this binary, including the configuration and the exact versions of the libraries it links, are at <https://github.com/BtbN/FFmpeg-Builds>.
  - **Questions:** if any of these locations stops working, please open an issue at <https://github.com/cpayton0/loom-archive/issues>.
- **Replacing it:** you can use a different FFmpeg under **Settings → Advanced → FFmpeg → Choose ffmpeg.exe…**, or replace the installed `ffmpeg.exe`.

## Projects Loom Archive's download flow is based on

| Project | License | Use |
| --- | --- | --- |
| [EcomGraduates/loom-downloader](https://github.com/EcomGraduates/loom-downloader) | MIT (`LICENSES/EcomGraduates-MIT.txt`) | The download flow this app follows |
| [robertoamoreno/loom_com_backup](https://github.com/robertoamoreno/loom_com_backup) | MIT (`LICENSES/RobertoMoreno-MIT.txt`) | Reference for recording metadata |

## Libraries compiled into the app

| Component | License |
| --- | --- |
| Tauri | MIT / Apache-2.0 |
| React | MIT |
| shadcn/ui and Radix UI | MIT |
| Tailwind CSS | MIT |
| Lucide | ISC |
| Rust crates (see `Cargo.lock`) | Their respective licenses, mostly MIT / Apache-2.0 |
