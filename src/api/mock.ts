// Development-only stand-in for the Tauri backend, used when the UI is opened
// in a regular browser to check layout. Never imported by production builds.
import type { AppInfo, DownloadRequest, SaveResult, Settings, TransferProgress, VideoDescriptor } from "./tauri";

let settings: Settings = {
  theme: "dark",
  folder: "C:\\Users\\you\\Videos\\Loom Archive\\Customer calls\\2026",
  ffmpeg: null, retries: 1, concurrency: 2, batchDelaySecs: 1, skipDownloaded: true,
  naming: "title", content: "video", quality: "auto", transcript: true, thumbnail: false, captions: false, gif: false, seekPreview: false, collision: "stop",
};
let override: string | null = null;
const history = new Set<string>(["fedcba9876543210fedcba9876543210"]);
const canceled = new Set<number>();
const active = new Set<number>();
const idOf = (url: string) => url.match(/[0-9a-f]{32}/i)?.[0].toLowerCase() ?? "";
const wait = (ms: number) => new Promise(r => setTimeout(r, ms));

const video: VideoDescriptor = {
  id: "0123456789abcdef0123456789abcdef",
  title: "Quarterly planning walkthrough",
  titleFileName: "Quarterly planning walkthrough",
  durationSeconds: 276,
  sourceUrl: "https://www.loom.com/share/0123456789abcdef0123456789abcdef",
  sourceType: "hls",
  assets: { transcript: true, captions: true, thumbnail: true },
};

export async function mockInvoke<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  await wait(150);
  const result: unknown = (() => {
    switch (cmd) {
      case "analyze_url":
        if (!String(args.url).includes("loom.com/share/")) throw { code: "invalidUrl", message: "Enter an HTTPS Loom share link with a 32-character video ID." };
        return video;
      case "get_settings": return settings;
      case "set_options": return (settings = { ...settings, ...(args.options as object) });
      case "set_theme": return (settings = { ...settings, theme: args.theme as Settings["theme"] });
      case "set_batch": return (settings = { ...settings, concurrency: Number(args.concurrency),
        batchDelaySecs: Number(args.delaySecs), skipDownloaded: Boolean(args.skipDownloaded) });
      case "downloaded": return (args.urls as string[]).map(u => history.has(idOf(u)));
      case "open_link_list": return [
        "https://www.loom.com/share/11111111111111111111111111111111",
        "https://www.loom.com/share/22222222222222222222222222222222",
        "https://www.loom.com/share/fedcba9876543210fedcba9876543210",
        "not a link",
      ];
      case "cancel_save":
        // Cancel-all applies to the jobs running now, like the Rust side.
        if (args.job == null) active.forEach(j => canceled.add(j)); else canceled.add(Number(args.job));
        return null;
      case "pin_download_folder": return (override = override ?? settings.folder);
      case "set_retries": return (settings = { ...settings, retries: Number(args.retries) });
      case "choose_folder": return (settings = { ...settings, folder: "D:\\Archive\\Loom" });
      case "ask_each_time": return (settings = { ...settings, folder: null });
      case "choose_ffmpeg": return (settings = { ...settings, ffmpeg: "C:\\Tools\\ffmpeg\\bin\\ffmpeg.exe" });
      case "auto_ffmpeg": return (settings = { ...settings, ffmpeg: null });
      case "app_info": return { version: "0.1.0", ffmpeg: settings.ffmpeg ?? "C:\\Program Files\\Loom Archive\\ffmpeg.exe" } satisfies AppInfo;
      case "choose_download_folder": return (override = "E:\\Shared\\Team recordings\\Quarterly reviews");
      case "clear_download_folder": override = null; return null;
      default: return null;
    }
  })();
  return result as T;
}

export async function mockSave(job: number, url: string, request: DownloadRequest,
  update: (p: TransferProgress) => void): Promise<SaveResult | null> {
  if (url.includes("2222")) { await wait(400); throw "Loom asked us to slow down. Try again later."; }
  active.add(job);
  for (let bytes = 0; bytes <= 40e6; bytes += 8e6) {
    if (canceled.has(job)) { canceled.delete(job); active.delete(job); return null; }
    update({ bytes, total: null, percent: bytes / 4e5, finalizing: false }); await wait(300);
  }
  active.delete(job);
  history.add(idOf(url));
  void request;
  update({ bytes: 40e6, total: 40e6, percent: null, finalizing: true });
  await wait(300);
  const dir = override ?? settings.folder ?? "C:\\Users\\you\\Downloads";
  return { files: [`${dir}\\${video.title}.mp4`, `${dir}\\${video.title}.transcript.json`], notes: [] };
}
