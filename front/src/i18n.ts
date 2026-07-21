// Internationalization with compile-time-bundled message catalogs.
//
// Translations live in JSON files under front/locales/, one file per language
// named after its code (e.g. en.json, ja.json). scripts/gen-i18n.mjs scans
// that directory and bundles every *.json into the app, so the language set is
// driven entirely by the files present — no language is hard-coded here. The
// detected locale is resolved at runtime and falls back to the default catalog
// when a code has no file.

import { catalogs } from "./locales.generated";

const DEFAULT_LANG = "en" in catalogs ? "en" : Object.keys(catalogs)[0];

function detectLang(): string {
  const nav =
    typeof navigator !== "undefined" ? navigator.language : "";
  const code = nav.toLowerCase().split("-")[0];
  return code in catalogs ? code : DEFAULT_LANG;
}

export const lang: string = detectLang();

// Reflect the detected locale on the document root for accessibility/CSS.
if (typeof document !== "undefined") {
  document.documentElement.lang = lang;
}

export function t(key: string): string {
  return catalogs[lang]?.[key] ?? catalogs[DEFAULT_LANG]?.[key] ?? key;
}

export function tFmt(
  key: string,
  params: Record<string, string | number>,
): string {
  let msg = t(key);
  for (const [k, v] of Object.entries(params)) {
    msg = msg.replaceAll(`{${k}}`, String(v));
  }
  return msg;
}
