import { useRef } from "react";
import { Download, Loader2, RotateCcw, Square } from "lucide-react";
import {
  analyzeUrl, cancelSave, chooseDownloadFolder, clearDownloadFolder, errorMessage, nextJob, openInLoom, saveOptions,
  saveRecording, type SaveOptions, type Settings, type VideoDescriptor,
} from "@/api/tauri";
import { Button } from "@/components/ui/button";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { parseLinks } from "@/lib/links";
import { BatchPanel, emptyBatch, runBatch, stopBatch, summarize, type Batch } from "./BatchPanel";
import { DestinationPicker } from "./DestinationPicker";
import { DownloadAction, idleJob, type Job } from "./DownloadAction";
import { DownloadOptions } from "./DownloadOptions";
import { FileNameInput } from "./FileNameInput";
import { RecordingDetails } from "./RecordingDetails";
import { RecordingUrlInput } from "./RecordingUrlInput";

/** The in-progress download form. Lives in App so it survives visits to Settings. */
export interface Session {
  mode: "single" | "batch";
  batch: Batch;
  /** Job number of the running single download, for cancel_save. */
  jobId: number | null;
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
  mode: "single", batch: emptyBatch, jobId: null, url: "", video: null, analyzing: false, analyzeError: "", fileName: "", folderOverride: null, overrides: {}, job: idleJob,
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
  const busy = job.saving || session.batch.running;
  const batchMode = session.mode === "batch";
  const updateBatch = (change: (b: Batch) => Batch) => update(s => ({ ...s, batch: change(s.batch) }));
  const batchCount = parseLinks(session.batch.text).filter(l => l.id && !l.duplicate).length;
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
    const id = nextJob();
    update(s => ({ ...s, jobId: id, job: { ...idleJob, saving: true, message: askFolder ? "Choose a folder…" : "Starting…" } }));
    try {
      const result = await saveRecording(id, video.sourceUrl,
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
    try { await cancelSave(session.jobId ?? undefined); } catch (cause) { setJob({ canceling: false, error: errorMessage(cause) }); }
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

      <Tabs value={session.mode} onValueChange={mode => update(s => ({ ...s, mode: mode as Session["mode"] }))}>
        <TabsList className="grid w-full max-w-xs grid-cols-2">
          <TabsTrigger value="single" disabled={busy}>Single link</TabsTrigger>
          <TabsTrigger value="batch" disabled={busy}>Batch</TabsTrigger>
        </TabsList>
      </Tabs>

      {batchMode ? <BatchPanel batch={session.batch} update={updateBatch} /> : <>
      <RecordingUrlInput value={session.url} busy={session.analyzing} locked={busy} error={session.analyzeError}
        onChange={url => update(s => ({ ...s, url, analyzeError: "" }))} onAnalyze={analyze} />

      {video && <RecordingDetails video={video}
        onOpenInLoom={() => openInLoom(video.sourceUrl).catch(e => setJob({ error: errorMessage(e) }))} />}

      <FileNameInput value={session.fileName} placeholder={defaultName} extension={extensions[options.content]}
        disabled={busy || !video} onChange={fileName => update(s => ({ ...s, fileName }))} />
      </>}

      <DestinationPicker defaultFolder={settings.folder} override={session.folderOverride} disabled={busy}
        onBrowse={browse} onUseDefault={useDefault} />

      <DownloadOptions options={options} transcriptListed={!batchMode && video ? video.assets.transcript : null}
        captionsListed={!batchMode && video ? video.assets.captions : null} disabled={busy}
        onChange={change => update(s => ({ ...s, overrides: { ...s.overrides, ...change } }))} />

      {changed && !busy && <div>
        <Button variant="link" size="xs" className="h-auto px-0 text-muted-foreground"
          onClick={() => update(s => ({ ...s, overrides: {} }))}>
          <RotateCcw aria-hidden="true" /> Reset options to your defaults
        </Button>
      </div>}
    </div>

    <div className="sticky bottom-0 -mx-4 border-t bg-background/95 px-4 pt-3 pb-4 backdrop-blur sm:-mx-6 sm:px-6">
      {batchMode ? <div className="grid gap-2">
        {session.batch.running
          ? <div className="flex gap-2">
              <Button size="lg" className="h-11 flex-1" disabled>
                <Loader2 className="animate-spin" aria-hidden="true" /> Downloading batch…</Button>
              <Button size="lg" variant="outline" className="h-11" disabled={session.batch.stopping}
                onClick={() => stopBatch(updateBatch)}>
                <Square aria-hidden="true" /> {session.batch.stopping ? "Stopping…" : "Stop"}</Button>
            </div>
          : <Button size="lg" className="h-11 w-full text-base" disabled={!batchCount}
              onClick={() => runBatch(session.batch, settings, options, session.folderOverride,
                folder => update(s => ({ ...s, folderOverride: folder })), updateBatch)}>
              <Download aria-hidden="true" /> Download {batchCount || ""} recording{batchCount === 1 ? "" : "s"}</Button>}
        <p role="status" aria-live="polite" className="text-center text-xs text-muted-foreground">
          {(session.batch.running && !session.batch.stopping && session.batch.items.length
            ? summarize(session.batch.items) : session.batch.message) || `${settings.concurrency} at a time · ${settings.batchDelaySecs} s pause after each · change in Settings → Advanced`}</p>
      </div> : <DownloadAction content={options.content} job={job} ready={!!video && !transcriptMissing && !session.analyzing}
        blockedReason={!video ? "Analyze a Loom link to enable downloading."
          : "This recording doesn’t list a transcript. Choose Video or Audio Only."}
        onDownload={download} onCancel={cancel} />}
    </div>
  </div>;
}
