import { Moon, Sun } from "lucide-react";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";

export type Theme = "light" | "dark";

/** The visitor's theme, shared by the website's pages: stored, else the system's; `dark` on `<html>`. */
export function useTheme(): [Theme, (theme: Theme) => void] {
  const [theme, setTheme] = useState<Theme>(() => {
    const stored = localStorage.getItem("demo-theme");
    if (stored === "light" || stored === "dark") {
      return stored;
    }
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  });
  useEffect(() => {
    document.documentElement.classList.toggle("dark", theme === "dark");
  }, [theme]);
  return [
    theme,
    (next) => {
      localStorage.setItem("demo-theme", next);
      setTheme(next);
    },
  ];
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
