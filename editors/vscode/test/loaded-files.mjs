import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';

const manifest = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'));
const compiled = readFileSync(new URL('../out/fileExplorer.js', import.meta.url), 'utf8');

function explorer() {
    const view = {
        visible: false,
        onDidChangeVisibility(listener) { this.notify = listener; },
        setVisible(visible) { this.visible = visible; this.notify({ visible }); },
    };
    const vscode = {
        window: { createTreeView: () => view },
        EventEmitter: class { fire() {} },
    };
    const exports = {};
    runInNewContext(compiled, { exports, require(id) { assert.equal(id, 'vscode'); return vscode; } });
    return { view, provider: new exports.LoadedFilesProvider() };
}

function server() {
    const requests = [];
    return {
        requests,
        sendRequest(method) {
            assert.equal(method, 'pdc/workspaceFiles');
            const request = Promise.withResolvers();
            requests.push(request);
            return request.promise;
        },
    };
}

function listing(id) {
    return { revision: id, roots: [{ id }], files: [{ id, rootId: id }] };
}

test('a collapsed Loaded Files view waits for demand and reuses its listing', async () => {
    const contribution = manifest.contributes.views.explorer.find(view => view.id === 'paradoxcode.loadedFiles');
    assert.equal(contribution.visibility, 'collapsed');
    const { view, provider } = explorer();
    const client = server();
    provider.setClient(client);
    await provider.refresh();
    assert.equal(provider.getChildren().length, 0);
    assert.equal(client.requests.length, 0);

    view.setVisible(true);
    assert.equal(client.requests.length, 1);
    view.setVisible(false);
    view.setVisible(true);
    const refresh = provider.refresh();
    assert.equal(client.requests.length, 1);
    client.requests[0].resolve(listing(1));
    await refresh;
    const [root] = provider.getChildren();
    assert.equal(provider.getChildren(root)[0].file.id, 1);
    view.setVisible(false);
    view.setVisible(true);
    assert.equal(client.requests.length, 1);

    view.setVisible(false);
    await provider.refresh();
    assert.equal(client.requests.length, 1);
    view.setVisible(true);
    assert.equal(client.requests.length, 2);
    const pending = provider.refresh();
    view.setVisible(false);
    await provider.refresh();
    client.requests[1].resolve(listing(2));
    await pending;
    view.setVisible(true);
    const latest = provider.refresh();
    assert.equal(client.requests.length, 3);
    client.requests[2].resolve(listing(3));
    await latest;
    assert.equal(provider.getChildren()[0].root.id, 3);
});

test('an expanded Loaded Files view waits for readiness and ignores superseded replies', async () => {
    const { view, provider } = explorer();
    const first = server();
    const next = server();
    view.setVisible(true);
    await provider.refresh();
    provider.setClient(first);
    assert.equal(first.requests.length, 1);
    const oldLoad = provider.refresh();
    provider.setClient(undefined);
    provider.setClient(next);
    provider.setClient(next);
    assert.equal(next.requests.length, 1);
    const newLoad = provider.refresh();
    next.requests[0].resolve(listing(2));
    await newLoad;
    first.requests[0].resolve(listing(1));
    await oldLoad;
    assert.equal(provider.getChildren()[0].root.id, 2);

    const failed = provider.refresh();
    next.requests[1].reject(new Error('unavailable'));
    await failed;
    assert.equal(provider.getChildren().length, 0);
    const retry = provider.refresh();
    next.requests[2].resolve(listing(3));
    await retry;
    assert.equal(provider.getChildren()[0].root.id, 3);
});
