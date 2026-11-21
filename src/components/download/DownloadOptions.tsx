import type { Content, Quality, SaveOptions } from "@/api/tauri";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Field } from "@/components/Field";

export const formats: { value: Content; label: string }[] = [
  { value: "video", label: "Video" },
  { value: "audioOnly", label: "Audio Only" },
  { value: "transcriptOnly", label: "Transcript Only" },
];

export const qualities: { value: Quality; label: string }[] = [
  { value: "auto", label: "Auto (original)" },
  { value: "best", label: "Best available" },
  { value: "1080p", label: "1080p or lower" },
  { value: "720p", label: "720p or lower" },
  { value: "480p", label: "480p or lower" },
];

export function DownloadOptions({ options, transcriptListed, captionsListed, disabled, onChange }: {
  options: SaveOptions;
  /** null before a recording is analyzed. */
  transcriptListed: boolean | null;
  captionsListed: boolean | null;
  disabled: boolean;
  onChange: (change: Partial<SaveOptions>) => void;
}) {
  const noTranscript = transcriptListed === false;
  const noCaptions = captionsListed === false;
  const transcriptOnly = options.content === "transcriptOnly";
  return <div className="grid gap-4">
    <div className="grid gap-2">
      <p id="format-label" className="text-sm font-semibold">Download format</p>
      <RadioGroup aria-labelledby="format-label" value={options.content} disabled={disabled}
        onValueChange={value => onChange({ content: value as Content })}
        className="flex flex-wrap gap-x-6 gap-y-2">
        {formats.map(f => {
          const unavailable = f.value === "transcriptOnly" && noTranscript;
          return <div key={f.value} className="flex items-center gap-2">
            <RadioGroupItem id={`format-${f.value}`} value={f.value} disabled={unavailable}
              aria-describedby={unavailable ? "format-hint" : undefined} />
            <Label htmlFor={`format-${f.value}`} className="cursor-pointer py-1 text-sm font-normal">{f.label}</Label>
          </div>;
        })}
      </RadioGroup>
      {noTranscript && <p id="format-hint" className="text-xs text-muted-foreground">
        Transcript is unavailable because this recording doesn’t list one.</p>}
    </div>

    {options.content === "video" && <Field id="quality" label="Video quality"
      hint="Auto passes Loom’s playlist to FFmpeg unchanged. Other choices apply to HLS streams.">
      <NativeSelect id="quality" value={options.quality} disabled={disabled}
        onChange={e => onChange({ quality: e.target.value as Quality })} className="h-10">
        {qualities.map(q => <NativeSelectOption key={q.value} value={q.value}>{q.label}</NativeSelectOption>)}
      </NativeSelect>
    </Field>}

    <div className="flex flex-wrap items-center gap-x-6 gap-y-2">
      <div className="flex items-center gap-2">
        <Checkbox id="also-transcript" checked={options.transcript || transcriptOnly}
          disabled={disabled || transcriptOnly || noTranscript}
          onCheckedChange={v => onChange({ transcript: v === true })} />
        <Label htmlFor="also-transcript" className="cursor-pointer py-1 font-normal">
          {transcriptOnly ? "Transcript is the download" : "Also save transcript"}
          {noTranscript && !transcriptOnly && <span className="text-muted-foreground"> (not listed)</span>}
        </Label>
      </div>
      <div className="flex items-center gap-2">
        <Checkbox id="also-captions" checked={options.captions && !noCaptions} disabled={disabled || noCaptions}
          onCheckedChange={v => onChange({ captions: v === true })} />
        <Label htmlFor="also-captions" className="cursor-pointer py-1 font-normal">
          Save captions{noCaptions && <span className="text-muted-foreground"> (not listed)</span>}
        </Label>
      </div>
      <div className="flex items-center gap-2">
        <Checkbox id="also-thumbnail" checked={options.thumbnail} disabled={disabled}
          onCheckedChange={v => onChange({ thumbnail: v === true })} />
        <Label htmlFor="also-thumbnail" className="cursor-pointer py-1 font-normal">Save thumbnail</Label>
      </div>
      <div className="flex items-center gap-2">
        <Checkbox id="also-gif" checked={options.gif} disabled={disabled}
          onCheckedChange={v => onChange({ gif: v === true })} />
        <Label htmlFor="also-gif" className="cursor-pointer py-1 font-normal">Save GIF preview</Label>
      </div>
      <div className="flex items-center gap-2">
        <Checkbox id="also-seek" checked={options.seekPreview} disabled={disabled}
          onCheckedChange={v => onChange({ seekPreview: v === true })} />
        <Label htmlFor="also-seek" className="cursor-pointer py-1 font-normal">Save seek preview</Label>
      </div>
    </div>
  </div>;
}
