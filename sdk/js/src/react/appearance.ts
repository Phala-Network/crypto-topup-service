import type { CSSProperties } from "react";

/** Theming in the spirit of Stripe's Appearance API: a base theme plus a few variables. */
export interface Appearance {
  theme?: "light" | "dark";
  variables?: {
    colorPrimary?: string;
    /** The color of text appearing on top of any `colorPrimary` background, such as the pay button. */
    accessibleColorOnColorPrimary?: string;
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
  colorPrimary: "--pp-color-primary",
  accessibleColorOnColorPrimary: "--pp-accessible-color-on-color-primary",
  colorBackground: "--pp-color-background",
  colorText: "--pp-color-text",
  colorTextSecondary: "--pp-color-text-secondary",
  colorBorder: "--pp-color-border",
  colorDanger: "--pp-color-danger",
  colorSuccess: "--pp-color-success",
  fontFamily: "--pp-font-family",
  borderRadius: "--pp-border-radius",
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
 * Every rule is scoped under `.pp-root`; the variables can also be set from the page's own CSS on
 * `.pp-root`.
 */
export const STYLES = `
.pp-root {
  --pp-color-primary: #0f62fe;
  --pp-accessible-color-on-color-primary: #ffffff;
  --pp-color-background: #ffffff;
  --pp-color-text: #1a1a1a;
  --pp-color-text-secondary: #5c5f66;
  --pp-color-border: #d9dce1;
  --pp-color-danger: #c62828;
  --pp-color-success: #1b7f3b;
  --pp-font-family: system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  --pp-border-radius: 8px;
  box-sizing: border-box;
  max-width: 440px;
  padding: 20px;
  border: 1px solid var(--pp-color-border);
  border-radius: var(--pp-border-radius);
  background: var(--pp-color-background);
  color: var(--pp-color-text);
  font-family: var(--pp-font-family);
  font-size: 14px;
  line-height: 1.45;
}
.pp-root[data-theme="dark"] {
  --pp-color-primary: #78a9ff;
  --pp-color-background: #161616;
  --pp-color-text: #f4f4f4;
  --pp-color-text-secondary: #a8a8a8;
  --pp-color-border: #393939;
  --pp-color-danger: #ff8389;
  --pp-color-success: #42be65;
}
.pp-root *, .pp-root *::before, .pp-root *::after { box-sizing: inherit; }
.pp-amount { margin: 0; overflow-wrap: anywhere; font-size: 22px; font-weight: 600; }
.pp-subtitle { margin: 2px 0 16px; color: var(--pp-color-text-secondary); }
.pp-badge { font-weight: 600; }
.pp-payments { list-style: none; margin: 12px 0 0; padding: 0; }
.pp-payments li { margin: 4px 0; }
.pp-status { display: flex; gap: 8px; align-items: baseline; justify-content: space-between;
  margin-bottom: 12px; padding: 10px 12px; border-radius: var(--pp-border-radius);
  border: 1px solid var(--pp-color-border); }
.pp-status[data-tone="success"] { border-color: var(--pp-color-success); color: var(--pp-color-success); }
.pp-status[data-tone="danger"] { border-color: var(--pp-color-danger); color: var(--pp-color-danger); }
.pp-countdown { font-variant-numeric: tabular-nums; white-space: nowrap; }
.pp-notice { margin: 0 0 16px; color: var(--pp-color-text-secondary); font-size: 13px; }
.pp-notice strong { color: var(--pp-color-text); }
.pp-tabs { display: flex; gap: 4px; border-bottom: 1px solid var(--pp-color-border); margin-bottom: 16px; }
.pp-tab { appearance: none; border: 0; border-bottom: 2px solid transparent; background: none;
  color: var(--pp-color-text-secondary); font: inherit; padding: 8px 10px; cursor: pointer; }
.pp-tab[aria-selected="true"] { color: var(--pp-color-text); border-bottom-color: var(--pp-color-primary); font-weight: 600; }
.pp-root button:focus-visible, .pp-root a:focus-visible { outline: 2px solid var(--pp-color-primary); outline-offset: 2px; }
.pp-wallets { display: grid; gap: 8px; }
.pp-button { display: flex; gap: 10px; align-items: center; justify-content: center; width: 100%;
  appearance: none; border: 1px solid var(--pp-color-primary); border-radius: var(--pp-border-radius);
  background: var(--pp-color-primary); color: var(--pp-accessible-color-on-color-primary); font: inherit; font-weight: 600; padding: 10px 14px; cursor: pointer; }
.pp-button:disabled { opacity: 0.6; cursor: progress; }
.pp-button img { width: 20px; height: 20px; }
.pp-wallet-name { font-weight: 400; opacity: 0.85; }
.pp-message { margin: 10px 0 0; color: var(--pp-color-text-secondary); }
.pp-message[data-tone="danger"] { color: var(--pp-color-danger); }
.pp-qr-panel { display: grid; justify-items: center; gap: 10px; text-align: center; }
.pp-qr { border-radius: 4px; }
.pp-fields { display: grid; gap: 10px; margin: 0; }
.pp-field dt { color: var(--pp-color-text-secondary); font-size: 12px; }
.pp-field dd { display: flex; gap: 8px; align-items: center; margin: 2px 0 0; }
.pp-value { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; word-break: break-all; }
.pp-copy { flex: none; appearance: none; border: 1px solid var(--pp-color-border); border-radius: 6px;
  background: none; color: var(--pp-color-text); font: inherit; font-size: 12px; padding: 3px 8px; cursor: pointer; }
.pp-tx { margin: 0 0 12px; font-size: 13px; }
.pp-tx a { color: inherit; text-decoration: underline; text-underline-offset: 2px; }
.pp-tx .pp-value { overflow-wrap: anywhere; }
`;
