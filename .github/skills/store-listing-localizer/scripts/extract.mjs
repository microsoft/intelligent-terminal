#!/usr/bin/env node
// extract.mjs — build a translation "work order" from a listingData export.
//
// Reads the exported CSV and prints a JSON object describing exactly what needs
// translating: the en-US source value for every translatable field, the full
// target locale list, and (per field) which locales currently look stale
// (their value equals the en-US value or is empty). The agent consumes this to
// drive per-language translation, then feeds results to apply.mjs.
//
// Usage:
//   node extract.mjs --csv <export.csv> [--enus <overrides.json>] \
//       [--changed-fields <Field1,Field2>] [--out <work-order.json>]
//
//   --csv            Path to the Partner Center listingData export (required).
//   --enus           Optional JSON { "<Field>": "<value>" } overriding the en-US
//                    source for specific fields. Use this when the new en-US text
//                    (e.g. a new ReleaseNotes) is supplied in the prompt rather
//                    than already present in the export's en-US column.
//   --changed-fields Comma-separated fields whose en-US text changed, so every
//                    locale is flagged stale (ReleaseNotes version drift is also
//                    auto-detected).
//   --out            Where to write the work order JSON (default: stdout).

import fs from 'node:fs';
import { readCsv, indexListing } from './csvlib.mjs';
import { classifyField } from './fields.mjs';

function arg(name, def) {
  const i = process.argv.indexOf(name);
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : def;
}

if (process.argv.includes('--help') || !arg('--csv')) {
  console.log('Usage: node extract.mjs --csv <export.csv> [--enus <overrides.json>] [--changed-fields <Field1,Field2>] [--out <work-order.json>]');
  // --help is a successful request; only a genuinely missing --csv is an error.
  process.exit(process.argv.includes('--help') ? 0 : 1);
}

const csvPath = arg('--csv');
const enusOverrides = arg('--enus') ? JSON.parse(fs.readFileSync(arg('--enus'), 'utf8')) : {};
const changedFields = new Set(
  (arg('--changed-fields', '').split(',').map(s => s.trim()).filter(Boolean))
);

// Extract a version token like "v0.1.1661" regardless of the (possibly
// localized) leading word — "Version", "版本", "バージョン", "Weergawe", etc.
function versionToken(s) {
  const m = (s || '').match(/v\d+(?:\.\d+)+/i);
  return m ? m[0].toLowerCase() : '';
}

const records = readCsv(csvPath);
const { localeCols, fieldRows } = indexListing(records);

const enUsKey = Object.keys(localeCols).find(k => k.toLowerCase() === 'en-us');
if (!enUsKey) throw new Error('export has no en-us column');
const enCol = localeCols[enUsKey];
const targetLocales = Object.keys(localeCols).filter(k => k.toLowerCase() !== 'en-us');

const fields = [];
for (const [field, row] of Object.entries(fieldRows)) {
  if (classifyField(field) !== 'translate') continue;
  const enUs = (field in enusOverrides) ? enusOverrides[field] : (records[row][enCol] || '');
  if (!enUs.trim()) continue; // nothing to translate

  // A field is "changed" when its en-US text was overridden, when the caller
  // lists it in --changed-fields, or (for ReleaseNotes) when the locale's
  // embedded version token differs from en-US — the classic stale-translation
  // case where a locale still holds a fully-translated *older* release note.
  const isChanged = (field in enusOverrides) || changedFields.has(field);
  const enVer = versionToken(enUs);

  const staleLocales = [];
  for (const loc of targetLocales) {
    const cur = records[row][localeCols[loc]] || '';
    let stale = !cur.trim() || cur === (records[row][enCol] || '');
    if (!stale && isChanged) stale = true;                  // forced changed
    if (!stale && enVer && versionToken(cur) !== enVer) stale = true; // version drift
    if (stale) staleLocales.push(loc);
  }
  fields.push({ field, enUs, changed: isChanged || staleLocales.length > 0, staleLocales });
}

const workOrder = {
  csv: csvPath,
  enUsColumn: enUsKey,
  targetLocales,
  fieldsNeedingTranslation: fields,
  note: 'Translate each field.enUs into every targetLocale (or at least staleLocales). ' +
        'Follow references/localization-rules.md for locked tokens and terminology. ' +
        'Return results to apply.mjs as { "<locale>": { "<Field>": "<translated>" } }.',
};

const json = JSON.stringify(workOrder, null, 2);
if (arg('--out')) { fs.writeFileSync(arg('--out'), json, 'utf8'); console.error(`wrote ${arg('--out')}`); }
else console.log(json);
