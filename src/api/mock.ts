// Development-only stand-in for the Tauri backend, used when the UI is opened
// in a regular browser to check layout. Never imported by production builds.
import type { AppInfo, SaveResult, Settings, TransferProgress, VideoDescriptor } from "./tauri";

let settings: Settings = {
  theme: "dark",
  folder: "C:\\Users\\you\\Videos\\Loom Archive\\Customer calls\\2026",
  ffmpeg: null, retries: 1,
  naming: "title", content: "video", quality: "auto", transcript: true, thumbnail: false, captions: false, collision: "stop",
};
let override: string | null = null;
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

export async function mockSave(update: (p: TransferProgress) => void): Promise<SaveResult> {
  for (let bytes = 0; bytes <= 40e6; bytes += 8e6) { update({ bytes, total: null, percent: bytes / 4e5, finalizing: false }); await wait(300); }
  update({ bytes: 40e6, total: 40e6, percent: null, finalizing: true });
  await wait(300);
  const dir = override ?? settings.folder ?? "C:\\Users\\you\\Downloads";
  return { files: [`${dir}\\${video.title}.mp4`, `${dir}\\${video.title}.transcript.json`], notes: [] };
}
