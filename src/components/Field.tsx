import type { ReactNode } from "react";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";

/** A labeled control with optional supporting text. */
export function Field({ id, label, extra, hint, hintId, className, children }: {
  id?: string; label: ReactNode; extra?: ReactNode; hint?: ReactNode; hintId?: string;
  className?: string; children: ReactNode;
}) {
  return <div className={cn("grid gap-1.5", className)}>
    <Label id={id ? `${id}-label` : undefined} htmlFor={id} className="text-sm font-semibold">
      {label}{extra && <span className="font-normal text-muted-foreground">{extra}</span>}
    </Label>
    {children}
    {hint && <p id={hintId} className="text-xs text-muted-foreground">{hint}</p>}
  </div>;
}

/** A settings row: title and description beside the control, stacked when narrow. */
export function SettingRow({ title, description, labelId, children }: {
  title: string; description?: ReactNode; labelId?: string; children: ReactNode;
}) {
  return <div className="flex flex-col gap-3 py-4 sm:flex-row sm:items-start sm:justify-between sm:gap-6">
    <div className="min-w-0 sm:max-w-[48%]">
      <p id={labelId} className="text-sm font-semibold">{title}</p>
      {description && <p className="mt-0.5 text-xs text-muted-foreground">{description}</p>}
    </div>
    <div className="min-w-0 sm:flex-1 sm:max-w-sm">{children}</div>
  </div>;
}
