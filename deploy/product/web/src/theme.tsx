import { Moon, Sun } from "lucide-react";
import { useSyncExternalStore } from "react";
import { Button } from "@/components/ui/button";

export type Theme = "light" | "dark";

const KEY = "demo-theme";

/**
 * Sets the visitor's theme on <html> before the first paint, as the root route's first script:
 * `dark` if stored, else if the system prefers it.
 */
export const THEME_SCRIPT = `try{var t=localStorage.getItem("${KEY}");if(t!=="light"&&t!=="dark")t=matchMedia("(prefers-color-scheme: dark)").matches?"dark":"light";document.documentElement.classList.toggle("dark",t==="dark")}catch(e){}`;

const listeners = new Set<() => void>();

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function current(): Theme {
  return document.documentElement.classList.contains("dark") ? "dark" : "light";
}

function setTheme(next: Theme): void {
  localStorage.setItem(KEY, next);
  document.documentElement.classList.toggle("dark", next === "dark");
  for (const listener of listeners) {
    listener();
  }
}

/** The visitor's theme, shared by the website's pages: `dark` on <html> (see THEME_SCRIPT). */
export function useTheme(): [Theme, (theme: Theme) => void] {
  // Prerendered pages render light; the browser's theme applies as soon as they hydrate.
  return [useSyncExternalStore(subscribe, current, () => "light"), setTheme];
}

export function ThemeToggle({ theme, onChange }: { theme: Theme; onChange: (theme: Theme) => void }) {
  const next = theme === "dark" ? "light" : "dark";
  return (
    <Button type="button" variant="outline" onClick={() => onChange(next)} aria-label={`Switch to ${next} theme`}>
      {theme === "dark" ? <Sun aria-hidden="true" /> : <Moon aria-hidden="true" />}
      <span className="hidden sm:inline">{theme === "dark" ? "Light" : "Dark"} theme</span>
    </Button>
  );
}
