import type { CSSProperties } from "react";

/** Theming in the spirit of Stripe's Appearance API: a base theme plus a few variables. */
export interface Appearance {
  theme?: "light" | "dark";
  variables?: {
    colorPrimary?: string;
    colorBackground?: string;
    colorText?: string;
    colorTextSecondary?: string;
    colorBorder?: string;
    colorDanger?: string;
    colorSuccess?: string;
    fontFamily?: string;
    borderRadius?: string;
  };
}

const VARIABLES: Record<keyof NonNullable<Appearance["variables"]>, string> = {
  colorPrimary: "--ctp-color-primary",
  colorBackground: "--ctp-color-background",
  colorText: "--ctp-color-text",
  colorTextSecondary: "--ctp-color-text-secondary",
  colorBorder: "--ctp-color-border",
  colorDanger: "--ctp-color-danger",
  colorSuccess: "--ctp-color-success",
  fontFamily: "--ctp-font-family",
  borderRadius: "--ctp-border-radius",
};

/** The root element's custom properties for `appearance.variables`. */
export function appearanceStyle(appearance: Appearance | undefined): CSSProperties {
  const variables = appearance?.variables ?? {};
  const style: Record<string, string> = {};
  for (const [name, property] of Object.entries(VARIABLES)) {
    const value = variables[name as keyof typeof VARIABLES];
    if (value !== undefined) {
      style[property] = value;
    }
  }
  return style;
}

/**
 * Every rule is scoped under `.ctp-root`; the variables can also be set from the page's own CSS on
 * `.ctp-root`.
 */
export const STYLES = `
.ctp-root {
  --ctp-color-primary: #0f62fe;
  --ctp-color-background: #ffffff;
  --ctp-color-text: #1a1a1a;
  --ctp-color-text-secondary: #5c5f66;
  --ctp-color-border: #d9dce1;
  --ctp-color-danger: #c62828;
  --ctp-color-success: #1b7f3b;
  --ctp-font-family: system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  --ctp-border-radius: 8px;
  box-sizing: border-box;
  max-width: 440px;
  padding: 20px;
  border: 1px solid var(--ctp-color-border);
  border-radius: var(--ctp-border-radius);
  background: var(--ctp-color-background);
  color: var(--ctp-color-text);
  font-family: var(--ctp-font-family);
  font-size: 14px;
  line-height: 1.45;
}
.ctp-root[data-theme="dark"] {
  --ctp-color-primary: #78a9ff;
  --ctp-color-background: #161616;
  --ctp-color-text: #f4f4f4;
  --ctp-color-text-secondary: #a8a8a8;
  --ctp-color-border: #393939;
  --ctp-color-danger: #ff8389;
  --ctp-color-success: #42be65;
}
.ctp-root *, .ctp-root *::before, .ctp-root *::after { box-sizing: inherit; }
.ctp-amount { margin: 0; font-size: 22px; font-weight: 600; }
.ctp-subtitle { margin: 2px 0 16px; color: var(--ctp-color-text-secondary); }
.ctp-status { display: flex; gap: 8px; align-items: baseline; justify-content: space-between;
  margin-bottom: 12px; padding: 10px 12px; border-radius: var(--ctp-border-radius);
  border: 1px solid var(--ctp-color-border); }
.ctp-status[data-tone="success"] { border-color: var(--ctp-color-success); color: var(--ctp-color-success); }
.ctp-status[data-tone="danger"] { border-color: var(--ctp-color-danger); color: var(--ctp-color-danger); }
.ctp-countdown { font-variant-numeric: tabular-nums; white-space: nowrap; }
.ctp-notice { margin: 0 0 16px; color: var(--ctp-color-text-secondary); font-size: 13px; }
.ctp-notice strong { color: var(--ctp-color-text); }
.ctp-tabs { display: flex; gap: 4px; border-bottom: 1px solid var(--ctp-color-border); margin-bottom: 16px; }
.ctp-tab { appearance: none; border: 0; border-bottom: 2px solid transparent; background: none;
  color: var(--ctp-color-text-secondary); font: inherit; padding: 8px 10px; cursor: pointer; }
.ctp-tab[aria-selected="true"] { color: var(--ctp-color-text); border-bottom-color: var(--ctp-color-primary); font-weight: 600; }
.ctp-root button:focus-visible, .ctp-root a:focus-visible { outline: 2px solid var(--ctp-color-primary); outline-offset: 2px; }
.ctp-wallets { display: grid; gap: 8px; }
.ctp-button { display: flex; gap: 10px; align-items: center; justify-content: center; width: 100%;
  appearance: none; border: 1px solid var(--ctp-color-primary); border-radius: var(--ctp-border-radius);
  background: var(--ctp-color-primary); color: #fff; font: inherit; font-weight: 600; padding: 10px 14px; cursor: pointer; }
.ctp-button:disabled { opacity: 0.6; cursor: progress; }
.ctp-button img { width: 20px; height: 20px; }
.ctp-message { margin: 10px 0 0; color: var(--ctp-color-text-secondary); }
.ctp-message[data-tone="danger"] { color: var(--ctp-color-danger); }
.ctp-qr-panel { display: grid; justify-items: center; gap: 10px; text-align: center; }
.ctp-qr { border-radius: 4px; }
.ctp-fields { display: grid; gap: 10px; margin: 0; }
.ctp-field dt { color: var(--ctp-color-text-secondary); font-size: 12px; }
.ctp-field dd { display: flex; gap: 8px; align-items: center; margin: 2px 0 0; }
.ctp-value { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; word-break: break-all; }
.ctp-copy { flex: none; appearance: none; border: 1px solid var(--ctp-color-border); border-radius: 6px;
  background: none; color: var(--ctp-color-text); font: inherit; font-size: 12px; padding: 3px 8px; cursor: pointer; }
.ctp-tx { margin: 0 0 12px; font-size: 13px; }
.ctp-tx a { color: var(--ctp-color-primary); }
`;
