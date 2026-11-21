import type { ClipboardEvent, FormEvent } from "react";
import { Loader2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Field } from "@/components/Field";

// Share or embed links, optionally with a title slug; Rust does the authoritative check.
const shareLink = /^https:\/\/(www\.)?loom\.com\/(share|embed)\/([\w.%-]+-)?[0-9a-f]{32}\/?([?#]\S*)?$/i;

export function RecordingUrlInput({ value, busy, locked, error, onChange, onAnalyze }: {
  value: string; busy: boolean; locked: boolean; error: string;
  onChange: (value: string) => void; onAnalyze: (url?: string) => void;
}) {
  function submit(event: FormEvent) {
    event.preventDefault();
    if (!busy && !locked && value.trim()) onAnalyze();
  }
  // Pasting a complete share link analyzes it immediately; Analyze stays for typed links.
  function paste(event: ClipboardEvent<HTMLInputElement>) {
    if (locked) return;
    const input = event.currentTarget;
    const pasted = event.clipboardData.getData("text");
    const next = value.slice(0, input.selectionStart ?? value.length) + pasted
      + value.slice(input.selectionEnd ?? value.length);
    // Shells escape `?` and `=` with backslashes; Rust strips them too.
    const link = next.trim().replace(/\\/g, "");
    if (shareLink.test(link)) {
      event.preventDefault();
      onAnalyze(link);
    }
  }
  return <form onSubmit={submit} noValidate>
    <Field id="share-url" label="Loom URL" hint={error ? <span className="text-destructive">{error}</span>
      : "Paste a share link to analyze it automatically."}
      hintId="share-url-error">
      <div className="flex gap-2">
        <Input id="share-url" type="text" inputMode="url" autoComplete="off" spellCheck={false} maxLength={8192}
          placeholder="https://www.loom.com/share/…" value={value} readOnly={locked}
          aria-invalid={error ? true : undefined} aria-describedby="share-url-error"
          onChange={e => onChange(e.target.value)} onPaste={paste} className="h-10 min-w-0 flex-1" />
        <Button type="submit" variant="secondary" className="h-10" disabled={busy || locked || !value.trim()}>
          {busy && <Loader2 className="animate-spin" aria-hidden="true" />}
          {busy ? "Analyzing…" : "Analyze"}
        </Button>
      </div>
    </Field>
    {error && <span className="sr-only" role="alert">{error}</span>}
  </form>;
}
