import { Folder, FolderOpen, RotateCcw } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

function split(path: string) {
  const at = Math.max(path.lastIndexOf("\\"), path.lastIndexOf("/"));
  return at > 0 ? { name: path.slice(at + 1) || path, parent: path.slice(0, at) } : { name: path, parent: "" };
}

export function DestinationPicker({ defaultFolder, override, disabled, onBrowse, onUseDefault }: {
  defaultFolder: string | null; override: string | null; disabled: boolean;
  onBrowse: () => void; onUseDefault: () => void;
}) {
  const path = override ?? defaultFolder;
  const parts = path ? split(path) : null;
  return <div className="grid gap-1.5">
    <p id="save-to-label" className="text-sm font-semibold">Save to</p>
    <div className="flex flex-col gap-2 sm:flex-row">
      <Tooltip>
        <TooltipTrigger asChild>
          <div tabIndex={path ? 0 : -1} aria-labelledby="save-to-label" aria-describedby="save-to-path"
            className="flex h-10 min-w-0 flex-1 items-center gap-2 rounded-md border bg-card dark:bg-input/30 px-3 text-sm outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50">
            <Folder className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
            <span id="save-to-path" className="flex min-w-0 flex-1 items-baseline gap-1.5">
              {parts ? <>
                <span className="truncate font-medium">{parts.name}</span>
                {parts.parent && <span className="min-w-0 truncate text-xs text-muted-foreground">{parts.parent}</span>}
                <span className="sr-only">Full path: {path}</span>
              </> : <span className="text-muted-foreground">Ask when downloading</span>}
            </span>
            <Badge variant={override ? "default" : "secondary"} className="shrink-0">
              {override ? "This download" : "Default"}</Badge>
          </div>
        </TooltipTrigger>
        {path && <TooltipContent side="bottom" className="max-w-[min(32rem,90vw)] break-all font-mono">{path}</TooltipContent>}
      </Tooltip>
      <div className="flex gap-2">
        <Button variant="outline" className="h-10 flex-1 sm:flex-none" disabled={disabled} onClick={onBrowse}>
          <FolderOpen aria-hidden="true" /> Browse
        </Button>
        {override && <Button variant="ghost" className="h-10" disabled={disabled} onClick={onUseDefault}
          aria-label="Use the default folder">
          <RotateCcw aria-hidden="true" /> Default
        </Button>}
      </div>
    </div>
    <p className="text-xs text-muted-foreground">
      {override ? "Used for this download only. Your default folder is unchanged."
        : "Browse to save this download somewhere else. Change the default in Settings."}</p>
  </div>;
}
