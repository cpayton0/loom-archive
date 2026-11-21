import { Download, FileCheck2, Loader2, X } from "lucide-react";
import type { Content, SaveResult, TransferProgress } from "@/api/tauri";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";

export interface Job {
  saving: boolean;
  canceling: boolean;
  progress: TransferProgress | null;
  message: string;
  result: SaveResult | null;
  error: string;
}

export const idleJob: Job = { saving: false, canceling: false, progress: null, message: "", result: null, error: "" };

const labels: Record<Content, string> = {
  video: "Download MP4", audioOnly: "Download audio (M4A)", transcriptOnly: "Download transcript",
};

function size(n: number) { return (n / 1048576).toLocaleString(undefined, { maximumFractionDigits: 1 }) + " MiB"; }

export function DownloadProgress({ job }: { job: Job }) {
  const p = job.progress;
  // FFmpeg jobs report progress against the recording length; direct files against their size.
  const percent = p?.percent ?? (p?.total ? (p.bytes / p.total) * 100 : null);
  return <div className="grid gap-1.5">
    {p && !p.finalizing && <>
      {percent !== null ? <Progress value={percent} aria-label="Download progress" aria-valuetext={`${Math.floor(percent)}%`} />
        : <div className="h-2 overflow-hidden rounded-full bg-primary/20" role="progressbar" aria-label="Download progress">
            <div className="h-full w-1/3 animate-pulse rounded-full bg-primary" /></div>}
      <p className="text-xs text-muted-foreground">
        {percent !== null && `${Math.floor(percent)}% · `}
        {size(p.bytes)}{p.total ? ` of ${size(p.total)}` : " received"}</p>
    </>}
  </div>;
}

export function DownloadAction({ content, job, ready, blockedReason, onDownload, onCancel }: {
  content: Content; job: Job; ready: boolean; blockedReason: string;
  onDownload: () => void; onCancel: () => void;
}) {
  return <div className="grid gap-2">
    {job.saving
      ? <div className="flex gap-2">
          <Button size="lg" className="h-11 flex-1" disabled>
            <Loader2 className="animate-spin" aria-hidden="true" /> {job.message || "Downloading…"}
          </Button>
          <Button size="lg" variant="outline" className="h-11" disabled={job.canceling} onClick={onCancel}>
            <X aria-hidden="true" /> {job.canceling ? "Canceling…" : "Cancel"}
          </Button>
        </div>
      : <Button size="lg" className="h-11 w-full text-base" disabled={!ready} onClick={onDownload}
          aria-describedby={!ready ? "download-blocked" : undefined}>
          <Download aria-hidden="true" /> {labels[content]}
        </Button>}
    {!ready && !job.saving && <p id="download-blocked" className="text-center text-xs text-muted-foreground">{blockedReason}</p>}
    {job.saving && <DownloadProgress job={job} />}
    <div role="status" aria-live="polite" className="grid gap-1">
      {!job.saving && job.message && <p className="flex items-center gap-1.5 text-sm">
        {job.result && <FileCheck2 className="size-4 text-success" aria-hidden="true" />}{job.message}</p>}
      {job.result && job.result.files.length > 0 && <ul className="grid gap-0.5" aria-label="Saved files">
        {job.result.files.map(f => <li key={f} className="truncate font-mono text-xs text-muted-foreground" title={f}>{f}</li>)}
      </ul>}
      {job.result?.notes.map(n => <p key={n} className="text-xs text-muted-foreground">{n}</p>)}
    </div>
    {job.error && <p role="alert" className="border-l-2 border-destructive pl-3 text-sm text-destructive">{job.error}</p>}
  </div>;
}
