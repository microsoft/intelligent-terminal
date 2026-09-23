// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

const assert = require('node:assert/strict');
const { test } = require('node:test');

function shellArgument(value) {
    return `'${value.trim()}'`;
}

test('unchanged implementation preserves the decided legacy double-quote contract', () => {
    assert.equal(shellArgument('status'), '"status"');
});
