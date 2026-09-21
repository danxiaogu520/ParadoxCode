// Mission-icon picker for the ParadoxCode webview.
//
// Receives the full sprite catalog as metadata, renders a filterable grid
// (mission icons by default, every sprite on the second tab), and requests
// decoded pixels lazily: an IntersectionObserver batches the names of tiles
// scrolling into view, so opening the picker never decodes a thousand
// textures up front. Clicking a tile posts an `insert` message — the
// extension host writes the name into the editor and closes the panel.

(function () {
    'use strict';

    const vscode = acquireVsCodeApi();

    /** Batch size for image requests; the host clamps to the same bound. */
    const IMAGE_BATCH = 32;
    /** Tiles appended per animation frame while building a large grid. */
    const RENDER_CHUNK = 400;

    // English fallback table; the extension host sends the real dictionary
    // (matching the current UI language) as the first message, before the
    // catalog can paint visible text. Keys must mirror the table in
    // src/webviewI18n.ts (the contract test enforces the pairing).

    const DEFAULT_STRINGS = {
        panelTitle: 'Mission Icons',
        toolbarAria: 'Mission icon picker controls',
        searchPlaceholder: 'Search icons…',
        searchAria: 'Search icons by sprite name',
        tabsAria: 'Sprite scope',
        tabMission: 'Mission icons',
        tabAll: 'All sprites',
        gridAria: 'Sprite tiles',
        hint: 'Click a tile to write it into the focused editor and close this panel.',
        copy: 'copy',
        copyTitle: 'Copy "{0}"',
        frames: '{0} frames',
        countAll: '{0} sprites',
        countFiltered: '{0} / {1} sprites',
        waiting: 'Waiting for the sprite catalog…',
        noMatch: 'No sprites match the current search and tab.',
        originVanilla: 'vanilla',
        originMod: 'mod',
    };

    const strings = { ...DEFAULT_STRINGS };

    function t(key, ...args) {
        const template = strings[key] ?? DEFAULT_STRINGS[key] ?? key;
        return template.replace(/\{(\d+)\}/g, (match, index) => (
            index < args.length ? String(args[index]) : match
        ));
    }

    function applyStaticStrings() {
        document.title = t('panelTitle');
        for (const element of document.querySelectorAll('[data-i18n]')) {
            element.textContent = t(element.dataset.i18n);
        }
        for (const element of document.querySelectorAll('[data-i18n-title]')) {
            element.title = t(element.dataset.i18nTitle);
        }
        for (const element of document.querySelectorAll('[data-i18n-placeholder]')) {
            element.placeholder = t(element.dataset.i18nPlaceholder);
        }
        for (const element of document.querySelectorAll('[data-i18n-aria-label]')) {
            element.setAttribute('aria-label', t(element.dataset.i18nAriaLabel));
        }
    }

    const sprites = [];
    /** Sprite names with an image request in flight -> the awaiting <img>. */
    const pendingImages = new Map();
    let tab = 'mission';
    let query = '';

    const grid = document.getElementById('grid');
    const search = document.getElementById('search');
    const count = document.getElementById('count');
    const messageBox = document.getElementById('message');
    const tabMission = document.getElementById('tab-mission');
    const tabAll = document.getElementById('tab-all');

    const observer = new IntersectionObserver((entries) => {
        const wanted = [];
        for (const entry of entries) {
            observer.unobserve(entry.target);
            if (wanted.length >= IMAGE_BATCH) {
                continue;
            }
            const name = entry.target.dataset.name;
            const img = entry.target.querySelector('img[data-pending="1"]');
            if (name && img && !pendingImages.has(name)) {
                pendingImages.set(name, img);
                wanted.push(name);
            }
        }
        if (wanted.length > 0) {
            vscode.postMessage({ type: 'requestImages', names: wanted });
        }
    }, { root: grid, rootMargin: '200px' });

    function matches(sprite) {
        if (tab === 'mission' && !sprite.mission) {
            return false;
        }
        return query === '' || sprite.name.toLowerCase().includes(query);
    }

    function tile(sprite) {
        const element = document.createElement('div');
        element.className = 'tile';
        element.dataset.name = sprite.name;
        element.setAttribute('role', 'listitem');
        element.tabIndex = 0;
        element.title = `${sprite.name}\n${sprite.textureFile}`;

        const art = document.createElement('div');
        art.className = 'art';
        const img = document.createElement('img');
        // No src until the host streams pixels; a non-empty alt would render
        // as duplicate text beside the name label while waiting.
        img.alt = '';
        img.dataset.pending = '1';
        art.appendChild(img);
        element.appendChild(art);

        const name = document.createElement('div');
        name.className = 'name';
        name.textContent = sprite.name;
        element.appendChild(name);

        const badges = document.createElement('div');
        badges.className = 'badges';
        const origin = document.createElement('span');
        origin.className = 'badge';
        origin.textContent = sprite.origin === 'vanilla' ? t('originVanilla') : t('originMod');
        badges.appendChild(origin);
        if (sprite.frames !== undefined && sprite.frames > 1) {
            const frames = document.createElement('span');
            frames.className = 'badge';
            frames.textContent = t('frames', sprite.frames);
            badges.appendChild(frames);
        }
        const copy = document.createElement('span');
        copy.className = 'copy';
        copy.textContent = t('copy');
        copy.title = t('copyTitle', sprite.name);
        copy.addEventListener('click', (event) => {
            event.stopPropagation();
            vscode.postMessage({ type: 'copy', name: sprite.name });
        });
        badges.appendChild(copy);
        element.appendChild(badges);

        const pick = () => vscode.postMessage({ type: 'insert', name: sprite.name });
        element.addEventListener('click', pick);
        element.addEventListener('keydown', (event) => {
            if (event.key === 'Enter' || event.key === ' ') {
                event.preventDefault();
                pick();
            }
        });
        observer.observe(element);
        return element;
    }

    let renderQueue = null;

    /** Rebuilds the grid in animation-frame chunks so a full-sprite tab
     * (thousands of tiles) never blocks the first paint or the search box. */
    function render() {
        pendingImages.clear();
        grid.replaceChildren();
        observer.disconnect();
        const filtered = sprites.filter(matches);
        count.textContent = filtered.length === sprites.length
            ? t('countAll', filtered.length)
            : t('countFiltered', filtered.length, sprites.length);
        if (filtered.length === 0) {
            showMessage(sprites.length === 0
                ? t('waiting')
                : t('noMatch'));
            return;
        }
        messageBox.hidden = true;
        renderQueue = filtered;
        requestAnimationFrame(drain);
    }

    function drain() {
        if (renderQueue === null) {
            return;
        }
        const fragment = document.createDocumentFragment();
        for (let i = 0; i < RENDER_CHUNK && renderQueue.length > 0; i += 1) {
            fragment.appendChild(tile(renderQueue.shift()));
        }
        grid.appendChild(fragment);
        if (renderQueue.length > 0) {
            requestAnimationFrame(drain);
        } else {
            renderQueue = null;
        }
    }

    function switchTab(next) {
        if (tab === next) {
            return;
        }
        tab = next;
        tabMission.setAttribute('aria-selected', String(tab === 'mission'));
        tabAll.setAttribute('aria-selected', String(tab === 'all'));
        render();
    }

    function showMessage(text) {
        messageBox.textContent = text;
        messageBox.hidden = false;
        grid.replaceChildren();
        observer.disconnect();
    }

    let searchTimer;

    search.addEventListener('input', () => {
        clearTimeout(searchTimer);
        searchTimer = setTimeout(() => {
            query = search.value.trim().toLowerCase();
            render();
        }, 120);
    });

    search.addEventListener('keydown', (event) => {
        if (event.key === 'Enter') {
            const name = grid.querySelector('.tile')?.dataset.name;
            if (name) {
                vscode.postMessage({ type: 'insert', name });
            }
        }
    });

    tabMission.addEventListener('click', () => switchTab('mission'));
    tabAll.addEventListener('click', () => switchTab('all'));

    document.addEventListener('keydown', (event) => {
        if (event.key === 'Escape' && document.activeElement !== search) {
            vscode.postMessage({ type: 'close' });
        }
    });

    window.addEventListener('message', (event) => {
        const data = event.data;
        if (!data || typeof data !== 'object') {
            return;
        }
        if (data.type === 'i18n') {
            Object.assign(strings, data.strings);
            document.documentElement.lang = data.language;
            applyStaticStrings();
            if (sprites.length > 0) {
                render();
            }
            return;
        }
        if (data.type === 'catalog') {
            sprites.length = 0;
            sprites.push(...data.sprites);
            render();
            search.focus();
            return;
        }
        if (data.type === 'images') {
            for (const [name, url] of Object.entries(data.textures)) {
                const img = pendingImages.get(name);
                if (img) {
                    img.src = url;
                    delete img.dataset.pending;
                    pendingImages.delete(name);
                }
            }
            // Tiles rendered after a request went out are waiting unseen;
            // re-observe everything still pending.
            for (const element of grid.querySelectorAll('img[data-pending="1"]')) {
                observer.observe(element.closest('.tile'));
            }
            return;
        }
        if (data.type === 'error') {
            showMessage(data.message);
        }
    });
})();
