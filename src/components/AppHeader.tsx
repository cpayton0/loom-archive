import { ArrowLeft, MonitorPlay, Settings2 } from "lucide-react";
import { Button } from "@/components/ui/button";

export type Page = "download" | "settings";

export function AppHeader({ page, onNavigate }: { page: Page; onNavigate: (page: Page) => void }) {
  return <header className="shrink-0 border-b bg-background">
    <div className="mx-auto flex h-14 max-w-3xl items-center justify-between px-4 sm:px-6">
      <div className="flex items-center gap-2.5 font-semibold tracking-tight">
        <span className="grid size-8 place-items-center rounded-lg bg-primary text-primary-foreground" aria-hidden="true">
          <MonitorPlay className="size-4.5" />
        </span>
        Loom Archive
      </div>
      <nav aria-label="Main">
        {page === "download"
          ? <Button variant="ghost" size="sm" onClick={() => onNavigate("settings")}>
              <Settings2 aria-hidden="true" /> Settings
            </Button>
          : <Button variant="ghost" size="sm" onClick={() => onNavigate("download")}>
              <ArrowLeft aria-hidden="true" /> Back to download
            </Button>}
      </nav>
    </div>
  </header>;
}
