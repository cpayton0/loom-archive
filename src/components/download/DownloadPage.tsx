import { useRef } from "react";
import { RotateCcw } from "lucide-react";
import {
  analyzeUrl, cancelSave, chooseDownloadFolder, clearDownloadFolder, errorMessage, openInLoom, saveOptions,
  saveRecording, type SaveOptions, type Settings, type VideoDescriptor,
} from "@/api/tauri";
import { Button } from "@/components/ui/button";
import { DestinationPicker } from "./DestinationPicker";
import { DownloadAction, idleJob, type Job } from "./DownloadAction";
import { DownloadOptions } from "./DownloadOptions";
import { FileNameInput } from "./FileNameInput";
import { RecordingDetails } from "./RecordingDetails";
import { RecordingUrlInput } from "./RecordingUrlInput";

/** The in-progress download form. Lives in App so it survives visits to Settings. */
export interface Session {
  url: string;
  video: VideoDescriptor | null;
  analyzing: boolean;
  analyzeError: string;
  fileName: string;
  folderOverride: string | null;
  /** Per-download choices layered over the saved defaults. Never persisted. */
  overrides: Partial<SaveOptions>;
  job: Job;
}

export const emptySession: Session = {
  url: "", video: null, analyzing: false, analyzeError: "", fileName: "", folderOverride: null, overrides: {}, job: idleJob,
};

type Update = (change: (s: Session) => Session) => void;

const extensions = { video: ".mp4", audioOnly: ".m4a", transcriptOnly: ".transcript.json" } as const;

export function effectiveOptions(settings: Settings, overrides: Partial<SaveOptions>): SaveOptions {
  return { ...saveOptions(settings), ...overrides };
}

export function DownloadPage({ settings, session, update }: { settings: Settings; session: Session; update: Update }) {
  const { video, job } = session;
  // Ignores results from an earlier analysis when a newer link was pasted meanwhile.
  const latest = useRef(0);
  const options = effectiveOptions(settings, session.overrides);
  const busy = job.saving;
  const setJob = (change: Partial<Job>) => update(s => ({ ...s, job: { ...s.job, ...change } }));
  const changed = Object.entries(session.overrides).some(([k, v]) => settings[k as keyof SaveOptions] !== v);

  async function analyze(input = session.url) {
    const url = input.trim();
    const request = ++latest.current;
    update(s => ({ ...s, url: input, analyzing: true, analyzeError: "", video: null, fileName: "", folderOverride: null, job: idleJob }));
    clearDownloadFolder().catch(() => {});
    try {
      const result = await analyzeUrl(url);
      if (request === latest.current) update(s => ({ ...s, analyzing: false, video: result }));
    } catch (cause) {
      if (request === latest.current) update(s => ({ ...s, analyzing: false, analyzeError: errorMessage(cause) }));
    }
  }

  async function browse() {
    try {
      const folder = await chooseDownloadFolder();
      update(s => ({ ...s, folderOverride: folder }));
    } catch (cause) { setJob({ error: errorMessage(cause) }); }
  }

  async function useDefault() {
    try { await clearDownloadFolder(); update(s => ({ ...s, folderOverride: null })); }
    catch (cause) { setJob({ error: errorMessage(cause) }); }
  }

  async function download() {
    if (!video) return;
    const askFolder = !session.folderOverride && !settings.folder;
    update(s => ({ ...s, job: { ...idleJob, saving: true, message: askFolder ? "Choose a folder…" : "Starting…" } }));
    try {
      const result = await saveRecording(video.sourceUrl,
        // Captions shown as unavailable are not requested.
        { options: { ...options, captions: options.captions && video.assets.captions },
          fileName: session.fileName.trim() || null },
        progress => setJob({ progress, message: progress.finalizing ? "Finishing…" : "Downloading…" }));
      update(s => ({ ...s, job: { ...idleJob, result,
        message: result ? `Saved ${result.files.length} file${result.files.length === 1 ? "" : "s"}.` : "Download canceled." } }));
    } catch (cause) {
      update(s => ({ ...s, job: { ...idleJob, error: errorMessage(cause) } }));
    }
  }

  async function cancel() {
    setJob({ canceling: true });
    try { await cancelSave(); } catch (cause) { setJob({ canceling: false, error: errorMessage(cause) }); }
  }

  const transcriptMissing = options.content === "transcriptOnly" && video?.assets.transcript === false;
  const defaultName = !video ? "Analyze a link to see the file name"
    : options.naming === "title" ? video.titleFileName ?? video.id : video.id;

  return <div className="flex min-h-full flex-col">
    <div className="grid flex-1 gap-5 pb-4">
      <div>
        <h1 tabIndex={-1} className="text-xl font-semibold tracking-tight">Download a recording</h1>
        <p className="text-sm text-muted-foreground">Save a Loom video, audio track, or transcript.</p>
      </div>

      <RecordingUrlInput value={session.url} busy={session.analyzing} locked={busy} error={session.analyzeError}
        onChange={url => update(s => ({ ...s, url, analyzeError: "" }))} onAnalyze={analyze} />

      {video && <RecordingDetails video={video}
        onOpenInLoom={() => openInLoom(video.sourceUrl).catch(e => setJob({ error: errorMessage(e) }))} />}

      <FileNameInput value={session.fileName} placeholder={defaultName} extension={extensions[options.content]}
        disabled={busy || !video} onChange={fileName => update(s => ({ ...s, fileName }))} />

      <DestinationPicker defaultFolder={settings.folder} override={session.folderOverride} disabled={busy}
        onBrowse={browse} onUseDefault={useDefault} />

      <DownloadOptions options={options} transcriptListed={video ? video.assets.transcript : null}
        captionsListed={video ? video.assets.captions : null} disabled={busy}
        onChange={change => update(s => ({ ...s, overrides: { ...s.overrides, ...change } }))} />

      {changed && !busy && <div>
        <Button variant="link" size="xs" className="h-auto px-0 text-muted-foreground"
          onClick={() => update(s => ({ ...s, overrides: {} }))}>
          <RotateCcw aria-hidden="true" /> Reset options to your defaults
        </Button>
      </div>}
    </div>

    <div className="sticky bottom-0 -mx-4 border-t bg-background/95 px-4 pt-3 pb-4 backdrop-blur sm:-mx-6 sm:px-6">
      <DownloadAction content={options.content} job={job} ready={!!video && !transcriptMissing && !session.analyzing}
        blockedReason={!video ? "Analyze a Loom link to enable downloading."
          : "This recording doesn’t list a transcript. Choose Video or Audio Only."}
        onDownload={download} onCancel={cancel} />
    </div>
  </div>;
}
