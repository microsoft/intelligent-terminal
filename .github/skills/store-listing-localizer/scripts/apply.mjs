#!/usr/bin/env node
// apply.mjs — merge translations into a listingData export, producing the CSV
// to re-import into Partner Center.
//
// Handling per field row (see fields.mjs / references/listing-csv-format.md):
//   - appname  (Title)   : every locale column = localized AppName from the
//                          translations.md table (falls back to en-US).
//   - translate (text)   : locale = translations[locale][field] if provided,
//                          else keep the existing non-empty locale value,
//                          else fall back to the en-US value.
//   - verbatim (assets)  : empty locale cells are filled from en-US; existing
//                          values are left untouched (never clobbered).
//
// en-US overrides (--enus) are applied to the en-US column FIRST, so a new
// ReleaseNotes supplied in the prompt becomes the source of truth for both the
// en-US listing and the per-locale fallbacks.
//
// Usage:
//   node apply.mjs --csv <export.csv> [--appnames <translations.md>] \
//       [--translations <translations.json>] [--enus <overrides.json>] \
//       [--changed-fields <Field1,Field2>] [--no-localize-product-name] \
//       [--out <out.csv>]
//
// --appnames defaults to the bundled references/intelligent-terminal-translations.md.
// Output defaults to "<export-stem>-localized.csv" next to the source.
// Exits non-zero if any ReleaseNotes/Description/ShortDescription exceeds the
// Store's per-locale character limit (see the length guard at the end).

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { readCsv, writeCsv, indexListing } from './csvlib.mjs';
import { classifyField, parseAppNames, appNameFor } from './fields.mjs';

function arg(name, def) {
  const i = process.argv.indexOf(name);
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : def;
}

// The AppName table ships with the skill; default to the bundled copy so the
// script is runnable without an external file. Override with --appnames.
const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const defaultAppNames = path.join(scriptDir, '..', 'references', 'intelligent-terminal-translations.md');

if (process.argv.includes('--help') || !arg('--csv')) {
  console.log('Usage: node apply.mjs --csv <export.csv> [--appnames <translations.md>] ' +
              '[--translations <translations.json>] [--enus <overrides.json>] ' +
              '[--changed-fields <Field1,Field2>] [--no-localize-product-name] [--out <out.csv>]');
  console.log('  --appnames defaults to the bundled references/intelligent-terminal-translations.md');
  process.exit(process.argv.includes('--help') ? 0 : 1);
}

const csvPath = arg('--csv');
const outPath = arg('--out') ||
  path.join(path.dirname(csvPath), path.basename(csvPath, '.csv') + '-localized.csv');

const appNamesPath = arg('--appnames', defaultAppNames);
if (!fs.existsSync(appNamesPath)) {
  throw new Error(`AppName table not found: ${appNamesPath}. Pass --appnames <translations.md> ` +
                  `or restore references/intelligent-terminal-translations.md.`);
}

const records = readCsv(csvPath);
const { localeCols, fieldRows } = indexListing(records);
const appNames = parseAppNames(fs.readFileSync(appNamesPath, 'utf8'));
const translations = arg('--translations') ? JSON.parse(fs.readFileSync(arg('--translations'), 'utf8')) : {};
const enusOverrides = arg('--enus') ? JSON.parse(fs.readFileSync(arg('--enus'), 'utf8')) : {};

// Extract a version token like "v0.1.1841" regardless of the (possibly
// localized) leading word — "Version", "版本", "バージョン", etc. Used for the
// automatic ReleaseNotes stale-version safety net below.
function versionToken(s) {
  const m = (s || '').match(/v\d+(?:\.\d+)+/i);
  return m ? m[0].toLowerCase() : '';
}

// "Changed" fields: their en-US text was updated, so existing per-locale values
// are stale and must NOT be preserved. Any field given an en-US override is
// implicitly changed; --changed-fields adds more (comma-separated).
const changedFields = new Set(Object.keys(enusOverrides));
for (const f of (arg('--changed-fields', '').split(',').map(s => s.trim()).filter(Boolean))) changedFields.add(f);

const enUsKey = Object.keys(localeCols).find(k => k.toLowerCase() === 'en-us');
if (!enUsKey) throw new Error('export has no en-us column');
const enCol = localeCols[enUsKey];
const targetLocales = Object.keys(localeCols).filter(k => k.toLowerCase() !== 'en-us');

// A valid listingData export always has a `Title` row (it drives both the
// localized Title and product-name substitution in body text). Its absence
// means the input isn't a real export — fail fast like the other structural
// checks (missing `default`/`en-us`, column-count mismatch) instead of silently
// skipping product-name localization and producing a quietly-wrong CSV.
if (fieldRows['Title'] == null) {
  throw new Error('export has no "Title" field row — not a valid listingData export?');
}

// Case-insensitive lookup into translations.json by locale. Validates that a
// found value is a string: null/undefined are treated as "missing" (returns
// undefined so the caller falls back), while any other non-string type
// (object/number/array) is a malformed payload and throws with the offending
// locale+field so the failure is explicit rather than crashing later in
// applyProductName()'s .replace().
function transFor(locale, field) {
  const pick = (obj, srcLoc) => {
    if (!obj || !(field in obj)) return undefined;
    const v = obj[field];
    if (v === null || v === undefined) return undefined;
    if (typeof v !== 'string') {
      throw new Error(`translations["${srcLoc}"]["${field}"] is ${typeof v}, expected string. ` +
                      `Fix the malformed translation payload.`);
    }
    return v;
  };
  const direct = pick(translations[locale], locale);
  if (direct !== undefined) return direct;
  const lc = locale.toLowerCase();
  for (const [k, v] of Object.entries(translations)) {
    if (k.toLowerCase() === lc) {
      const hit = pick(v, k);
      if (hit !== undefined) return hit;
    }
  }
  return undefined;
}

const stats = { appname: 0, translate: 0, verbatim: 0, enusOverridden: 0, cellsWritten: 0, versionDrift: 0 };

// Product-name localization: inside translatable text the product name should
// read as the locale's AppName (matching the localized Title), e.g. de-DE
// "Intelligentes Terminal", zh-CN "智能终端". We replace the en-US product name
// (the en-US Title) with the locale's AppName. This is URL-safe: the en-US
// Title is "Intelligent Terminal" (spaced, capitalized) while the GitHub URL
// uses "intelligent-terminal" (hyphenated, lowercase), so the URL is never
// touched. Disable with --no-localize-product-name.
const localizeProductName = !process.argv.includes('--no-localize-product-name');
// Use the --enus Title override when present (it's the source of truth); fall
// back to the CSV's en-US Title. Computed here from enusOverrides directly since
// the override isn't written into `records` until the main loop runs.
const enUsProductName = ('Title' in enusOverrides
  ? enusOverrides['Title']
  : (records[fieldRows['Title']] ? records[fieldRows['Title']][enCol] : '')) || '';
// Precompute the matcher once (it depends only on the constant en-US product
// name): matches the standalone spaced/capitalized "Intelligent Terminal", not
// the hyphenated URL form. null when there's no product name to localize.
const productNameRe = enUsProductName
  ? new RegExp(`(^|[^/\\w])${enUsProductName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?![\\w-])`, 'g')
  : null;

function applyProductName(text, loc) {
  if (!localizeProductName || !productNameRe) return text;
  const localized = appNameFor(appNames, loc);
  if (!localized || localized === enUsProductName) return text;
  // Use a replace callback so any `$` in a localized name is inserted literally
  // rather than interpreted as a special replacement pattern ($1, $&, …).
  return text.replace(productNameRe, (_m, pre) => pre + localized);
}

for (const [field, row] of Object.entries(fieldRows)) {
  const kind = classifyField(field);

  // 1) apply en-US override first (becomes source of truth + fallback)
  if (field in enusOverrides) {
    records[row][enCol] = enusOverrides[field];
    stats.enusOverridden++;
  }
  const enUs = records[row][enCol] || '';
  // Automatic stale-version safety net (ReleaseNotes and any versioned field):
  // if en-US carries a version token, a locale whose existing value has a
  // DIFFERENT token is stale even when the operator forgot --changed-fields.
  const enVer = versionToken(enUs);

  for (const loc of targetLocales) {
    const col = localeCols[loc];
    const before = records[row][col] || '';
    let next = before;

    if (kind === 'appname') {
      next = appNameFor(appNames, loc) || enUs;
    } else if (kind === 'translate') {
      // Drift only applies to an EXISTING non-empty locale value with a
      // different version token; an empty cell is a separate "fall back to
      // en-US" case, not drift (kept out of the drift stat).
      const versionDrift = enVer && before.trim() && versionToken(before) !== enVer;
      const t = transFor(loc, field);
      if (t !== undefined) next = t;                 // use provided translation
      else if (changedFields.has(field)) next = enUs; // marked changed: never keep stale → new en-US
      else if (versionDrift) { next = enUs; stats.versionDrift++; } // auto: stale version → new en-US
      else if (before.trim()) next = before;          // unchanged: keep existing translation
      else next = enUs;                              // empty: fall back to en-US
      next = applyProductName(next, loc);            // localize product name in body text
    } else { // verbatim
      if (!before.trim()) next = enUs;             // fill empty asset cells only
    }

    if (next !== before) { records[row][col] = next; stats.cellsWritten++; }
  }
  stats[kind]++;
}

// Length guard (runs BEFORE writing so a known-bad CSV never lands on disk —
// this file is the one the operator uploads). Microsoft Store hard-rejects
// ReleaseNotes/Description/ShortDescription over their per-locale character
// limits, failing the WHOLE import mid-run, so refuse to emit an over-limit CSV.
const LIMITS = { ReleaseNotes: 1500, Description: 10000, ShortDescription: 1000 };
const violations = [];
for (const [field, limit] of Object.entries(LIMITS)) {
  const row = fieldRows[field];
  if (row == null) continue;
  for (const [loc, col] of Object.entries(localeCols)) {
    const len = (records[row][col] || '').length;
    if (len > limit) violations.push({ field, loc, len, limit });
  }
}
if (violations.length) {
  console.error(`\n❌ LENGTH LIMIT EXCEEDED — refusing to write ${outPath} (would fail Partner Center import):`);
  for (const v of violations.sort((a, b) => b.len - a.len)) {
    console.error(`   ${v.field} [${v.loc}]: ${v.len} > ${v.limit}`);
  }
  console.error(`Shorten these locales' translations (or the en-US source) and re-run.`);
  process.exit(2);
}

writeCsv(outPath, records);
console.log(`Wrote ${outPath}`);
console.log(`Fields: ${stats.appname} appname, ${stats.translate} translate, ${stats.verbatim} verbatim`);
console.log(`en-US overrides applied: ${stats.enusOverridden}; locale cells written: ${stats.cellsWritten}`);
if (stats.versionDrift) {
  console.log(`Auto version-drift: ${stats.versionDrift} stale-version locale cell(s) refreshed to the new en-US text.`);
}
console.log(`Length check: all ReleaseNotes/Description/ShortDescription within Store limits.`);
