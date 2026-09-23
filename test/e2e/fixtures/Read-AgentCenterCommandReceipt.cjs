// Read only the disposable fixture's command receipt; never alter its database.
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const { DatabaseSync } = require('node:sqlite');

const [evidenceDirectory, commandId] = process.argv.slice(2);
if (!evidenceDirectory || !/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(commandId || '')) {
    throw new Error('Expected a fixture directory and diagnostic command UUID.');
}
const root = fs.realpathSync(evidenceDirectory);
const runtime = JSON.parse(fs.readFileSync(path.join(root, 'runtime.json'), 'utf8'));
const database = fs.realpathSync(path.join(runtime.stateRoot, 'work.db'));
const relative = path.relative(root, database);
if (path.isAbsolute(relative) || relative.startsWith('..') || runtime.scenario !== 'WorkFlow') {
    throw new Error('Receipt inspection is restricted to this isolated WorkFlow fixture.');
}
const db = new DatabaseSync(database, { readOnly: true });
try {
    const row = db.prepare('SELECT response FROM commands WHERE principal = ? AND command_id = ?')
        .get('human', commandId);
    console.log(JSON.stringify({ commandId, receipt: row ? JSON.parse(row.response) : null }));
} finally {
    db.close();
}
