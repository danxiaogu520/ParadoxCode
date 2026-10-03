// Execute the extension's actual PATH lookup implementation.
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { delimiter, join } from 'node:path';
import { findExecutableOnPath } from '../out/serverPath.js';
const root = mkdtempSync(join(tmpdir(), 'pdc-path-test-'));
const previous = process.env.PATH;
try {
    const name = process.platform === 'win32' ? 'pdc-fake.exe' : 'pdc-fake';
    writeFileSync(join(root, name), '');
    process.env.PATH = root + (previous ? delimiter + previous : '');
    assert.equal(findExecutableOnPath('pdc-fake'), join(root, name));
    assert.equal(findExecutableOnPath('definitely-missing-paradoxcode'), undefined);
} finally {
    if (previous === undefined) delete process.env.PATH;
    else process.env.PATH = previous;
    rmSync(root, { recursive: true, force: true });
}
