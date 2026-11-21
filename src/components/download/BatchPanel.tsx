import { CheckCircle2, CircleDashed, CircleSlash, FileText, Loader2, XCircle } from "lucide-react";
import {
  cancelSave, chooseDownloadFolder, downloaded, errorMessage, nextJob, openLinkList, saveRecording,
  type SaveOptions, type Settings, type TransferProgress,
} from "@/api/tauri";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Progress } from "@/components/ui/progress";
import { Field } from "@/components/Field";
import { parseLinks } from "@/lib/links";

export type ItemStatus = "queued" | "running" | "saved" | "failed" | "canceled" | "skipped" | "invalid";

export interface BatchItem {
  job: number;
  url: string;
  /** Upstream-style position for `<prefix>-<n>-` numbering; null for invalid lines. */
  index: number | null;
  status: ItemStatus;
  progress: TransferProgress | null;
  message: string;
}

export interface Batch {
  text: string;
  prefix: string;
  items: BatchItem[];
  running: boolean;
  stopping: boolean;
  message: string;
}

export const emptyBatch: Batch = { text: "", prefix: "", items: [], running: false, stopping: false, message: "" };

type UpdateBatch = (change: (b: Batch) => Batch) => void;

// Survives the panel unmounting while the user visits Settings.
const control = { stop: false };

const wait = (ms: number) => new Promise(r => setTimeout(r, ms));

/** Waits between downloads but returns early when the batch is stopped. */
async function pause(seconds: number) {
  for (let left = seconds * 10; left > 0 && !control.stop; left--) await wait(100);
}

export function summarize(items: BatchItem[]) {
  const count = (s: ItemStatus) => items.filter(i => i.status === s).length;
  const invalid = count("invalid");
  return [
    [count("running"), "downloading"], [count("queued"), "waiting"],
    [count("saved"), "saved"], [count("failed"), "failed"], [count("canceled"), "canceled"],
    [count("skipped"), "skipped"], [invalid, invalid === 1 ? "line isn’t a Loom link" : "lines aren’t Loom links"],
  ].filter(([n]) => n).map(([n, label]) => `${n} ${label}`).join(" · ");
}

/** Runs the queue: skip known recordings, pick a folder once, then download with a worker pool. */
export async function runBatch(batch: Batch, settings: Settings, options: SaveOptions,
  folderOverride: string | null, onFolder: (folder: string) => void, update: UpdateBatch) {
  control.stop = false;
  const listed = parseLinks(batch.text);
  let position = 0;
  const items: BatchItem[] = listed.map(link => ({
    job: nextJob(), url: link.url, progress: null, message: "",
    index: link.id && !link.duplicate ? ++position : null,
    status: !link.id ? "invalid" : link.duplicate ? "skipped" : "queued",
  }));
  items.forEach((item, i) => {
    if (listed[i].duplicate) item.message = "Listed twice";
    if (!listed[i].id) item.message = "Not a Loom share link";
  });
  const setItem = (job: number, change: Partial<BatchItem>) =>
    update(b => ({ ...b, items: b.items.map(i => i.job === job ? { ...i, ...change } : i) }));
  update(b => ({ ...b, items, running: true, stopping: false, message: "Preparing…" }));

  try {
    const queued = items.filter(i => i.status === "queued");
    if (settings.skipDownloaded && queued.length) {
      const known = await downloaded(queued.map(i => i.url));
      queued.forEach((item, i) => {
        if (known[i]) { item.status = "skipped"; item.message = "Already downloaded"; }
      });
      update(b => ({ ...b, items: [...items] }));
    }
    const work = items.filter(i => i.status === "queued");
    if (!work.length) {
      update(b => ({ ...b, running: false, message: `Nothing to download. ${summarize(items)}` }));
      return;
    }
    if (!folderOverride && !settings.folder) {
      const folder = await chooseDownloadFolder();
      if (!folder) {
        update(b => ({ ...b, running: false, message: "Choose a folder to start the batch." }));
        return;
      }
      onFolder(folder);
    }

    let next = 0;
    const worker = async () => {
      while (!control.stop && next < work.length) {
        const item = work[next++];
        setItem(item.job, { status: "running", message: "Starting…" });
        try {
          const result = await saveRecording(item.job, item.url,
            { options, fileName: null, prefix: batch.prefix.trim() || null, index: item.index },
            progress => setItem(item.job, { progress, message: progress.finalizing ? "Finishing…" : "Downloading…" }));
          item.status = result ? "saved" : "canceled";
          setItem(item.job, { status: item.status, progress: null,
            message: result ? [`${result.files.length} file${result.files.length === 1 ? "" : "s"}`, ...result.notes].join(" · ") : "Canceled" });
        } catch (cause) {
          item.status = "failed";
          setItem(item.job, { status: "failed", progress: null, message: errorMessage(cause) });
        }
        // Upstream pauses after each finished download before taking the next.
        if (next < work.length && settings.batchDelaySecs > 0) await pause(settings.batchDelaySecs);
      }
    };
    await Promise.all(Array.from({ length: Math.min(settings.concurrency, work.length) }, worker));
    if (control.stop) {
      work.filter(i => i.status === "queued").forEach(i => { i.status = "canceled"; i.message = "Canceled"; });
    }
  } catch (cause) {
    update(b => ({ ...b, message: errorMessage(cause) }));
  }
  update(b => ({
    ...b, running: false, stopping: false,
    items: b.items.map(i => i.status === "queued" && control.stop ? { ...i, status: "canceled", message: "Canceled" } : i),
    message: summarize(items) || "Done.",
  }));
}

export async function stopBatch(update: UpdateBatch) {
  control.stop = true;
  update(b => ({ ...b, stopping: true, message: "Stopping…" }));
  await cancelSave().catch(() => {});
}

const icons: Record<ItemStatus, React.ReactNode> = {
  queued: <CircleDashed className="size-4 text-muted-foreground" aria-hidden="true" />,
  running: <Loader2 className="size-4 animate-spin text-primary" aria-hidden="true" />,
  saved: <CheckCircle2 className="size-4 text-success" aria-hidden="true" />,
  failed: <XCircle className="size-4 text-destructive" aria-hidden="true" />,
  canceled: <CircleSlash className="size-4 text-muted-foreground" aria-hidden="true" />,
  skipped: <CircleSlash className="size-4 text-muted-foreground" aria-hidden="true" />,
  invalid: <XCircle className="size-4 text-muted-foreground" aria-hidden="true" />,
};

export function BatchPanel({ batch, update }: { batch: Batch; update: UpdateBatch }) {
  const links = parseLinks(batch.text);
  const valid = links.filter(l => l.id && !l.duplicate).length;
  const invalid = links.filter(l => !l.id).length;
  const duplicates = links.filter(l => l.duplicate).length;
  const locked = batch.running;

  async function openList() {
    try {
      const lines = await openLinkList();
      if (lines) update(b => ({ ...b, text: [b.text.trim(), ...lines].filter(Boolean).join("\n") }));
    } catch (cause) { update(b => ({ ...b, message: errorMessage(cause) })); }
  }

  return <div className="grid gap-5">
    <div className="grid gap-1.5">
      <div className="flex items-end justify-between gap-2">
        <Label htmlFor="batch-links" className="text-sm font-semibold">Loom links</Label>
        <Button variant="outline" size="sm" onClick={openList} disabled={locked}>
          <FileText aria-hidden="true" /> Open list…</Button>
      </div>
      <textarea id="batch-links" value={batch.text} readOnly={locked} spellCheck={false} rows={6}
        placeholder={"One share link per line\nhttps://www.loom.com/share/…"} aria-describedby="batch-links-hint"
        onChange={e => update(b => ({ ...b, text: e.target.value }))}
        className="min-h-28 w-full resize-y rounded-md border border-input bg-card px-3 py-2 font-mono text-xs outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 dark:bg-input/30" />
      <p id="batch-links-hint" className="text-xs text-muted-foreground">
        {links.length ? `${valid} recording${valid === 1 ? "" : "s"}` : "Paste links or open a .txt list. Lines starting with # are ignored."}
        {invalid > 0 && ` · ${invalid} ${invalid === 1 ? "line isn’t a Loom link" : "lines aren’t Loom links"}`}{duplicates > 0 && ` · ${duplicates} duplicate${duplicates === 1 ? "" : "s"}`}
      </p>
    </div>

    <Field id="batch-prefix" label="Numbering prefix" extra=" (optional)" hintId="batch-prefix-hint"
      hint={batch.prefix.trim() ? `Files are named ${batch.prefix.trim()}-1-<name>, ${batch.prefix.trim()}-2-<name>, … in list order.`
        : "Adds a prefix and the list position to each file name, like upstream’s --prefix."}>
      <Input id="batch-prefix" value={batch.prefix} readOnly={locked} maxLength={60} autoComplete="off"
        placeholder="e.g. onboarding" aria-describedby="batch-prefix-hint"
        onChange={e => update(b => ({ ...b, prefix: e.target.value }))} className="h-10" />
    </Field>

    {batch.items.length > 0 && <section aria-labelledby="queue-title" className="grid gap-2">
      <h2 id="queue-title" className="text-sm font-semibold">Queue</h2>
      <ol className="grid divide-y rounded-lg border bg-card">
        {batch.items.map(item => <li key={item.job} className="grid gap-1 px-3 py-2">
          <div className="flex items-center gap-2">
            {icons[item.status]}
            <span className="min-w-0 flex-1 truncate font-mono text-xs" title={item.url}>
              {item.index !== null && <span className="text-muted-foreground">{item.index}. </span>}{item.url}</span>
            <span className="sr-only">{item.status}</span>
          </div>
          {item.message && <p className={`pl-6 text-xs ${item.status === "failed" ? "text-destructive" : "text-muted-foreground"}`}>
            {item.message}</p>}
          {item.status === "running" && item.progress && !item.progress.finalizing && <div className="pl-6">
            <Progress value={item.progress.percent ?? (item.progress.total ? item.progress.bytes / item.progress.total * 100 : 0)}
              aria-label={`Progress for item ${item.index}`} className="h-1.5" /></div>}
        </li>)}
      </ol>
    </section>}
  </div>;
}
