import { Input } from "@/components/ui/input";
import { Field } from "@/components/Field";

export function FileNameInput({ value, placeholder, extension, disabled, onChange }: {
  value: string; placeholder: string; extension: string; disabled: boolean; onChange: (value: string) => void;
}) {
  return <Field id="file-name" label="File name" extra=" (optional override)" hintId="file-name-hint"
    hint="Leave blank to use the default name. Unsupported characters are replaced when saving.">
    <div className="flex min-w-0 items-stretch rounded-md border border-input bg-card dark:bg-input/30 focus-within:border-ring focus-within:ring-[3px] focus-within:ring-ring/50">
      <Input id="file-name" value={value} placeholder={placeholder} disabled={disabled} maxLength={200}
        autoComplete="off" spellCheck={false} aria-describedby="file-name-hint"
        onChange={e => onChange(e.target.value)}
        className="h-10 min-w-0 flex-1 border-0 bg-transparent shadow-none focus-visible:ring-0 dark:bg-transparent" />
      <span className="flex shrink-0 items-center border-l px-3 font-mono text-xs text-muted-foreground" aria-label={`Extension ${extension}`}>
        {extension}</span>
    </div>
  </Field>;
}
