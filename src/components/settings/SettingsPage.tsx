import { useEffect, useState } from "react";
import { CheckCircle2, FolderOpen, RotateCcw, TriangleAlert } from "lucide-react";
import {
  appInfo, askEachTime, autoFfmpeg, chooseFfmpeg, chooseFolder, errorMessage, saveOptions, setBatch, setOptions, setResume, setRetries, setTheme,
  type AppInfo, type Collision, type Content, type FileNaming, type Quality, type SaveOptions, type Settings, type Theme,
} from "@/api/tauri";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Separator } from "@/components/ui/separator";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { SettingRow } from "@/components/Field";
import { formats, qualities } from "@/components/download/DownloadOptions";

function Radios<T extends string>({ labelId, name, value, items, onChange }: {
  labelId: string; name: string; value: T; items: { value: T; label: string }[]; onChange: (value: T) => void;
}) {
  return <RadioGroup aria-labelledby={labelId} value={value} onValueChange={v => onChange(v as T)} className="gap-2">
    {items.map(item => <div key={item.value} className="flex items-center gap-2">
      <RadioGroupItem id={`${name}-${item.value}`} value={item.value} />
      <Label htmlFor={`${name}-${item.value}`} className="cursor-pointer py-0.5 font-normal">{item.label}</Label>
    </div>)}
  </RadioGroup>;
}

function PathBox({ path, empty }: { path: string | null; empty: string }) {
  return <p className="truncate rounded-md border bg-card dark:bg-input/30 px-3 py-2 font-mono text-xs" title={path ?? undefined}>
    {path ?? <span className="font-sans text-muted-foreground">{empty}</span>}</p>;
}

const tabClass = "data-[state=active]:border-primary/70 data-[state=active]:bg-primary/10 dark:data-[state=active]:border-primary/70 dark:data-[state=active]:bg-primary/15";

const licenses: [string, string, string][] = [
  ["EcomGraduates/loom-downloader", "MIT", "Download flow this app follows"],
  ["robertoamoreno/loom_com_backup", "MIT", "Reference for recording metadata"],
  ["FFmpeg", "LGPL/GPL", "Run as a separate program to combine streams; not bundled"],
  ["Tauri", "MIT / Apache-2.0", "Desktop runtime"],
  ["React", "MIT", "Interface library"],
  ["shadcn/ui and Radix UI", "MIT", "Interface components"],
  ["Tailwind CSS", "MIT", "Styling"],
  ["Lucide", "ISC", "Icons"],
];

export function SettingsPage({ settings, onSettings }: { settings: Settings; onSettings: (s: Settings) => void }) {
  const [error, setError] = useState("");
  const [info, setInfo] = useState<AppInfo | null>(null);

  useEffect(() => { appInfo().then(setInfo, () => setInfo(null)); }, [settings.ffmpeg]);

  async function apply(change: Promise<Settings>) {
    try { onSettings(await change); setError(""); } catch (cause) { setError(errorMessage(cause)); }
  }
  const set = (change: Partial<SaveOptions>) => {
    const next = { ...saveOptions(settings), ...change };
    onSettings({ ...settings, ...next });
    void apply(setOptions(next));
  };

  return <div className="grid gap-4 pb-6">
    <div>
      <h1 tabIndex={-1} className="text-xl font-semibold tracking-tight">Settings</h1>
      <p className="text-sm text-muted-foreground">Defaults for every download. Changes are saved automatically.</p>
    </div>
    {error && <p role="alert" className="border-l-2 border-destructive pl-3 text-sm text-destructive">{error}</p>}

    <Tabs defaultValue="downloads" className="gap-2">
      <TabsList className="grid w-full grid-cols-4">
        <TabsTrigger value="downloads" className={tabClass}>Downloads</TabsTrigger>
        <TabsTrigger value="advanced" className={tabClass}>Advanced</TabsTrigger>
        <TabsTrigger value="appearance" className={tabClass}>Appearance</TabsTrigger>
        <TabsTrigger value="about" className={tabClass}>About</TabsTrigger>
      </TabsList>

      <TabsContent value="downloads" className="divide-y">
        <SettingRow title="Default download folder" description="Used unless you browse to another folder for a single download.">
          <div className="grid gap-2">
            <PathBox path={settings.folder} empty="Ask when downloading" />
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" size="sm" onClick={() => apply(chooseFolder())}>
                <FolderOpen aria-hidden="true" /> {settings.folder ? "Change…" : "Choose…"}</Button>
              {settings.folder && <Button variant="ghost" size="sm" onClick={() => apply(askEachTime())}>Ask each time</Button>}
            </div>
          </div>
        </SettingRow>
        <SettingRow title="File naming" labelId="naming-label" description="Used when the file name field is left blank.">
          <Radios<FileNaming> labelId="naming-label" name="naming" value={settings.naming} onChange={naming => set({ naming })}
            items={[{ value: "title", label: "Recording title" }, { value: "id", label: "Video ID" }]} />
        </SettingRow>
        <SettingRow title="Default format" labelId="content-label">
          <Radios<Content> labelId="content-label" name="content" value={settings.content}
            onChange={content => set({ content })} items={formats} />
        </SettingRow>
        <SettingRow title="Default video quality" description="Auto matches Loom’s player exactly.">
          <NativeSelect aria-label="Default video quality" value={settings.quality} className="w-full"
            onChange={e => set({ quality: e.target.value as Quality })}>
            {qualities.map(q => <NativeSelectOption key={q.value} value={q.value}>{q.label}</NativeSelectOption>)}
          </NativeSelect>
        </SettingRow>
        <SettingRow title="Also save by default">
          <div className="grid gap-3">
            <div className="flex items-center gap-2">
              <Switch id="default-transcript" checked={settings.transcript} onCheckedChange={transcript => set({ transcript })} />
              <Label htmlFor="default-transcript" className="font-normal">Transcript (.transcript.json)</Label>
            </div>
            <div className="flex items-center gap-2">
              <Switch id="default-captions" checked={settings.captions} onCheckedChange={captions => set({ captions })} />
              <Label htmlFor="default-captions" className="font-normal">Captions (.captions.vtt)</Label>
            </div>
            <div className="flex items-center gap-2">
              <Switch id="default-thumbnail" checked={settings.thumbnail} onCheckedChange={thumbnail => set({ thumbnail })} />
              <Label htmlFor="default-thumbnail" className="font-normal">Thumbnail (-thumbnail.jpg)</Label>
            </div>
            <div className="flex items-center gap-2">
              <Switch id="default-gif" checked={settings.gif} onCheckedChange={gif => set({ gif })} />
              <Label htmlFor="default-gif" className="font-normal">GIF preview (-thumbnail.gif)</Label>
            </div>
            <div className="flex items-center gap-2">
              <Switch id="default-seek" checked={settings.seekPreview} onCheckedChange={seekPreview => set({ seekPreview })} />
              <Label htmlFor="default-seek" className="font-normal">Seek preview (-seek-preview.jpg + .vtt)</Label>
            </div>
          </div>
        </SettingRow>
        <SettingRow title="If a file already exists" labelId="collision-label" description="Existing files are never overwritten.">
          <Radios<Collision> labelId="collision-label" name="collision" value={settings.collision}
            onChange={collision => set({ collision })}
            items={[{ value: "stop", label: "Stop and tell me" }, { value: "rename", label: "Add a number, e.g. “name (2)”" }]} />
        </SettingRow>
      </TabsContent>

      <TabsContent value="advanced" className="divide-y">
        <SettingRow title="FFmpeg" description="Needed for HLS streams and audio-only downloads.">
          <div className="grid gap-2">
            <p className="flex items-center gap-1.5 text-xs">
              {info?.ffmpeg ? <><CheckCircle2 className="size-3.5 text-success" aria-hidden="true" /> Found</>
                : <><TriangleAlert className="size-3.5 text-destructive" aria-hidden="true" /> Not found</>}
              <span className="text-muted-foreground">· {settings.ffmpeg ? "Custom location" : "Automatic (beside the app, then PATH)"}</span>
            </p>
            <PathBox path={info?.ffmpeg ?? null} empty="Install FFmpeg or choose ffmpeg.exe" />
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" size="sm" onClick={() => apply(chooseFfmpeg())}>Choose ffmpeg.exe…</Button>
              {settings.ffmpeg && <Button variant="ghost" size="sm" onClick={() => apply(autoFfmpeg())}>
                <RotateCcw aria-hidden="true" /> Use automatic</Button>}
            </div>
          </div>
        </SettingRow>
        <SettingRow title="Resume interrupted downloads" description="Keeps a partial direct-MP4 download and continues it on the next attempt, like upstream’s --resume. HLS downloads always restart.">
          <div className="flex items-center gap-2">
            <Switch id="resume" checked={settings.resume} onCheckedChange={resume => void apply(setResume(resume))} />
            <Label htmlFor="resume" className="font-normal">{settings.resume ? "On" : "Off"}</Label>
          </div>
        </SettingRow>
        <SettingRow title="Retry interrupted downloads" description="Extra attempts when the connection drops during a transfer.">
          <NativeSelect aria-label="Retry interrupted downloads" value={String(settings.retries)} className="w-full"
            onChange={e => void apply(setRetries(Number(e.target.value)))}>
            {[0, 1, 2, 3].map(n => <NativeSelectOption key={n} value={n}>{n === 0 ? "Don’t retry" : `${n} time${n > 1 ? "s" : ""}`}</NativeSelectOption>)}
          </NativeSelect>
        </SettingRow>
        <SettingRow title="Batch downloads at once" description="How many recordings a batch downloads in parallel. Upstream uses 5.">
          <NativeSelect aria-label="Batch downloads at once" value={String(settings.concurrency)} className="w-full"
            onChange={e => void apply(setBatch(Number(e.target.value), settings.batchDelaySecs, settings.skipDownloaded))}>
            {[1, 2, 3, 4, 5].map(n => <NativeSelectOption key={n} value={n}>{n}</NativeSelectOption>)}
          </NativeSelect>
        </SettingRow>
        <SettingRow title="Pause between batch downloads" description="Waits after each finished download, like upstream’s --timeout.">
          <NativeSelect aria-label="Pause between batch downloads" value={String(settings.batchDelaySecs)} className="w-full"
            onChange={e => void apply(setBatch(settings.concurrency, Number(e.target.value), settings.skipDownloaded))}>
            {[0, 2, 5, 10, 30, 60].map(n => <NativeSelectOption key={n} value={n}>{n === 0 ? "No pause" : `${n} seconds`}</NativeSelectOption>)}
          </NativeSelect>
        </SettingRow>
        <SettingRow title="Skip recordings already downloaded" description="Batches skip links saved before, like upstream’s downloaded.log.">
          <div className="flex items-center gap-2">
            <Switch id="skip-downloaded" checked={settings.skipDownloaded}
              onCheckedChange={skip => void apply(setBatch(settings.concurrency, settings.batchDelaySecs, skip))} />
            <Label htmlFor="skip-downloaded" className="font-normal">{settings.skipDownloaded ? "On" : "Off"}</Label>
          </div>
        </SettingRow>
        <SettingRow title="Diagnostic logging" description="Not available yet. Download errors are shown in the app.">
          <div className="flex items-center gap-2">
            <Switch id="logging" checked={false} disabled aria-describedby="logging-note" />
            <Label htmlFor="logging" className="font-normal text-muted-foreground">Off</Label>
            <span id="logging-note" className="sr-only">Diagnostic logging is not available in this version.</span>
          </div>
        </SettingRow>
      </TabsContent>

      <TabsContent value="appearance" className="divide-y">
        <SettingRow title="Theme" labelId="theme-label" description="Match Windows follows your Windows app mode.">
          <Radios<Theme> labelId="theme-label" name="theme" value={settings.theme}
            onChange={theme => { onSettings({ ...settings, theme }); void apply(setTheme(theme)); }}
            items={[{ value: "dark", label: "Dark" }, { value: "light", label: "Light" }, { value: "system", label: "Match Windows" }]} />
        </SettingRow>
      </TabsContent>

      <TabsContent value="about" className="grid gap-4 pt-2">
        <div>
          <p className="font-semibold">Loom Archive</p>
          <p className="text-sm text-muted-foreground">Version {info?.version ?? "…"}</p>
        </div>
        <Separator />
        <div className="grid gap-2">
          <p className="text-sm font-semibold">Third-party components</p>
          <ul className="grid gap-2">
            {licenses.map(([name, license, use]) => <li key={name} className="flex flex-wrap items-baseline justify-between gap-x-4 text-sm">
              <span className="min-w-0"><span className="font-medium">{name}</span>
                <span className="block text-xs text-muted-foreground">{use}</span></span>
              <span className="shrink-0 text-xs text-muted-foreground">{license}</span>
            </li>)}
          </ul>
          <p className="text-xs text-muted-foreground">License texts for the Loom projects are in the LICENSES folder of the source repository.</p>
        </div>
      </TabsContent>
    </Tabs>
  </div>;
}
