import { Channel, invoke } from "@tauri-apps/api/core";

export interface VideoDescriptor {
  id: string;
  title: string | null;
  titleFileName: string | null;
  durationSeconds: number | null;
  sourceUrl: string;
  sourceType: "mp4" | "hls";
  assets: { transcript: boolean; captions: boolean; thumbnail: boolean };
}

export type Content = "video" | "audioOnly" | "transcriptOnly";
export type Quality = "auto" | "best" | "1080p" | "720p" | "480p";
export type FileNaming = "id" | "title";
export type Collision = "stop" | "rename";
export type Theme = "dark" | "light" | "system";

export interface SaveOptions {
  naming: FileNaming;
  content: Content;
  quality: Quality;
  transcript: boolean;
  thumbnail: boolean;
  captions: boolean;
  gif: boolean;
  seekPreview: boolean;
  collision: Collision;
}

/** Persistent defaults, stored by the Rust side. */
export interface Settings extends SaveOptions {
  theme: Theme;
  folder: string | null;
  ffmpeg: string | null;
  retries: number;
  concurrency: number;
  batchDelaySecs: number;
  skipDownloaded: boolean;
}

export interface AppInfo { version: string; ffmpeg: string | null; }
export interface TransferProgress { bytes: number; total: number | null; percent: number | null; finalizing: boolean; }
export interface SaveResult { files: string[]; notes: string[]; }

const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

// Browser-only preview data for layout work (`pnpm dev`). Not included in builds.
const mock = import.meta.env.DEV && !inTauri ? import("./mock") : null;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (mock) return (await mock).mockInvoke<T>(cmd, args);
  return invoke<T>(cmd, args);
}

export function saveOptions(settings: Settings): SaveOptions {
  const { naming, content, quality, transcript, thumbnail, captions, gif, seekPreview, collision } = settings;
  return { naming, content, quality, transcript, thumbnail, captions, gif, seekPreview, collision };
}

export const analyzeUrl = (url: string) => call<VideoDescriptor>("analyze_url", { url });
export const getSettings = () => call<Settings>("get_settings");
export const setOptions = (options: SaveOptions) => call<Settings>("set_options", { options });
export const setTheme = (theme: Theme) => call<Settings>("set_theme", { theme });
export const setBatch = (concurrency: number, delaySecs: number, skipDownloaded: boolean) =>
  call<Settings>("set_batch", { concurrency, delaySecs, skipDownloaded });
/** For each link, whether the recording is already in the download history. */
export const downloaded = (urls: string[]) => call<boolean[]>("downloaded", { urls });
/** Opens a .txt list in a native dialog; Rust reads it and returns its non-empty lines. */
export const openLinkList = () => call<string[] | null>("open_link_list");
export const setRetries = (retries: number) => call<Settings>("set_retries", { retries });
export const chooseFolder = () => call<Settings>("choose_folder");
export const askEachTime = () => call<Settings>("ask_each_time");
export const chooseFfmpeg = () => call<Settings>("choose_ffmpeg");
export const autoFfmpeg = () => call<Settings>("auto_ffmpeg");
export const appInfo = () => call<AppInfo>("app_info");
/** Picks a folder for the current download only; returns the active override. */
export const chooseDownloadFolder = () => call<string | null>("choose_download_folder");
export const clearDownloadFolder = () => call<void>("clear_download_folder");
/** Cancels one job, or every running download when no job is given. */
export const cancelSave = (job?: number) => call<void>("cancel_save", { job: job ?? null });

let lastJob = 0;
/** A process-unique job number for save_recording and cancel_save. */
export const nextJob = () => ++lastJob;

export interface DownloadRequest {
  options: SaveOptions;
  fileName: string | null;
  /** Batch numbering: files become `<prefix>-<index>-<name>`. */
  prefix?: string | null;
  index?: number | null;
}
export const openInLoom = (url: string) => call<void>("open_in_loom", { url });

export function saveRecording(
  job: number,
  url: string,
  request: DownloadRequest,
  update: (value: TransferProgress) => void,
) {
  if (mock) return mock.then(m => m.mockSave(job, url, request, update));
  const progress = new Channel<TransferProgress>();
  progress.onmessage = update;
  return invoke<SaveResult | null>("save_recording", {
    job, url, request: { prefix: null, index: null, ...request }, progress,
  });
}

export function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error && typeof error === "object" && "message" in error && typeof error.message === "string")
    return error.message;
  return "The app could not finish this action. Please try again.";
}
