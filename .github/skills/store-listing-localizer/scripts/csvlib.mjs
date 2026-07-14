// Shared CSV helpers for Partner Center listingData CSV files.
// The export is RFC-4180-ish: UTF-8 (with BOM), comma-delimited, CRLF line
// endings, double-quote escaping, and values that legitimately contain
// embedded newlines (ReleaseNotes, Description). We parse/serialize by hand so
// we never depend on a CSV package and never corrupt the field structure.

import fs from 'node:fs';

/** Parse full CSV text into an array of records, each a string[] of fields. */
export function parseCsv(text) {
  const t = text.replace(/^\uFEFF/, '');
  const records = [];
  let field = '';
  let record = [];
  let inQuotes = false;
  for (let i = 0; i < t.length; i++) {
    const c = t[i];
    if (inQuotes) {
      if (c === '"') {
        if (t[i + 1] === '"') { field += '"'; i++; }
        else inQuotes = false;
      } else {
        field += c;
      }
    } else {
      if (c === '"') inQuotes = true;
      else if (c === ',') { record.push(field); field = ''; }
      else if (c === '\r') { /* swallow, handled by \n */ }
      else if (c === '\n') { record.push(field); records.push(record); record = []; field = ''; }
      else field += c;
    }
  }
  // trailing field / record (file may or may not end with newline)
  if (field.length > 0 || record.length > 0) { record.push(field); records.push(record); }
  return records;
}

/** Quote a single field the way Partner Center expects. */
function quoteField(v) {
  const s = v == null ? '' : String(v);
  if (s === '') return '';
  if (/[",\r\n]/.test(s)) return '"' + s.replace(/"/g, '""') + '"';
  return s;
}

/**
 * Serialize records back to CSV text.
 * Defaults match the Partner Center export: UTF-8 BOM + CRLF line endings.
 */
export function serializeCsv(records, { bom = true, eol = '\r\n' } = {}) {
  const body = records.map(r => r.map(quoteField).join(',')).join(eol);
  // Partner Center exports end with a trailing newline; mirror that.
  return (bom ? '\uFEFF' : '') + body + eol;
}

/** Read + parse a CSV file from disk. */
export function readCsv(path) {
  return parseCsv(fs.readFileSync(path, 'utf8'));
}

/** Write records to disk with BOM + CRLF (Partner Center compatible). */
export function writeCsv(path, records, opts) {
  fs.writeFileSync(path, serializeCsv(records, opts), 'utf8');
}

/**
 * Build an index over a parsed listingData CSV.
 * Returns { header, localeCols: {locale: colIndex}, fieldRows: {fieldName: rowIndex} }.
 * Locale columns are every header column after the fixed `default` column.
 */
export function indexListing(records) {
  const header = records[0];
  const lower = header.map(h => h.trim().toLowerCase());
  const defaultIdx = lower.indexOf('default');
  if (defaultIdx < 0) throw new Error('CSV header missing "default" column — not a listingData export?');
  const localeCols = {};
  for (let c = defaultIdx + 1; c < header.length; c++) {
    const name = header[c].trim();
    if (name) localeCols[name] = c;
  }
  const fieldRows = {};
  for (let r = 1; r < records.length; r++) {
    const f = (records[r][0] || '').trim();
    if (f) fieldRows[f] = r;
  }
  return { header, defaultIdx, localeCols, fieldRows };
}

/** Case-insensitive locale lookup (Partner Center uses lowercase like `zh-cn`). */
export function findLocaleCol(localeCols, locale) {
  if (locale in localeCols) return localeCols[locale];
  const want = locale.toLowerCase();
  for (const [k, v] of Object.entries(localeCols)) {
    if (k.toLowerCase() === want) return v;
  }
  return undefined;
}
