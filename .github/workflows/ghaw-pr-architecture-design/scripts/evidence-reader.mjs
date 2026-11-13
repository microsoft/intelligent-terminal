import { execFileSync } from 'node:child_process';

const SHA = /^[0-9a-f]{40}$/;
const MAX_BYTES = 1024 * 1024;

function canonicalPath(value) {
    if (typeof value !== 'string' || value.length > 4096 || value.startsWith('-') ||
        value.startsWith('/') || /[\\:\x00-\x1f\x7f]/.test(value) ||
        value.split('/').some(part => !part || part === '.' || part === '..'))
        throw Error('Invalid canonical repository path.');
}

function integer(value, minimum, maximum) {
    if (!Number.isSafeInteger(value) || value < minimum || value > maximum)
        throw Error('Invalid bounded range or page.');
}

export function readEvidence(root, scope, request) {
    try {
        if (!scope?.admitted || ![scope.comparisonBase, scope.headSha, scope.baseSha].every(
            value => typeof value === 'string' && SHA.test(value)))
            throw Error('Invalid trusted immutable scope.');
        const allowed = ['operation', 'revision', 'path', 'start_line', 'end_line', 'query', 'offset', 'limit'];
        if (!request || Object.keys(request).some(key => !allowed.includes(key)) ||
            !['diff', 'source', 'search', 'tree'].includes(request.operation))
            throw Error('Unsupported evidence operation or extra option.');
        const { operation, revision = 'head', path = '', start_line = 1,
            end_line = 400, query = '', offset = 0, limit = 100 } = request;
        if (!['base', 'head'].includes(revision)) throw Error('Revision must be base or head.');
        if (path) canonicalPath(path);
        else if (typeof path !== 'string') throw Error('Invalid repository path.');
        integer(start_line, 1, 10000000);
        integer(end_line, start_line, start_line + 399);
        integer(offset, 0, 1000000);
        integer(limit, 1, 200);
        if (typeof query !== 'string' || Buffer.byteLength(query) > 256 || /[\x00-\x1f\x7f]/.test(query))
            throw Error('Invalid bounded literal query.');
        const sha = revision === 'base' ? scope.comparisonBase : scope.headSha;
        const nil = process.platform === 'win32' ? 'NUL' : '/dev/null';
        const env = { ...Object.fromEntries(Object.entries(process.env).filter(
            ([key]) => !/^GIT_/i.test(key))), GIT_CONFIG_NOSYSTEM: '1',
            GIT_CONFIG_GLOBAL: nil, GIT_NO_REPLACE_OBJECTS: '1', GIT_LITERAL_PATHSPECS: '1',
            GIT_OPTIONAL_LOCKS: '0', GIT_PAGER: 'cat' };
        const git = args => execFileSync('git', ['--no-pager', '-c', 'core.fsmonitor=false',
            '-c', `core.hooksPath=${nil}`, '-c', 'core.pager=cat', '-c', 'core.quotePath=false', ...args],
        { cwd: root, env, timeout: 30000, maxBuffer: MAX_BYTES, encoding: 'buffer' });
        const text = bytes => {
            if (bytes.includes(0)) throw Error('Binary data is not readable text.');
            return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
        };
        const identity = { comparisonBase: scope.comparisonBase, headSha: scope.headSha };
        if (path) {
            const revisions = operation === 'diff' ? [scope.comparisonBase, scope.headSha] : [sha];
            if (!revisions.some(value => git(['ls-tree', value, '--', path]).length))
                throw Error('Missing repository path at the selected snapshot.');
        }
        if (operation === 'diff') {
            const diff = text(git(['diff', '--no-ext-diff', '--no-textconv', '--no-color',
                '--no-renames', '--ignore-submodules=none', '--unified=3', scope.comparisonBase, scope.headSha, '--',
                ...(path ? [path] : [])]));
            return { ...identity, diff, gaps: /(^|\n)Binary files /.test(diff) ?
                ['Binary changes have no textual hunks.'] : [] };
        }
        if (operation === 'source') {
            canonicalPath(path);
            const entry = text(git(['ls-tree', sha, '--', path])).trim();
            if (!/^\d+ blob [0-9a-f]{40}\t/.test(entry))
                throw Error('Missing source blob (directories and submodules are not source).');
            const lines = text(git(['cat-file', 'blob', `${sha}:${path}`])).split('\n');
            if (lines.at(-1) === '') lines.pop();
            if (start_line > lines.length) throw Error('Source range is beyond the blob.');
            const source = lines.slice(start_line - 1, end_line).join('\n');
            if (Buffer.byteLength(source) > 65536) throw Error('Source range exceeds 64 KiB; request fewer lines.');
            return { ...identity, revision, path, startLine: start_line,
                endLine: Math.min(end_line, lines.length), totalLines: lines.length, source };
        }
        if (operation === 'tree') {
            const entries = new TextDecoder('utf-8', { fatal: true }).decode(
                git(['ls-tree', '-r', '-z', '--name-only', sha, '--',
                    ...(path ? [path] : [])])).split('\0').filter(Boolean);
            entries.forEach(canonicalPath);
            return { ...identity, revision, entries: entries.slice(offset, offset + limit),
                total: entries.length, nextOffset: offset + limit < entries.length ? offset + limit : null };
        }
        if (!query) throw Error('Search requires a nonempty literal query.');
        let matches;
        try {
            matches = text(git(['grep', '--no-textconv', '-I', '-n', '-F', '-e', query,
                sha, '--', ...(path ? [path] : [])]));
        } catch (error) {
            if (error.status !== 1) throw error;
            matches = '';
        }
        return { ...identity, revision, matches, gaps: ['Search excludes binary blobs; no text conversion is performed.'] };
    } catch (error) {
        return { error: `Evidence unavailable: ${error.code === 'ENOBUFS' ?
            'output exceeds 1 MiB; narrow the request' : error.message.slice(0, 1000)}`, gap: true };
    }
}
