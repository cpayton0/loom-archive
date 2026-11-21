import { useState } from "react";
import { CheckCircle2, ChevronDown, ExternalLink, Video } from "lucide-react";
import type { VideoDescriptor } from "@/api/tauri";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";

export function duration(seconds: number) {
  const m = Math.floor(seconds / 60), s = Math.floor(seconds % 60);
  return m ? `${m} min ${s} sec` : `${s} sec`;
}

export function RecordingDetails({ video, onOpenInLoom }: { video: VideoDescriptor; onOpenInLoom: () => void }) {
  const [open, setOpen] = useState(false);
  const meta = [
    video.durationSeconds !== null && duration(video.durationSeconds),
    video.sourceType === "hls" ? "HLS · Video and audio" : "Direct MP4",
  ].filter(Boolean).join(" · ");
  const assets = [
    ["Transcript", video.assets.transcript], ["Captions", video.assets.captions], ["Thumbnail", video.assets.thumbnail],
  ] as const;
  return <section aria-labelledby="recording-title" className="rounded-lg border bg-card px-4 py-3">
    <div className="flex items-start gap-3">
      <Video className="mt-0.5 size-5 shrink-0 text-muted-foreground" aria-hidden="true" />
      <div className="min-w-0 flex-1">
        <h2 id="recording-title" className="truncate font-semibold" title={video.title ?? undefined}>
          {video.title ?? "Untitled recording"}</h2>
        <p className="text-xs text-muted-foreground">{meta}</p>
        <ul className="mt-2 flex flex-wrap gap-1.5" aria-label="Available assets">
          {assets.map(([name, listed]) => <li key={name}>
            <Badge variant={listed ? "secondary" : "outline"} className={listed ? "" : "text-muted-foreground"}>
              {name}{listed ? "" : " · not listed"}</Badge>
          </li>)}
        </ul>
      </div>
      <CheckCircle2 className="size-5 shrink-0 text-success" aria-label="Ready to download" />
    </div>
    <Collapsible open={open} onOpenChange={setOpen}>
      <div className="mt-2 flex flex-wrap items-center gap-1">
        <CollapsibleTrigger asChild>
          <Button variant="ghost" size="xs" className="text-muted-foreground">
            <ChevronDown className={open ? "rotate-180" : ""} aria-hidden="true" /> Diagnostics
          </Button>
        </CollapsibleTrigger>
        <Button variant="ghost" size="xs" className="text-muted-foreground" onClick={onOpenInLoom}>
          <ExternalLink aria-hidden="true" /> Open in Loom
        </Button>
      </div>
      <CollapsibleContent>
        <dl className="mt-1 grid gap-1 rounded-md bg-muted px-3 py-2 text-xs">
          <div className="flex gap-2"><dt className="w-20 shrink-0 text-muted-foreground">Video ID</dt>
            <dd className="min-w-0 break-all font-mono">{video.id}</dd></div>
          <div className="flex gap-2"><dt className="w-20 shrink-0 text-muted-foreground">Share page</dt>
            <dd className="min-w-0 break-all font-mono">{video.sourceUrl}</dd></div>
          <div className="flex gap-2"><dt className="w-20 shrink-0 text-muted-foreground">Source</dt>
            <dd>{video.sourceType === "hls" ? "HLS playlist, combined by FFmpeg" : "Direct MP4 file"}</dd></div>
        </dl>
      </CollapsibleContent>
    </Collapsible>
  </section>;
}
