import { execFile, spawnSync } from 'node:child_process';
import { appendFile } from 'node:fs/promises';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const publisher = 'paradoxcode';
const extension = 'paradoxcode-vscode';
const extensionId = `${publisher}.${extension}`;
const vsce = fileURLToPath(new URL('../../editors/vscode/node_modules/@vscode/vsce/vsce', import.meta.url));
const run = promisify(execFile);
const { RELEASE_TAG, VSIX_PATH, VSIX_SHA256, GITHUB_STEP_SUMMARY } = process.env;
const version = RELEASE_TAG.slice(1);

async function isPublic() {
    const { stdout } = await run(process.execPath, [vsce, 'show', extensionId, '--json'], {
        timeout: 20_000,
        maxBuffer: 8 * 1024 * 1024,
    });
    const record = JSON.parse(stdout);
    if (record?.publisher?.publisherName?.toLowerCase() !== publisher || record.extensionName !== extension ||
        !Array.isArray(record.versions)) {
        throw new Error(`Unexpected Marketplace metadata for ${extensionId}`);
    }
    return record.versions.some(entry => entry.version === version &&
        !entry.properties?.some(property => property.key === 'Microsoft.VisualStudio.Code.PreRelease'));
}

async function verifyPublication() {
    for (let attempt = 1; attempt <= 12; attempt++) {
        try {
            if (await isPublic()) return;
            console.log(`Marketplace version ${version} is not public yet (${attempt}/12).`);
        } catch (error) {
            console.log(`Marketplace verification attempt ${attempt}/12 failed. ${error.message}`);
        }
        if (attempt < 12) await delay(10_000);
    }
    throw new Error(`Marketplace has not confirmed ${extensionId} ${version}. Inspect the publisher dashboard before retrying.`);
}

let status = 'Already public and verified';
if (!await isPublic()) {
    const publish = spawnSync(process.execPath, [vsce, 'publish', '--oidc', '--packagePath', VSIX_PATH], {
        stdio: 'inherit',
        timeout: 300_000,
    });
    if (publish.error) throw publish.error;
    if (publish.status !== 0) throw new Error(`Marketplace upload failed (${publish.status ?? publish.signal}).`);
    await verifyPublication();
    status = 'Uploaded and verified';
}

const url = `https://marketplace.visualstudio.com/items?itemName=${extensionId}`;
await appendFile(GITHUB_STEP_SUMMARY, [
    '## Visual Studio Marketplace',
    '',
    `- Release tag: \`${RELEASE_TAG}\``,
    `- Extension: [${extensionId}](${url})`,
    `- Public version: \`${version}\``,
    `- GitHub Release VSIX SHA-256: \`${VSIX_SHA256}\``,
    `- Result: ${status}`,
    '',
].join('\n'));
