import { useEffect } from "react";
import type { Theme } from "@/api/tauri";

const query = "(prefers-color-scheme: dark)";

function resolve(theme: Theme) {
  return theme === "system" ? (window.matchMedia(query).matches ? "dark" : "light") : theme;
}

/** Puts `dark` or `light` on <html> and follows Windows while set to "system". */
export function useTheme(theme: Theme) {
  useEffect(() => {
    const root = document.documentElement;
    const apply = () => {
      const mode = resolve(theme);
      root.classList.toggle("dark", mode === "dark");
      root.classList.toggle("light", mode === "light");
    };
    apply();
    if (theme !== "system") return;
    const media = window.matchMedia(query);
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [theme]);
}
