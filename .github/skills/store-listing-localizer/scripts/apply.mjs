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
//   node apply.mjs --csv <export.csv> --appnames <translations.md> \
//       [--translations <translations.json>] [--enus <overrides.json>] \
//       [--changed-fields <Field1,Field2>] [--no-localize-product-name] \
//       [--out <out.csv>]
//
// Output defaults to "<export-stem>-localized.csv" next to the source.
// Exits non-zero if any ReleaseNotes/Description/ShortDescription exceeds the
// Store's per-locale character limit (see the length guard at the end).

import fs from 'node:fs';
import path from 'node:path';
import { readCsv, writeCsv, indexListing } from './csvlib.mjs';
import { classifyField, parseAppNames, appNameFor } from './fields.mjs';

function arg(name, def) {
  const i = process.argv.indexOf(name);
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : def;
}

if (process.argv.includes('--help') || !arg('--csv') || !arg('--appnames')) {
  console.log('Usage: node apply.mjs --csv <export.csv> --appnames <translations.md> ' +
              '[--translations <translations.json>] [--enus <overrides.json>] ' +
              '[--changed-fields <Field1,Field2>] [--no-localize-product-name] [--out <out.csv>]');
  process.exit(process.argv.includes('--help') ? 0 : 1);
}

const csvPath = arg('--csv');
const outPath = arg('--out') ||
  path.join(path.dirname(csvPath), path.basename(csvPath, '.csv') + '-localized.csv');

const records = readCsv(csvPath);
const { localeCols, fieldRows } = indexListing(records);
const appNames = parseAppNames(fs.readFileSync(arg('--appnames'), 'utf8'));
const translations = arg('--translations') ? JSON.parse(fs.readFileSync(arg('--translations'), 'utf8')) : {};
const enusOverrides = arg('--enus') ? JSON.parse(fs.readFileSync(arg('--enus'), 'utf8')) : {};

// "Changed" fields: their en-US text was updated, so existing per-locale values
// are stale and must NOT be preserved. Any field given an en-US override is
// implicitly changed; --changed-fields adds more (comma-separated).
const changedFields = new Set(Object.keys(enusOverrides));
for (const f of (arg('--changed-fields', '').split(',').map(s => s.trim()).filter(Boolean))) changedFields.add(f);

const enUsKey = Object.keys(localeCols).find(k => k.toLowerCase() === 'en-us');
if (!enUsKey) throw new Error('export has no en-us column');
const enCol = localeCols[enUsKey];
const targetLocales = Object.keys(localeCols).filter(k => k.toLowerCase() !== 'en-us');

// Case-insensitive lookup into translations.json by locale.
function transFor(locale, field) {
  if (translations[locale] && field in translations[locale]) return translations[locale][field];
  const lc = locale.toLowerCase();
  for (const [k, v] of Object.entries(translations)) {
    if (k.toLowerCase() === lc && field in v) return v[field];
  }
  return undefined;
}

const stats = { appname: 0, translate: 0, verbatim: 0, enusOverridden: 0, cellsWritten: 0 };

// Product-name localization: inside translatable text the product name should
// read as the locale's AppName (matching the localized Title), e.g. de-DE
// "Intelligentes Terminal", zh-CN "智能终端". We replace the en-US product name
// (the en-US Title) with the locale's AppName. This is URL-safe: the en-US
// Title is "Intelligent Terminal" (spaced, capitalized) while the GitHub URL
// uses "intelligent-terminal" (hyphenated, lowercase), so the URL is never
// touched. Disable with --no-localize-product-name.
const localizeProductName = !process.argv.includes('--no-localize-product-name');
const enUsProductName = (records[fieldRows['Title']] ? records[fieldRows['Title']][enCol] : '') || '';

function applyProductName(text, loc) {
  if (!localizeProductName || !enUsProductName) return text;
  const localized = appNameFor(appNames, loc);
  if (!localized || localized === enUsProductName) return text;
  // Replace standalone product-name occurrences only (word boundaries around
  // the spaced/capitalized form); the hyphenated URL form is unaffected.
  const esc = enUsProductName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return text.replace(new RegExp(`(^|[^/\\w])${esc}(?![\\w-])`, 'g'), `$1${localized}`);
}

for (const [field, row] of Object.entries(fieldRows)) {
  const kind = classifyField(field);

  // 1) apply en-US override first (becomes source of truth + fallback)
  if (field in enusOverrides) {
    records[row][enCol] = enusOverrides[field];
    stats.enusOverridden++;
  }
  const enUs = records[row][enCol] || '';

  for (const loc of targetLocales) {
    const col = localeCols[loc];
    const before = records[row][col] || '';
    let next = before;

    if (kind === 'appname') {
      next = appNameFor(appNames, loc) || enUs;
    } else if (kind === 'translate') {
      const t = transFor(loc, field);
      if (t !== undefined) next = t;                 // use provided translation
      else if (changedFields.has(field)) next = enUs; // changed: never keep stale → new en-US
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

writeCsv(outPath, records);
console.log(`Wrote ${outPath}`);
console.log(`Fields: ${stats.appname} appname, ${stats.translate} translate, ${stats.verbatim} verbatim`);
console.log(`en-US overrides applied: ${stats.enusOverridden}; locale cells written: ${stats.cellsWritten}`);

// Length guard: Microsoft Store hard-rejects ReleaseNotes/Description over
// their per-locale character limits (ReleaseNotes: 1500, Description: 10000).
// A too-long localized value fails the WHOLE import mid-run, so flag it here
// BEFORE the operator uploads. Exit non-zero if any field is over its limit.
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
  console.error(`\n❌ LENGTH LIMIT EXCEEDED — these will fail Partner Center import:`);
  for (const v of violations.sort((a, b) => b.len - a.len)) {
    console.error(`   ${v.field} [${v.loc}]: ${v.len} > ${v.limit}`);
  }
  console.error(`Shorten these locales' translations (or the en-US source) and re-run before importing.`);
  process.exitCode = 2;
} else {
  console.log(`Length check: all ReleaseNotes/Description/ShortDescription within Store limits.`);
}
