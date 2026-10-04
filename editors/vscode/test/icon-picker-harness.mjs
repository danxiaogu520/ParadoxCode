// Execute the actual webview and extension-host picker code with controlled
// DOM, observer notifications, and timers. No browser dependencies are needed.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runInNewContext } from 'node:vm';

const extension = join(dirname(fileURLToPath(import.meta.url)), '..');

export function pickerHost(store, postMessage, vscode = {}) {
    const file = join(extension, 'out', 'iconPickerPanel.js');
    const require = createRequire(file);
    const exports = {};
    runInNewContext(readFileSync(file, 'utf8'), {
        exports,
        require(id) {
            if (id === 'vscode') return vscode;
            if (id === './webviewI18n') return {};
            if (id === './previewPanel') return { MissionPreviewPanel: { store: () => store } };
            return require(id);
        },
    }, { filename: file });
    const picker = exports.MissionIconPickerPanel;
    picker.panel = { webview: { postMessage }, dispose() {} };
    return picker;
}

export function pickerWebview() {
    class Element {
        constructor(tag) {
            this.tag = tag;
            this.children = [];
            this.dataset = {};
            this.attributes = {};
            this.listeners = new Map();
            this.hidden = false;
            this.value = '';
        }
        appendChild(child) {
            if (child.tag === 'fragment') {
                for (const member of child.children) this.appendChild(member);
            } else {
                child.parent = this;
                this.children.push(child);
            }
            return child;
        }
        replaceChildren() {
            for (const child of this.children) child.parent = undefined;
            this.children = [];
        }
        contains(element) { return element === this || this.children.some((child) => child.contains(element)); }
        setAttribute(name, value) { this.attributes[name] = value; }
        addEventListener(type, callback) { this.listeners.set(type, callback); }
        emit(type, event = {}) { this.listeners.get(type)?.(event); }
        focus() { document.activeElement = this; }
        matches(selector) {
            return selector === '.tile' ? this.className === 'tile'
                : selector === 'img[data-pending="1"]' && this.tag === 'img' && this.dataset.pending === '1';
        }
        querySelectorAll(selector) {
            return this.children.flatMap((child) => [
                ...(child.matches(selector) ? [child] : []), ...child.querySelectorAll(selector),
            ]);
        }
        querySelector(selector) { return this.querySelectorAll(selector)[0]; }
    }
    const elements = Object.fromEntries(['grid', 'search', 'count', 'message', 'tab-mission', 'tab-all']
        .map((id) => [id, new Element('div')]));
    const document = {
        getElementById: (id) => elements[id],
        createElement: (tag) => new Element(tag),
        createDocumentFragment: () => new Element('fragment'),
        addEventListener() {}, querySelectorAll: () => [], documentElement: {},
    };
    const frames = new Map();
    const timers = new Map();
    const sent = [];
    let sequence = 0;
    let receive;
    let observer;
    const file = join(extension, 'media', 'icon-picker.js');
    runInNewContext(readFileSync(file, 'utf8'), {
        document,
        window: { addEventListener: (_, callback) => { receive = callback; } },
        acquireVsCodeApi: () => ({ postMessage: (message) => sent.push(structuredClone(message)) }),
        requestAnimationFrame: (callback) => { frames.set(++sequence, callback); return sequence; },
        cancelAnimationFrame: (id) => frames.delete(id),
        setTimeout: (callback) => { timers.set(++sequence, callback); return sequence; },
        clearTimeout: (id) => timers.delete(id),
        IntersectionObserver: class {
            constructor(callback) { this.callback = callback; this.observed = new Set(); observer = this; }
            observe(element) { this.observed.add(element); }
            unobserve(element) { this.observed.delete(element); }
            disconnect() { this.observed.clear(); }
        },
    }, { filename: file });
    return {
        elements, sent, observer, frames, timers,
        receive: (data) => receive({ data }),
        frame() {
            const callbacks = [...frames.values()];
            frames.clear();
            for (const callback of callbacks) callback();
        },
        flushFrames() {
            for (let i = 0; i < 100 && frames.size > 0; i += 1) this.frame();
            assert.equal(frames.size, 0, 'rendering must finish');
        },
        search(value) { elements.search.value = value; elements.search.emit('input'); },
        debounce() {
            const callbacks = [...timers.values()];
            timers.clear();
            for (const callback of callbacks) callback();
        },
        intersect(visible = () => true, targets = [...observer.observed]) {
            observer.callback(targets.map((target, index) => ({ target, isIntersecting: visible(index) })));
        },
        tiles: () => elements.grid.children.map((tile) => tile.dataset.name),
    };
}

export function sprite(name, mission = true) {
    return { name, mission, textureFile: `${name}.dds`, origin: 'mod' };
}
