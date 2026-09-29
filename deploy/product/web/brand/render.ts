// Renders the site's static images into public/ from the SVGs committed beside this script, in the
// pinned Playwright's Chromium, with Geist from the page's own bundled typeface: `pnpm run brand`,
// then commit the PNGs. The page links them from index.html; public/_headers sets their caching.
import { chromium } from "@playwright/test";
import { readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { resolve } from "node:path";

const web = resolve(import.meta.dirname, "..");
const font = readFileSync(
  createRequire(import.meta.url).resolve("@fontsource-variable/geist/files/geist-latin-wght-normal.woff2"),
).toString("base64");

const IMAGES: { source: string; output: string; size: [number, number] }[] = [
  { source: "brand/og-image.svg", output: "public/og-image.png", size: [1200, 630] },
  { source: "public/favicon.svg", output: "public/favicon-32.png", size: [32, 32] },
  { source: "brand/app-icon.svg", output: "public/apple-touch-icon.png", size: [180, 180] },
  { source: "brand/app-icon.svg", output: "public/icon-192.png", size: [192, 192] },
  { source: "brand/app-icon.svg", output: "public/icon-512.png", size: [512, 512] },
];

const browser = await chromium.launch();
try {
  for (const { source, output, size } of IMAGES) {
    const [width, height] = size;
    const page = await browser.newPage({ viewport: { width, height }, deviceScaleFactor: 1 });
    const svg = readFileSync(resolve(web, source), "utf8");
    await page.setContent(
      `<!doctype html><style>
        @font-face { font-family: "Geist Variable"; font-weight: 100 900; src: url(data:font/woff2;base64,${font}) format("woff2"); }
        html, body { margin: 0; background: transparent; }
        svg { display: block; width: ${width}px; height: ${height}px; }
      </style>${svg}`,
    );
    await page.evaluate(async () => {
      await document.fonts.load('600 16px "Geist Variable"');
      await document.fonts.ready;
    });
    writeFileSync(resolve(web, output), await page.screenshot({ omitBackground: true }));
    await page.close();
    console.log(`${output} (${width}×${height})`);
  }
} finally {
  await browser.close();
}
