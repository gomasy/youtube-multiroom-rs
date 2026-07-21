type Catalog = Record<string, string>;

const DEFAULT_LANG = "en";

let catalogs: Record<string, Catalog> = {};
export let lang: string = DEFAULT_LANG;

function detectLang(): string {
  const code = navigator.language.toLowerCase().split("-")[0];
  return code || DEFAULT_LANG;
}

async function fetchCatalog(code: string): Promise<Catalog> {
  const res = await fetch(`/locales/${code}.json`);
  if (!res.ok) throw new Error(`locale ${code}: ${res.status}`);
  return res.json();
}

export async function init(): Promise<void> {
  const detected = detectLang();

  if (detected === DEFAULT_LANG) {
    catalogs[DEFAULT_LANG] = await fetchCatalog(DEFAULT_LANG);
  } else {
    const [fallback, local] = await Promise.all([
      fetchCatalog(DEFAULT_LANG),
      fetchCatalog(detected).catch(() => null),
    ]);
    catalogs[DEFAULT_LANG] = fallback;
    if (local) {
      catalogs[detected] = local;
      lang = detected;
    }
  }

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
