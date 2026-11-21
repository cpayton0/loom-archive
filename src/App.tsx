import { useEffect, useRef, useState } from "react";
import { Loader2 } from "lucide-react";
import { errorMessage, getSettings, type Settings } from "@/api/tauri";
import { useTheme } from "@/lib/theme";
import { AppHeader, type Page } from "@/components/AppHeader";
import { DownloadPage, emptySession, type Session } from "@/components/download/DownloadPage";
import { SettingsPage } from "@/components/settings/SettingsPage";

export default function App() {
  const [page, setPage] = useState<Page>("download");
  const [settings, setSettings] = useState<Settings | null>(null);
  const [loadError, setLoadError] = useState("");
  // Kept here so an in-progress form or download survives a visit to Settings.
  const [session, setSession] = useState<Session>(emptySession);
  const scroller = useRef<HTMLElement>(null);
  useTheme(settings?.theme ?? "dark");

  useEffect(() => { getSettings().then(setSettings, e => setLoadError(errorMessage(e))); }, []);

  function navigate(next: Page) {
    setPage(next);
    scroller.current?.scrollTo({ top: 0 });
    requestAnimationFrame(() => document.querySelector<HTMLElement>("main h1")?.focus());
  }

  return <div className="flex h-full flex-col overflow-hidden">
    <AppHeader page={page} onNavigate={navigate} />
    <main ref={scroller} className="min-h-0 flex-1 overflow-x-hidden overflow-y-auto [scrollbar-gutter:stable]">
      <div className="mx-auto h-full max-w-3xl px-4 pt-5 sm:px-6 [&_h1]:outline-none" >
        {loadError ? <p role="alert" className="text-sm text-destructive">{loadError}</p>
          : !settings ? <p className="flex items-center gap-2 text-sm text-muted-foreground" role="status">
              <Loader2 className="size-4 animate-spin" aria-hidden="true" /> Loading settings…</p>
          : page === "download"
            ? <DownloadPage settings={settings} session={session} update={setSession} />
            : <SettingsPage settings={settings} onSettings={setSettings} />}
      </div>
    </main>
  </div>;
}
