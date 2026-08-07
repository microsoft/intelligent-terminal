// Field classification for the listingData CSV, plus the AppName table parser.
//
// Each field row in the CSV is classified into one of three handling modes:
//   - 'appname'  : the localized product name (Title). Filled from the
//                  intelligent-terminal-translations.md table.
//   - 'translate': free user-visible text that must be localized per locale
//                  (Description, ReleaseNotes, captions, features, ...).
//   - 'verbatim' : non-translatable values copied from en-US to every locale
//                  (screenshot/logo/trailer URLs, booleans, legal/IDs).
//
// The classification is intentionally conservative: anything not explicitly
// recognized as translatable text is treated as 'verbatim' so we never garble
// an asset URL or a boolean flag.

// Only `Title` is auto-filled from the AppName table. ShortTitle/SortTitle/
// VoiceTitle are usually empty; treat them as translatable text (skipped when
// empty) rather than forcing the AppName into them.
const APPNAME_FIELDS = new Set(['Title']);

const TRANSLATE_PATTERNS = [
  /^Description$/,
  /^ReleaseNotes$/,
  /^ShortTitle$/,
  /^SortTitle$/,
  /^VoiceTitle$/,
  /^ShortDescription$/,
  /^Feature\d+$/,
  /^[A-Za-z]*ScreenshotCaption\d+$/,   // Desktop/Mobile/Xbox/Holographic/SurfaceHub caption
  /^MinimumHardwareReq\d+$/,
  /^RecommendedHardwareReq\d+$/,
  /^SearchTerm\d+$/,
  /^TrailerTitle\d+$/,
];

/** Return 'appname' | 'translate' | 'verbatim' for a field name. */
export function classifyField(field) {
  if (APPNAME_FIELDS.has(field)) return 'appname';
  if (TRANSLATE_PATTERNS.some(re => re.test(field))) return 'translate';
  return 'verbatim';
}

/**
 * Parse the markdown AppName table (| Locale | AppName |) into a map.
 * Keys are normalized to lowercase so `zh-CN` and `zh-cn` both resolve.
 */
export function parseAppNames(md) {
  const out = {};
  for (const line of md.split(/\r?\n/)) {
    // Primary language subtag is 2–8 letters per BCP-47 (not just 2) — covers
    // fil, kok, quz, and the qps-* pseudo-locales, not only 2-letter codes.
    const m = line.match(/^\s*\|\s*([A-Za-z]{2,8}(?:-[A-Za-z0-9]+)*)\s*\|\s*(.+?)\s*\|\s*$/);
    if (!m) continue;
    const locale = m[1].trim();
    const name = m[2].trim();
    if (locale.toLowerCase() === 'locale' || /^-+$/.test(name)) continue; // header/separator
    out[locale.toLowerCase()] = name;
  }
  return out;
}

export function appNameFor(appNames, locale) {
  return appNames[locale.toLowerCase()];
}
