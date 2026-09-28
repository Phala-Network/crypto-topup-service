import { HeadContent, Outlet, Scripts, ScriptOnce, createRootRoute } from "@tanstack/react-router";
import type { ReactNode } from "react";
import css from "../index.css?url";
import { THEME_SCRIPT } from "../theme.js";

export const Route = createRootRoute({
  head: () => ({
    meta: [
      { charSet: "utf-8" },
      { name: "viewport", content: "width=device-width, initial-scale=1" },
      { name: "color-scheme", content: "light dark" },
    ],
    links: [
      { rel: "icon", href: "data:," },
      { rel: "stylesheet", href: css },
    ],
  }),
  shellComponent: Document,
  component: Outlet,
});

function Document({ children }: { children: ReactNode }) {
  return (
    // The theme script sets `dark` on <html> before the first paint.
    <html lang="en" suppressHydrationWarning>
      <head>
        <ScriptOnce>{THEME_SCRIPT}</ScriptOnce>
        <HeadContent />
      </head>
      <body>
        {children}
        <Scripts />
      </body>
    </html>
  );
}
