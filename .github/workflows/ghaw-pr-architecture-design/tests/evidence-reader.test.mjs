import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import test from 'node:test';
import { readEvidence } from '../scripts/evidence-reader.mjs';

test('bounded immutable evidence preserves supporting paths and rejects command/options without side effects', () => {
    const generated = path.resolve('Generated Files');
    fs.mkdirSync(generated, { recursive: true });
    const directory = fs.mkdtempSync(path.join(generated, 'architecture-evidence-test-'));
    const nil = process.platform === 'win32' ? 'NUL' : '/dev/null';
    const git = (...args) => execFileSync('git', ['-c', `core.hooksPath=${nil}`,
        '-c', 'commit.gpgsign=false', ...args], { cwd: directory, encoding: 'utf8' }).trim();
    const originalGitDir = process.env.GIT_DIR;
    const originalExternalDiff = process.env.GIT_EXTERNAL_DIFF;
    try {
        git('init', '-q');
        git('config', 'user.name', 'Evidence test');
        git('config', 'user.email', 'evidence@example.invalid');
        fs.mkdirSync(path.join(directory, 'vendor'));
        fs.writeFileSync(path.join(directory, 'vendor', 'support.txt'), 'first\n');
        fs.writeFileSync(path.join(directory, 'binary'), Buffer.from([0, 1, 2]));
        fs.writeFileSync(path.join(directory, 'large.txt'), 'x'.repeat(1024 * 1024 + 1));
        git('add', '.');
        git('commit', '-qm', 'base');
        const base = git('rev-parse', 'HEAD');
        fs.writeFileSync(path.join(directory, 'vendor', 'support.txt'), 'first\nsecond\n');
        git('add', '.');
        git('commit', '-qm', 'head');
        const head = git('rev-parse', 'HEAD');
        const scope = { admitted: true, baseSha: base, comparisonBase: base, headSha: head };
        const read = request => readEvidence(directory, scope, request);
        const before = git('status', '--porcelain');
        process.env.GIT_DIR = path.join(directory, 'missing-git-directory');
        process.env.GIT_EXTERNAL_DIFF = 'node -e "require(\'fs\').writeFileSync(\'sentinel\',\'unsafe\')"';
        assert.match(read({ operation: 'diff' }).diff, /\+second/);
        assert.equal(read({ operation: 'source', path: 'vendor/support.txt', revision: 'base' }).source, 'first');
        assert.equal(read({ operation: 'source', path: 'vendor/support.txt' }).source, 'first\nsecond');
        assert.match(read({ operation: 'search', query: 'second' }).matches, /support.txt:2:second/);
        const page = read({ operation: 'tree', limit: 1 });
        assert.equal(page.entries.length, 1);
        assert.equal(page.nextOffset, 1);
        assert.equal(read({ operation: 'search', query: '--open-files-in-pager=touch sentinel' }).matches, '');
        for (const request of [
            { operation: 'diff', args: ['--output=sentinel'] },
            { operation: 'search', query: 'first', pager: 'touch sentinel' },
            { operation: '--output=sentinel' },
            { operation: 'diff', path: '--output=sentinel' },
            { operation: 'source', path: '../sentinel' },
            { operation: 'source', path: 'C:\\sentinel' },
            { operation: 'source', path: 'vendor/support.txt', revision: head },
            { operation: 'source', path: 'vendor/support.txt', start_line: 0 },
            { operation: 'source', path: 'vendor/support.txt', end_line: 401 },
            { operation: 'source', path: 'missing' },
            { operation: 'source', path: 'binary' },
            { operation: 'source', path: 'large.txt' },
            { operation: 'source', path: 'vendor/support.txt', start_line: 3, end_line: 4 },
            { operation: 'diff', path: 'missing' },
            { operation: 'tree', limit: 201 },
        ]) assert.equal(read(request).gap, true, JSON.stringify(request));
        assert.equal(readEvidence(directory, { ...scope, headSha: 'HEAD' }, { operation: 'diff' }).gap, true);
        assert.equal(fs.existsSync(path.join(directory, 'sentinel')), false);
        if (originalGitDir === undefined) delete process.env.GIT_DIR;
        else process.env.GIT_DIR = originalGitDir;
        assert.equal(git('status', '--porcelain'), before);
    } finally {
        if (originalGitDir === undefined) delete process.env.GIT_DIR;
        else process.env.GIT_DIR = originalGitDir;
        if (originalExternalDiff === undefined) delete process.env.GIT_EXTERNAL_DIFF;
        else process.env.GIT_EXTERNAL_DIFF = originalExternalDiff;
        fs.rmSync(directory, { recursive: true, force: true });
    }
});
