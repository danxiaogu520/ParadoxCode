// Mission-tree canvas renderer for the ParadoxCode preview webview.
//
// Consumes the `pdc/missionPreview` wire contract: world-space node/group
// positions, arrow glyph placements, and UTF-16 source ranges for jump-to-source,
// mission-scoped diagnostics. All geometry is presentation-only; the server
// owns layout semantics — including every arrow segment (the renderer maps
// glyph kinds to drawings and never recomputes layout).

(function () {
    'use strict';

    const vscode = acquireVsCodeApi();

    const NODE_WIDTH = 104;
    const NODE_HEIGHT = 122;

    // Arrow glyph world metrics, mirroring `game::eu4::mission::geometry`:
    // horizontal tiles span one flush column width; vertical fallback tiles
    // span a node bottom edge to the end-offset level.
    const ARROW_TILE_HEIGHT = 20;
    const ARROW_TILE_WIDTH = NODE_WIDTH;

    // EMT-style node layout inside a mission cell (world pixels): the mission
    // icon sits under the frame at (22, 20) in its 59x63 slot, the title is
    // centered within the frame's lower 96px slot.
    const EMT_ICON_X = 22;
    const EMT_ICON_Y = 20;
    const EMT_TITLE_X = 8;
    const EMT_TITLE_Y = 84;
    const EMT_TITLE_WIDTH = 96;

    function readColors() {
        const styles = getComputedStyle(document.body);
        const themeColor = (name, fallback) => styles.getPropertyValue(name).trim() || fallback;
        return {
            border: themeColor('--vscode-editorIndentGuide-background', '#39414c'),
            borderSelected: themeColor('--vscode-focusBorder', '#4d7cfe'),
            error: themeColor('--vscode-editorError-foreground', '#d55c5c'),
            warning: themeColor('--vscode-editorWarning-foreground', '#d9a13b'),
            text: styles.color || '#d4d8dd',
            dim: themeColor('--vscode-descriptionForeground', '#8a919c'),
            groupBg: themeColor('--vscode-editorWidget-background', 'rgba(42, 50, 64, 0.85)'),
            arrow: themeColor('--vscode-charts-blue', '#4d7cfe'),
            card: themeColor('--vscode-editorWidget-background', '#23282f'),
            errorBg: themeColor('--vscode-inputValidation-errorBackground', 'rgba(58, 31, 31, 0.9)'),
            externalBg: themeColor('--vscode-editorHoverWidget-background', 'rgba(27, 30, 35, 0.8)'),
            texturedText: themeColor('--vscode-editor-foreground', '#ffffff'),
            canvas: 'transparent',
        };
    }

    let COLORS = readColors();
    let FONT_FAMILY = getComputedStyle(document.body).fontFamily || 'sans-serif';

    const canvas = document.getElementById('tree');
    const status = document.getElementById('status');
    const tooltip = document.getElementById('tooltip');
    const seriesEntries = document.getElementById('series-entries');
    const seriesSummary = document.getElementById('series-summary');
    const searchInput = document.getElementById('search');
    const searchResults = document.getElementById('search-results');
    const fileBadge = document.getElementById('file');
    const ctx = canvas.getContext('2d');

    // --- localisation ---------------------------------------------------------
    //
    // English fallback table; the extension host sends the real dictionary
    // (matching the current UI language) as the first message, before any
    // data message can paint visible text. Keys must mirror the table in
    // src/webviewI18n.ts (the contract test enforces the pairing).

    const DEFAULT_STRINGS = {
        panelTitle: 'Mission Tree Preview',
        toolbarAria: 'Mission preview controls',
        canvasAria: 'Mission tree preview',
        fit: 'Fit',
        fitTitle: 'Fit mission tree (F)',
        zoomOutTitle: 'Zoom out (-)',
        zoomInTitle: 'Zoom in (+)',
        searchPlaceholder: 'Search missions…',
        searchAria: 'Search missions by title or id',
        resultsAria: 'Matching missions',
        series: 'Series',
        seriesCount: 'Series ({0}/{1})',
        seriesAria: 'Mission series visibility',
        all: 'All',
        none: 'None',
        seriesHiddenSuffix: ' · series hidden',
        slot: 'Slot {0}',
        missionAria: 'Mission {0}',
        noPreview: 'No preview available.',
        statusMissions: '{0} missions',
        errorSingular: '{0} error',
        errorPlural: '{0} errors',
        warningSingular: '{0} warning',
        warningPlural: '{0} warnings',
        flagError: 'error',
        flagWarning: 'warning',
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

    let preview = null;
    let hovered = null; // { kind: 'node'|'group', index, rect }
    let pan = { x: 0, y: 0 };
    let zoom = 1;
    let press = null; // { clientX, clientY, hit, panning, panX, panY }
    let keyboardIndex = -1;
    let options = {
        zoomSensitivity: 1,
        showTextures: true,
        showExternalPrerequisites: true,
        showDiagnostics: true,
        gameFonts: true,
    };
    let drawPending = false;
    let groupWidths = new WeakMap();
    let externalWidths = new Map();
    let externalByNode = new Map();

    // Pointer and wheel events can arrive much faster than the browser can
    // paint. Coalescing them into one frame keeps a large tree from being
    // rendered once per event while preserving the latest pan/zoom/hover
    // state.
    function scheduleDraw() {
        if (drawPending) {
            return;
        }
        drawPending = true;
        window.requestAnimationFrame(() => {
            drawPending = false;
            drawFrame();
        });
    }

    function refreshTheme() {
        COLORS = readColors();
        FONT_FAMILY = getComputedStyle(document.body).fontFamily || 'sans-serif';
        groupWidths = new WeakMap();
        externalWidths.clear();
    }

    // VS Code changes the webview theme by updating body attributes. Refresh
    // the small theme cache only when that happens, rather than on every paint.
    if (typeof MutationObserver !== 'undefined') {
        new MutationObserver(() => {
            refreshTheme();
            scheduleDraw();
        }).observe(document.body, { attributes: true, attributeFilter: ['class', 'style'] });
    }

    function resize() {
        refreshTheme();
        const dpr = window.devicePixelRatio || 1;
        canvas.width = Math.round(canvas.clientWidth * dpr);
        canvas.height = Math.round(canvas.clientHeight * dpr);
        ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        scheduleDraw();
    }

    function toScreen(wx, wy) {
        return { x: wx * zoom + pan.x, y: wy * zoom + pan.y };
    }

    function toWorld(sx, sy) {
        return { x: (sx - pan.x) / zoom, y: (sy - pan.y) / zoom };
    }

    function worldRectVisible(x, y, width, height, padding = 32) {
        const left = (-pan.x - padding) / zoom;
        const top = (-pan.y - padding) / zoom;
        const right = (canvas.clientWidth - pan.x + padding) / zoom;
        const bottom = (canvas.clientHeight - pan.y + padding) / zoom;
        return x + width >= left && x <= right && y + height >= top && y <= bottom;
    }

    function fitView() {
        if (!preview || (preview.nodes.length === 0 && preview.groups.length === 0)) {
            pan = { x: 24, y: 24 };
            zoom = 1;
            return;
        }
        let minX = Infinity;
        let minY = Infinity;
        let maxX = -Infinity;
        let maxY = -Infinity;
        for (const group of preview.groups) {
            if (!isTreeVisible(group.tree)) {
                continue;
            }
            minX = Math.min(minX, group.x);
            minY = Math.min(minY, group.y);
            maxX = Math.max(maxX, group.x + 200);
            maxY = Math.max(maxY, group.y + 18);
        }
        for (const node of preview.nodes) {
            if (!isTreeVisible(node.tree)) {
                continue;
            }
            minX = Math.min(minX, node.x);
            minY = Math.min(minY, node.y);
            maxX = Math.max(maxX, node.x + NODE_WIDTH);
            maxY = Math.max(maxY, node.y + NODE_HEIGHT);
        }
        if (!Number.isFinite(minX)) {
            pan = { x: 24, y: 24 };
            zoom = 1;
            return;
        }
        const margin = 48;
        const width = maxX - minX;
        const height = maxY - minY;
        const zoomX = (canvas.clientWidth - margin * 2) / width;
        const zoomY = (canvas.clientHeight - margin * 2) / height;
        zoom = Math.min(zoomX, zoomY, 1.5);
        pan = {
            x: (canvas.clientWidth - width * zoom) / 2 - minX * zoom,
            y: (canvas.clientHeight - height * zoom) / 2 - minY * zoom,
        };
    }

    // Session viewport memory: refreshing the same document must never move
    // the view, and switching between mission files remembers each document's
    // pan/zoom for the lifetime of the webview. No persistence beyond that —
    // a rebuilt page (window reload, webview discard) fits, which is expected.
    let lastDocumentUri = null;
    const viewportsByDocument = new Map();

    // Series visibility memory, same lifecycle as the viewport map: per
    // document, keyed by tree id (the series block name) so edits that add or
    // remove series keep the hidden set pointing at the right ones. Hidden
    // series keep their canvas position — the gap stays.
    const hiddenByDocument = new Map();
    let hiddenTreeIds = new Set();
    let treeIds = []; // tree index -> series id, rebuilt per payload

    function isTreeVisible(treeIndex) {
        return !hiddenTreeIds.has(treeIds[treeIndex]);
    }

    // Rebuild the index -> series id table from the payload and drop hidden
    // ids that no longer exist (their series were deleted or renamed).
    function syncSeriesState() {
        treeIds = [];
        for (const group of preview.groups) {
            treeIds[group.tree] = group.label;
        }
        const existing = new Set(treeIds.filter(Boolean));
        hiddenTreeIds = new Set([...hiddenTreeIds].filter((id) => existing.has(id)));
        hiddenByDocument.set(lastDocumentUri, hiddenTreeIds);
    }

    // Snapshot the outgoing document's viewport and adopt the incoming one
    // (remembered view, or a fit for a first look). Call after `setPreview`
    // so `fitView` can measure the new payload.
    function switchDocument(documentUri) {
        if (documentUri === lastDocumentUri) {
            return;
        }
        if (lastDocumentUri !== null) {
            viewportsByDocument.set(lastDocumentUri, { x: pan.x, y: pan.y, zoom });
            hiddenByDocument.set(lastDocumentUri, hiddenTreeIds);
        }
        lastDocumentUri = documentUri;
        hiddenTreeIds = new Set(hiddenByDocument.get(documentUri) || []);
        const saved = viewportsByDocument.get(documentUri);
        if (saved) {
            pan = { x: saved.x, y: saved.y };
            zoom = saved.zoom;
        } else {
            fitView();
        }
    }

    // Zooms around the current view center (keyboard and toolbar zoom have no
    // cursor anchor; the wheel handler keeps its own cursor-anchored math).
    function zoomBy(factor) {
        const sx = canvas.clientWidth / 2;
        const sy = canvas.clientHeight / 2;
        const world = toWorld(sx, sy);
        zoom = Math.min(2.5, Math.max(0.35, zoom * factor));
        pan.x = sx - world.x * zoom;
        pan.y = sy - world.y * zoom;
        scheduleDraw();
    }

    function roundRect(x, y, w, h, r) {
        ctx.beginPath();
        ctx.moveTo(x + r, y);
        ctx.arcTo(x + w, y, x + w, y + h, r);
        ctx.arcTo(x + w, y + h, x, y + h, r);
        ctx.arcTo(x, y + h, x, y, r);
        ctx.arcTo(x, y, x + w, y, r);
        ctx.closePath();
    }

    function nodeColor(node) {
        if (options.showDiagnostics && node.hasError) {
            return COLORS.error;
        }
        if (options.showDiagnostics && node.hasWarning) {
            return COLORS.warning;
        }
        return COLORS.border;
    }

    function groupWidth(group) {
        let width = groupWidths.get(group);
        if (width === undefined) {
            ctx.font = `11px ${FONT_FAMILY}`;
            width = Math.max(ctx.measureText(group.label).width + 16, 60);
            groupWidths.set(group, width);
        }
        return width;
    }

    function externalLabelWidth(label) {
        let width = externalWidths.get(label);
        if (width === undefined) {
            ctx.font = `10px ${FONT_FAMILY}`;
            width = ctx.measureText(label).width + 8;
            externalWidths.set(label, width);
        }
        return width;
    }

    function nodeKey(node) {
        return `${node.tree}:${node.mission}`;
    }

    function setPreview(next) {
        preview = next;
        externalByNode = new Map();
        for (const external of next.external || []) {
            const key = `${external.tree}:${external.mission}`;
            const entries = externalByNode.get(key);
            if (entries) {
                entries.push(external);
            } else {
                externalByNode.set(key, [external]);
            }
        }
        groupWidths = new WeakMap();
        externalWidths.clear();
    }

    function nodeAt(sx, sy) {
        if (!preview) {
            return null;
        }
        for (let i = preview.nodes.length - 1; i >= 0; i -= 1) {
            const node = preview.nodes[i];
            const pos = toScreen(node.x, node.y);
            const w = NODE_WIDTH * zoom;
            const h = NODE_HEIGHT * zoom;
            if (sx >= pos.x && sx <= pos.x + w && sy >= pos.y && sy <= pos.y + h) {
                return { kind: 'node', index: i, node, rect: { x: pos.x, y: pos.y, w, h } };
            }
        }
        for (let i = 0; i < preview.groups.length; i += 1) {
            const group = preview.groups[i];
            const pos = toScreen(group.x, group.y);
            const w = groupWidth(group) * zoom;
            const h = 20 * zoom;
            if (sx >= pos.x && sx <= pos.x + w && sy >= pos.y && sy <= pos.y + h) {
                return { kind: 'group', index: i, node: group, rect: { x: pos.x, y: pos.y, w, h } };
            }
        }
        return null;
    }

    // --- game textures -----------------------------------------------------

    const textureImages = new Map(); // sprite name -> HTMLImageElement
    const failedTextures = new Set(); // sprite names that failed to decode
    const textureUrls = {}; // sprite name -> data URL, delivered via 'assets'

    // Merges a batch of decoded sprite data URLs (and, once per font
    // generation, the game bitmap fonts). Sprites arrive separately from the
    // per-keystroke preview payload (which is pure text); a name whose URL
    // changed on disk drops its cached image so the new pixels reload.
    function setAssets(message) {
        let changed = false;
        for (const [name, url] of Object.entries(message.textures || {})) {
            if (textureUrls[name] === url) {
                continue;
            }
            changed = true;
            textureUrls[name] = url;
            textureImages.delete(name);
            failedTextures.delete(name);
        }
        if (message.fonts) {
            setFonts(message.fonts);
            changed = true;
        }
        if (changed) {
            scheduleDraw();
        }
    }

    // Returns the loaded image for a sprite, or null when no asset has been
    // delivered for it. Images decode asynchronously; drawing happens on `load`.
    // A failed decode is cached as a miss so the schematic fallback stays
    // reachable (a failed `Image` still reports `complete === true`).
    function textureImage(name) {
        if (!options.showTextures || !name || failedTextures.has(name)) {
            return null;
        }
        const url = textureUrls[name];
        if (!url) {
            return null;
        }
        let image = textureImages.get(name);
        if (!image) {
            image = new Image();
            image.onload = () => scheduleDraw();
            image.onerror = () => {
                failedTextures.add(name);
                textureImages.delete(name);
                scheduleDraw();
            };
            image.src = url;
            textureImages.set(name, image);
        }
        return image.complete && image.naturalWidth > 0 ? image : null;
    }

    // Draws a texture at world coordinates at its natural pixel size.
    function drawImageWorld(image, wx, wy) {
        if (!image) {
            return;
        }
        ctx.drawImage(
            image,
            wx * zoom + pan.x,
            wy * zoom + pan.y,
            image.width * zoom,
            image.height * zoom,
        );
    }

    // --- game fonts -----------------------------------------------------------

    // § localisation colour parser, provided by media/loc-format.js which is
    // loaded before this script.
    const parseLocFormat = window.LocFormat.parseLocFormat;

    // Game bitmap fonts delivered via the 'assets' message, keyed by id.
    // Rows are the compact wire format: chars[i] =
    // [id, x, y, w, h, xOffset, yOffset, xAdvance]; kernings[i] =
    // [first, second, amount].
    const fontBook = { english: null, chinese: null };

    function setFonts(fonts) {
        for (const id of ['english', 'chinese']) {
            const payload = fonts && fonts[id];
            if (!payload) {
                fontBook[id] = null;
                continue;
            }
            const chars = new Map();
            for (const row of payload.chars) {
                chars.set(row[0], {
                    x: row[1],
                    y: row[2],
                    w: row[3],
                    h: row[4],
                    xo: row[5],
                    yo: row[6],
                    adv: row[7],
                });
            }
            const kernings = new Map();
            for (const row of payload.kernings || []) {
                kernings.set(`${row[0]},${row[1]}`, row[2]);
            }
            const atlas = new Image();
            atlas.onload = () => scheduleDraw();
            atlas.onerror = () => {
                fontBook[id] = null;
                scheduleDraw();
            };
            atlas.src = payload.atlasUrl;
            fontBook[id] = { id, lineHeight: payload.lineHeight, base: payload.base, atlas, chars, kernings };
        }
        glyphTintCache.clear();
        scheduleDraw();
    }

    function fontReady(font) {
        return !!font && font.atlas.complete && font.atlas.naturalWidth > 0;
    }

    function fontKerning(font, first, second) {
        return first ? font.kernings.get(`${first},${second}`) || 0 : 0;
    }

    // CJK detection drives both font choice and per-character wrapping:
    // Chinese replace-file localisation carries l_english headers, so the
    // language tag alone cannot be trusted — the content decides.
    const CJK_RE = /[\u2e80-\u9fff\uf900-\ufaff\ufe30-\ufe4f\uff00-\uffef\u3000-\u303f]/;

    function titleFont(title) {
        const chinese = !!title && (title.language === 'simp_chinese' || CJK_RE.test(title.value));
        const primary = fontBook[chinese ? 'chinese' : 'english'];
        if (fontReady(primary)) {
            return primary;
        }
        const secondary = fontBook[chinese ? 'english' : 'chinese'];
        return fontReady(secondary) ? secondary : null;
    }

    // Glyph resolution order for one codepoint: the primary font, then the
    // other font (mixed-script titles), else null for the system fallback.
    function resolveGlyph(layout, codePoint) {
        if (layout.font) {
            const glyph = layout.font.chars.get(codePoint);
            if (glyph && fontReady(layout.font)) {
                return { font: layout.font, glyph };
            }
            const other = layout.font.id === 'english' ? fontBook.chinese : fontBook.english;
            const alt = other && other.chars.get(codePoint);
            if (alt && fontReady(other)) {
                return { font: other, glyph: alt };
            }
        }
        return null;
    }

    // Per-glyph tint cache: the font atlases are white-on-transparent, so a
    // coloured § run blits through a tiny canvas tinted with 'source-in'.
    // White draws straight from the atlas and skips the cache.
    const glyphTintCache = new Map();

    function glyphSource(font, glyph, codePoint, color) {
        if (color === '#ffffff') {
            return { source: font.atlas, sx: glyph.x, sy: glyph.y };
        }
        const key = `${font.id}|${codePoint}|${color}`;
        let canvas = glyphTintCache.get(key);
        if (!canvas) {
            canvas = document.createElement('canvas');
            canvas.width = Math.max(1, glyph.w);
            canvas.height = Math.max(1, glyph.h);
            const g = canvas.getContext('2d');
            g.drawImage(font.atlas, glyph.x, glyph.y, glyph.w, glyph.h, 0, 0, glyph.w, glyph.h);
            g.globalCompositeOperation = 'source-in';
            g.fillStyle = color;
            g.fillRect(0, 0, canvas.width, canvas.height);
            if (glyphTintCache.size > 8192) {
                glyphTintCache.clear(); // hard cap; titles revisit few glyphs
            }
            glyphTintCache.set(key, canvas);
        }
        return { source: canvas, sx: 0, sy: 0 };
    }

    // Advance of one character in screen pixels, kerned against `prev` when
    // the primary font owns both; the system font estimates the rest.
    function charAdvance(layout, codePoint, prev) {
        const hit = resolveGlyph(layout, codePoint);
        if (hit) {
            const kerning = hit.font === layout.font ? fontKerning(hit.font, prev, codePoint) : 0;
            return (hit.glyph.adv + kerning) * layout.zoom;
        }
        ctx.font = layout.sysFont;
        return ctx.measureText(String.fromCodePoint(codePoint)).width;
    }

    function measureStyled(layout, text) {
        let width = 0;
        let prev = 0;
        for (const ch of text) {
            const codePoint = ch.codePointAt(0);
            width += charAdvance(layout, codePoint, prev);
            prev = codePoint;
        }
        return width;
    }

    // Splits styled runs into wrap tokens: CJK characters break individually,
    // Latin words stay whole and wrap at word boundaries — whitespace opens a
    // new token (leading the word it precedes, stripped when that word starts
    // a line).
    function tokenizeStyled(line) {
        const tokens = [];
        let word = null;
        for (const run of line) {
            for (const ch of run.text) {
                if (CJK_RE.test(ch)) {
                    if (word) {
                        tokens.push(word);
                        word = null;
                    }
                    tokens.push({ text: ch, color: run.color });
                } else if (/\s/.test(ch)) {
                    if (word && word.color === run.color && /^\s+$/.test(word.text)) {
                        word.text += ch;
                    } else {
                        if (word) {
                            tokens.push(word);
                        }
                        word = { text: ch, color: run.color };
                    }
                } else if (word && word.color === run.color) {
                    word.text += ch;
                } else {
                    if (word) {
                        tokens.push(word);
                    }
                    word = { text: ch, color: run.color };
                }
            }
        }
        if (word) {
            tokens.push(word);
        }
        return tokens;
    }

    // Greedy wrap of one styled line at maxWidth; a token moved to a new line
    // never carries leading spaces with it.
    function wrapStyledLine(line, maxWidth, layout) {
        const lines = [];
        let current = [];
        let width = 0;
        for (let token of tokenizeStyled(line)) {
            if (current.length > 0 && width + measureStyled(layout, token.text) > maxWidth) {
                lines.push(current);
                current = [];
                width = 0;
                const trimmed = token.text.replace(/^\s+/, '');
                token = { text: trimmed, color: token.color };
            } else if (current.length === 0) {
                const trimmed = token.text.replace(/^\s+/, '');
                token = { text: trimmed, color: token.color };
            }
            if (!token.text) {
                continue;
            }
            current.push(token);
            width += measureStyled(layout, token.text);
        }
        if (current.length > 0 || lines.length === 0) {
            lines.push(current);
        }
        return lines;
    }

    // Splits parsed segments into explicit lines at newline boundaries.
    function segmentLines(segments) {
        const lines = [[]];
        for (const segment of segments) {
            const parts = segment.text.split('\n');
            parts.forEach((part, index) => {
                if (index > 0) {
                    lines.push([]);
                }
                if (part) {
                    lines[lines.length - 1].push({ text: part, color: segment.color });
                }
            });
        }
        return lines;
    }

    // Full title pipeline: parse § colour codes, honor newlines, wrap.
    function styledLines(text, maxWidth, layout) {
        const result = [];
        for (const line of segmentLines(parseLocFormat(text).segments)) {
            result.push(...wrapStyledLine(line, maxWidth, layout));
        }
        return result;
    }

    function lineWidth(tokens, layout) {
        let width = 0;
        for (const token of tokens) {
            width += measureStyled(layout, token.text);
        }
        return width;
    }

    // Draws one wrapped line left-to-right from x. Glyphs blit from the game
    // font (tinted per § colour, baseline-aligned when the other font owns
    // the glyph); characters missing from both fonts fall back to a
    // system-font run. `layout.baseline` is the baseline offset from line top.
    function drawStyledLine(tokens, x, lineTop, layout) {
        let pen = x;
        let prev = 0;
        let run = null; // pending system-font run { text, color }
        const flushRun = () => {
            if (!run || !run.text) {
                return;
            }
            ctx.font = layout.sysFont;
            ctx.textAlign = 'left';
            ctx.textBaseline = 'alphabetic';
            ctx.fillStyle = run.color ?? layout.defaultColor;
            ctx.fillText(run.text, pen, lineTop + layout.baseline);
            pen += ctx.measureText(run.text).width;
            run = null;
        };
        for (const token of tokens) {
            const color = token.color ?? null;
            for (const ch of token.text) {
                const codePoint = ch.codePointAt(0);
                const hit = resolveGlyph(layout, codePoint);
                if (hit) {
                    flushRun();
                    const kerning = hit.font === layout.font
                        ? fontKerning(hit.font, prev, codePoint) * layout.zoom
                        : 0;
                    const baseline = lineTop + layout.baseline;
                    const top = baseline - hit.font.base * layout.zoom;
                    const { source, sx, sy } = glyphSource(hit.font, hit.glyph, codePoint, token.color ?? layout.defaultColor);
                    ctx.drawImage(
                        source,
                        sx,
                        sy,
                        hit.glyph.w,
                        hit.glyph.h,
                        pen + hit.glyph.xo * layout.zoom + kerning,
                        top + hit.glyph.yo * layout.zoom,
                        hit.glyph.w * layout.zoom,
                        hit.glyph.h * layout.zoom,
                    );
                    pen += hit.glyph.adv * layout.zoom + kerning;
                } else if (run && run.color === color) {
                    run.text += ch;
                } else {
                    flushRun();
                    run = { text: ch, color };
                }
                prev = codePoint;
            }
        }
        flushRun();
    }

    function drawArrowHead(pos, direction) {
        const s = 7 * zoom;
        const w = 4 * zoom;
        const tip = 6 * zoom;
        ctx.fillStyle = COLORS.arrow;
        ctx.beginPath();
        if (direction === 'down') {
            ctx.moveTo(pos.x - s, pos.y - w);
            ctx.lineTo(pos.x + s, pos.y - w);
            ctx.lineTo(pos.x, pos.y + tip);
        } else if (direction === 'right') {
            ctx.moveTo(pos.x - w, pos.y - s);
            ctx.lineTo(pos.x - w, pos.y + s);
            ctx.lineTo(pos.x + tip, pos.y);
        } else {
            ctx.moveTo(pos.x + w, pos.y - s);
            ctx.lineTo(pos.x + w, pos.y + s);
            ctx.lineTo(pos.x - tip, pos.y);
        }
        ctx.closePath();
        ctx.fill();
    }

    // Runs a vertical stroke between two glyph anchors (world coordinates).
    function strokeVertical(from, to) {
        const minY = Math.min(from.y, to.y);
        const maxY = Math.max(from.y, to.y);
        if (!worldRectVisible(from.x - 2, minY, 4, maxY - minY)) {
            return;
        }
        const a = toScreen(from.x, from.y);
        const b = toScreen(to.x, to.y);
        ctx.strokeStyle = COLORS.arrow;
        ctx.lineWidth = 2.5;
        ctx.beginPath();
        ctx.moveTo(a.x, a.y);
        ctx.lineTo(b.x, b.y);
        ctx.stroke();
    }

    // Draws one server-placed arrow segment: its game texture when the asset
    // pipeline delivered one (EMT-style tile assembly), or a schematic stroke.
    function drawArrowSegment(segment, image = textureImage(segment.texture)) {
        const width = segment.glyph === 'horizontalSkipSlot' ? ARROW_TILE_WIDTH : 14;
        const height = segment.glyph === 'verticalTile' || segment.glyph === 'verticalSkipTier'
            ? ARROW_TILE_HEIGHT
            : 14;
        if (!worldRectVisible(segment.x, segment.y, width, height)) {
            return;
        }
        if (image) {
            drawImageWorld(image, segment.x, segment.y);
            return;
        }
        const pos = toScreen(segment.x, segment.y);
        ctx.strokeStyle = COLORS.arrow;
        ctx.lineWidth = 2.5;
        switch (segment.glyph) {
            case 'verticalTile':
            case 'verticalSkipTier': {
                ctx.beginPath();
                ctx.moveTo(pos.x, pos.y);
                ctx.lineTo(pos.x, pos.y + ARROW_TILE_HEIGHT * zoom);
                ctx.stroke();
                break;
            }
            case 'horizontalSkipSlot': {
                ctx.beginPath();
                ctx.moveTo(pos.x, pos.y);
                ctx.lineTo(pos.x + ARROW_TILE_WIDTH * zoom, pos.y);
                ctx.stroke();
                break;
            }
            case 'end':
                drawArrowHead(pos, 'down');
                break;
            case 'rightOut':
            case 'rightIn':
                drawArrowHead(pos, 'right');
                break;
            case 'leftOut':
            case 'leftIn':
                drawArrowHead(pos, 'left');
                break;
        }
    }

    // Draws all arrow segments. When textures are available every glyph is
    // drawn as its tile (vertical runs become continuous automatically);
    // without textures, vertical runs chain into one continuous stroke.
    function drawArrows() {
        if (!preview) {
            return;
        }
        let chain = null; // { x, y } of the previous vertical glyph in this run
        for (const segment of preview.arrows) {
            // A run disappears when either endpoint's series is hidden — the
            // dependent (`tree`) or the prerequisite (`from`) — so no arrow
            // ever dangles into or out of empty space.
            if (!isTreeVisible(segment.tree) || !isTreeVisible(segment.from)) {
                chain = null;
                continue;
            }
            const image = textureImage(segment.texture);
            if (image) {
                chain = null;
                drawArrowSegment(segment, image);
            } else if (segment.glyph === 'verticalTile' || segment.glyph === 'verticalSkipTier') {
                if (chain) {
                    strokeVertical(chain, segment);
                }
                chain = { x: segment.x, y: segment.y };
            } else if (segment.glyph === 'end') {
                if (chain) {
                    strokeVertical(chain, segment);
                }
                chain = null;
                drawArrowSegment(segment, null);
            } else {
                chain = null; // heads and horizontal tiles end any vertical chain
                drawArrowSegment(segment, null);
            }
        }
    }

    function drawGroups() {
        if (!preview) {
            return;
        }
        ctx.font = `11px ${FONT_FAMILY}`;
        for (const group of preview.groups) {
            if (!isTreeVisible(group.tree)) {
                continue;
            }
            const width = groupWidth(group);
            if (!worldRectVisible(group.x, group.y, width, 18)) {
                continue;
            }
            const pos = toScreen(group.x, group.y);
            const hoveredGroup = hovered && hovered.kind === 'group' && hovered.node === group;
            ctx.fillStyle = COLORS.groupBg;
            roundRect(pos.x, pos.y, width, 18, 4);
            ctx.fill();
            ctx.strokeStyle = hoveredGroup ? COLORS.borderSelected : COLORS.border;
            ctx.lineWidth = 1;
            ctx.stroke();
            ctx.fillStyle = COLORS.dim;
            ctx.textAlign = 'left';
            ctx.textBaseline = 'middle';
            ctx.fillText(group.label, pos.x + 8, pos.y + 10);
        }
    }

    function drawExternal(node, pos) {
        if (!preview || !options.showExternalPrerequisites) {
            return;
        }
        const external = externalByNode.get(nodeKey(node));
        if (!external) {
            return;
        }
        ctx.font = `10px ${FONT_FAMILY}`;
        for (const ext of external) {
            const label = `↥ ${ext.label}`;
            ctx.fillStyle = COLORS.externalBg;
            const width = externalLabelWidth(label);
            roundRect(pos.x, pos.y - 22 * zoom, width, 16, 3);
            ctx.fill();
            ctx.fillStyle = COLORS.warning;
            ctx.textAlign = 'left';
            ctx.textBaseline = 'middle';
            ctx.fillText(label, pos.x + 4, pos.y - 22 * zoom + 8);
        }
    }

    // Draws the node title centered inside the cell. With a game font it
    // renders §-coloured text glyph-by-glyph from the bitmap atlas (the
    // frame's lower slot in textured mode); otherwise the same styled
    // pipeline falls back to the system font. The raw id stays visible
    // (dimmed) when a localised title is shown.
    function drawNodeTitle(node, pos, w, textured) {
        const title = node.title ? node.title.value : '';
        const label = title || node.id;
        const font = options.gameFonts ? titleFont(node.title) : null;
        if (textured) {
            const slotX = pos.x + EMT_TITLE_X * zoom;
            const slotY = pos.y + EMT_TITLE_Y * zoom;
            const slotW = EMT_TITLE_WIDTH * zoom;
            // Top-aligned like the game's title label: two font line-heights
            // fit the frame's slot exactly (2 × 18px in 38px for vic_18).
            const layout = font
                ? {
                    font,
                    zoom,
                    defaultColor: COLORS.texturedText,
                    baseline: font.base * zoom,
                    sysFont: `${Math.max(8, font.base * zoom)}px ${FONT_FAMILY}`,
                }
                : {
                    font: null,
                    zoom,
                    defaultColor: COLORS.texturedText,
                    baseline: 8 * zoom,
                    sysFont: `bold ${Math.max(8, 10 * zoom)}px ${FONT_FAMILY}`,
                };
            const lineHeight = (font ? font.lineHeight : 11) * zoom;
            const lines = styledLines(label, slotW, layout).slice(0, 2);
            lines.forEach((tokens, index) => {
                drawStyledLine(tokens, slotX + (slotW - lineWidth(tokens, layout)) / 2, slotY + index * lineHeight, layout);
            });
            return;
        }
        const h = NODE_HEIGHT * zoom;
        const layout = font
            ? {
                font,
                zoom,
                defaultColor: COLORS.text,
                baseline: font.base * zoom,
                sysFont: `${Math.max(9, font.base * zoom)}px ${FONT_FAMILY}`,
            }
            : {
                font: null,
                zoom,
                defaultColor: COLORS.text,
                baseline: 9 * zoom,
                sysFont: `${Math.max(9, 11 * zoom)}px ${FONT_FAMILY}`,
            };
        const lineHeight = (font ? font.lineHeight : 13) * zoom;
        const lines = styledLines(label, w - 12, layout).slice(0, 2);
        const blockHeight = lines.length * lineHeight;
        const startY = pos.y + h / 2 - blockHeight / 2;
        lines.forEach((tokens, index) => {
            drawStyledLine(tokens, pos.x + (w - lineWidth(tokens, layout)) / 2, startY + index * lineHeight, layout);
        });
        if (title) {
            ctx.fillStyle = COLORS.dim;
            ctx.font = `${Math.max(8, 9 * zoom)}px ${FONT_FAMILY}`;
            ctx.textAlign = 'center';
            ctx.textBaseline = 'middle';
            ctx.fillText(node.id, pos.x + w / 2, startY + blockHeight + lineHeight);
        }
    }

    // EMT-style node: the mission icon under the game frame texture, with a
    // white bold title in the frame's slot and diagnostic/selection overlays.
    function drawNodeTextured(node, i, pos, frame) {
        const w = NODE_WIDTH * zoom;
        const h = NODE_HEIGHT * zoom;
        const isHovered = hovered && hovered.kind === 'node' && hovered.index === i;
        drawImageWorld(textureImage(node.icon), node.x + EMT_ICON_X, node.y + EMT_ICON_Y);
        drawImageWorld(frame, node.x, node.y);
        if (isHovered || (options.showDiagnostics && (node.hasError || node.hasWarning))) {
            ctx.strokeStyle = isHovered ? COLORS.borderSelected : nodeColor(node);
            ctx.lineWidth = isHovered ? 3 : 2;
            roundRect(pos.x, pos.y, w, h, 4 * zoom);
            ctx.stroke();
        }
        drawNodeTitle(node, pos, w, true);
        drawExternal(node, pos);
    }

    function drawNode(node, i, frame) {
        if (!worldRectVisible(node.x, node.y, NODE_WIDTH, NODE_HEIGHT, 36)) {
            return;
        }
        const pos = toScreen(node.x, node.y);
        if (frame) {
            drawNodeTextured(node, i, pos, frame);
            return;
        }
        // Texture-less fallback: a schematic card with color-coded borders.
        const w = NODE_WIDTH * zoom;
        const h = NODE_HEIGHT * zoom;
        const isHovered = hovered && hovered.kind === 'node' && hovered.index === i;
        ctx.fillStyle = options.showDiagnostics && node.hasError
            ? COLORS.errorBg
            : COLORS.card;
        roundRect(pos.x, pos.y, w, h, 6 * zoom);
        ctx.fill();
        ctx.strokeStyle = isHovered ? COLORS.borderSelected : nodeColor(node);
        ctx.lineWidth = isHovered ? 2 : 1;
        ctx.stroke();
        drawNodeTitle(node, pos, w, false);
        drawExternal(node, pos);
    }

    function drawFrame() {
        ctx.clearRect(0, 0, canvas.clientWidth, canvas.clientHeight);
        if (!preview) {
            return;
        }
        drawArrows();
        drawGroups();
        const frame = textureImage('GFX_mission_icons_frame');
        preview.nodes.forEach((node, index) => {
            if (isTreeVisible(node.tree)) {
                drawNode(node, index, frame);
            }
        });
    }

    function showStatus(message) {
        status.textContent = message;
        status.classList.add('visible');
        scheduleDraw();
    }

    // The preview pins to the most recently focused mission file, so the tree
    // on screen can belong to a different document than the active editor; the
    // badge names it. documentUri is the percent-encoded URI string from the
    // payload.
    function setFileBadge(documentUri) {
        let decoded = documentUri;
        try {
            decoded = decodeURIComponent(documentUri);
        } catch (error) {
            // Malformed percent sequences: keep the raw URI text.
        }
        const slash = decoded.lastIndexOf('/');
        fileBadge.textContent = slash === -1 ? decoded : decoded.slice(slash + 1);
        fileBadge.title = decoded;
        fileBadge.hidden = false;
    }

    function hideStatus() {
        status.classList.remove('visible');
    }

    function renderSummary() {
        if (!preview || !options.showDiagnostics) {
            hideStatus();
            return;
        }
        const visible = preview.nodes.filter((node) => isTreeVisible(node.tree));
        const errors = visible.filter((node) => node.hasError).length;
        const warnings = visible.filter((node) => node.hasWarning).length;
        if (errors + warnings > 0) {
            showStatus([
                t('statusMissions', visible.length),
                errors === 1 ? t('errorSingular', errors) : t('errorPlural', errors),
                warnings === 1 ? t('warningSingular', warnings) : t('warningPlural', warnings),
            ].join(' · '));
        } else {
            hideStatus();
        }
    }

    // §-stripped localised titles, cached per payload node. Search and the
    // plain-text labels must not match against colour codes.
    const plainTitles = new WeakMap();

    function plainTitle(node) {
        let plain = plainTitles.get(node);
        if (plain === undefined) {
            plain = node.title && node.title.value ? parseLocFormat(node.title.value).plain : '';
            plainTitles.set(node, plain);
        }
        return plain;
    }

    function nodeLabel(node) {
        const title = plainTitle(node) || node.id;
        const flags = [];
        if (options.showDiagnostics && node.hasError) flags.push('error');
        if (options.showDiagnostics && node.hasWarning) flags.push('warning');
        return `${title}${flags.length ? ` · ${flags.join(', ')}` : ''}`;
    }

    function postJump(hit) {
        if (!preview || !hit) {
            return;
        }
        const node = hit.node;
        vscode.postMessage({
            type: hit.kind === 'group' ? 'openGroup' : 'jump',
            uri: preview.documentUri,
            range: node.sourceRange || null,
        });
    }

    // --- mission search ------------------------------------------------------

    const SEARCH_RESULT_LIMIT = 50;
    let searchMatches = []; // node indices, in payload order
    let searchSelected = -1;

    function runSearch() {
        const query = (searchInput?.value ?? '').trim().toLowerCase();
        searchMatches = [];
        if (query && preview) {
            for (let i = 0; i < preview.nodes.length && searchMatches.length < SEARCH_RESULT_LIMIT; i += 1) {
                const node = preview.nodes[i];
                const title = plainTitle(node);
                if (node.id.toLowerCase().includes(query) || title.toLowerCase().includes(query)) {
                    searchMatches.push(i);
                }
            }
        }
        searchSelected = -1;
        renderSearchResults();
    }

    // Matches list regardless of series visibility; rows inside hidden
    // series stay reachable (jump to source) but are dimmed, since their
    // canvas node is not drawn.
    function renderSearchResults() {
        if (!searchResults) {
            return;
        }
        if (searchMatches.length === 0) {
            searchResults.replaceChildren();
            searchResults.classList.remove('visible');
            return;
        }
        const fragment = document.createDocumentFragment();
        searchMatches.forEach((nodeIndex, rank) => {
            const node = preview.nodes[nodeIndex];
            const row = document.createElement('button');
            row.type = 'button';
            row.className = 'search-result';
            if (!isTreeVisible(node.tree)) {
                row.classList.add('hidden-series');
            }
            row.title = node.titleKey || node.id;
            row.setAttribute('role', 'listitem');
            row.dataset.rank = String(rank);
            const primary = document.createElement('span');
            primary.className = 'search-result-title';
            primary.textContent = plainTitle(node) || node.id;
            const secondary = document.createElement('span');
            secondary.className = 'search-result-series';
            secondary.textContent = `${treeIds[node.tree]}${isTreeVisible(node.tree) ? '' : t('seriesHiddenSuffix')}`;
            row.append(primary, secondary);
            fragment.appendChild(row);
        });
        searchResults.replaceChildren(fragment);
        searchResults.classList.add('visible');
    }

    function highlightSearchSelection() {
        if (!searchResults) {
            return;
        }
        let selectedRow = null;
        searchResults.querySelectorAll('button[data-rank]').forEach((row) => {
            const isSelected = Number(row.dataset.rank) === searchSelected;
            row.classList.toggle('selected', isSelected);
            if (isSelected) {
                selectedRow = row;
            }
        });
        selectedRow?.scrollIntoView({ block: 'nearest' });
    }

    // Jump to the result's source definition; when its series is on the
    // canvas, also center the view on the node and show the hover ring.
    function openSearchResult(rank) {
        if (rank < 0 || rank >= searchMatches.length || !preview) {
            return;
        }
        const nodeIndex = searchMatches[rank];
        const node = preview.nodes[nodeIndex];
        postJump({ kind: 'node', node, index: nodeIndex });
        if (isTreeVisible(node.tree)) {
            pan.x = canvas.clientWidth / 2 - (node.x + NODE_WIDTH / 2) * zoom;
            pan.y = canvas.clientHeight / 2 - (node.y + NODE_HEIGHT / 2) * zoom;
            hovered = { kind: 'node', index: nodeIndex, node, rect: null };
            keyboardIndex = nodeIndex;
            scheduleDraw();
        }
    }

    searchInput?.addEventListener('input', runSearch);

    // mousedown rather than click so the input never loses focus mid-pick.
    searchResults?.addEventListener('mousedown', (event) => {
        const row = event.target.closest?.('button[data-rank]');
        if (row) {
            event.preventDefault();
            openSearchResult(Number(row.dataset.rank));
        }
    });

    searchInput?.addEventListener('keydown', (event) => {
        if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
            if (searchMatches.length === 0) {
                return;
            }
            event.preventDefault();
            const step = event.key === 'ArrowDown' ? 1 : -1;
            searchSelected = (searchSelected + step + searchMatches.length) % searchMatches.length;
            highlightSearchSelection();
        } else if (event.key === 'Enter') {
            if (searchMatches.length > 0) {
                event.preventDefault();
                openSearchResult(searchSelected >= 0 ? searchSelected : 0);
            }
        } else if (event.key === 'Escape') {
            event.preventDefault();
            if (searchInput) {
                searchInput.value = '';
            }
            runSearch();
        }
    });

    // --- series visibility panel -------------------------------------------

    function updateSeriesSummary() {
        if (!seriesSummary) {
            return;
        }
        const total = treeIds.filter(Boolean).length;
        seriesSummary.textContent = total > 0
            ? t('seriesCount', total - hiddenTreeIds.size, total)
            : t('series');
    }

    // World-space x of the grid origin (geometry::ORIGIN.0), used to derive a
    // column's slot number from a series label's x position.
    const ORIGIN_X = 16;

    // Groups series by column — identical label x means the same slot column,
    // sorted left to right; within a column the stacking order (y) is kept.
    // The panel is a horizontal mirror of the canvas layout.
    function seriesColumns() {
        const byX = new Map();
        for (const group of preview.groups) {
            let column = byX.get(group.x);
            if (!column) {
                column = {
                    x: group.x,
                    slot: Math.round((group.x - ORIGIN_X) / NODE_WIDTH) + 1,
                    groups: [],
                };
                byX.set(group.x, column);
            }
            column.groups.push(group);
        }
        const columns = [...byX.values()].sort((a, b) => a.x - b.x);
        for (const column of columns) {
            column.groups.sort((a, b) => a.y - b.y);
        }
        return columns;
    }

    function renderSeriesList() {
        if (!seriesEntries) {
            return;
        }
        if (!preview) {
            seriesEntries.replaceChildren();
            updateSeriesSummary();
            return;
        }
        const fragment = document.createDocumentFragment();
        for (const column of seriesColumns()) {
            const block = document.createElement('div');
            block.className = 'series-column';
            block.setAttribute('role', 'group');
            block.setAttribute('aria-label', t('slot', column.slot));
            const header = document.createElement('div');
            header.className = 'series-column-slot';
            header.textContent = t('slot', column.slot);
            block.append(header);
            for (const group of column.groups) {
                const row = document.createElement('label');
                row.className = 'series-entry';
                row.setAttribute('role', 'listitem');
                const box = document.createElement('input');
                box.type = 'checkbox';
                box.checked = isTreeVisible(group.tree);
                box.dataset.treeId = group.label;
                const text = document.createElement('span');
                text.textContent = group.label;
                row.append(box, text);
                block.appendChild(row);
            }
            fragment.appendChild(block);
        }
        seriesEntries.replaceChildren(fragment);
        updateSeriesSummary();
    }

    function applySeriesVisibility() {
        if (lastDocumentUri !== null) {
            hiddenByDocument.set(lastDocumentUri, hiddenTreeIds);
        }
        updateSeriesSummary();
        scheduleDraw();
        renderSearchResults();
        renderSummary();
    }

    seriesEntries?.addEventListener('change', (event) => {
        const box = event.target;
        if (!(box instanceof HTMLInputElement) || box.type !== 'checkbox' || !box.dataset.treeId) {
            return;
        }
        if (box.checked) {
            hiddenTreeIds.delete(box.dataset.treeId);
        } else {
            hiddenTreeIds.add(box.dataset.treeId);
        }
        applySeriesVisibility();
    });

    function setAllSeriesVisible(visible) {
        hiddenTreeIds = visible ? new Set() : new Set(treeIds.filter(Boolean));
        applySeriesVisibility();
        renderSeriesList();
    }

    document.getElementById('series-all')?.addEventListener('click', () => setAllSeriesVisible(true));
    document.getElementById('series-none')?.addEventListener('click', () => setAllSeriesVisible(false));

    function showTooltip(hit, clientX, clientY) {
        if (!tooltip || !hit) {
            return;
        }
        const node = hit.node;
        if (hit.kind === 'group') {
            tooltip.textContent = node.label;
        } else {
            // §-coloured spans: the tooltip mirrors the canvas colour model
            // (system font — the bitmap font only exists as an atlas).
            tooltip.replaceChildren();
            const titleLine = document.createElement('div');
            const label = (node.title && node.title.value) || node.id;
            for (const segment of parseLocFormat(label).segments) {
                const span = document.createElement('span');
                span.textContent = segment.text;
                if (segment.color) {
                    span.style.color = segment.color;
                }
                titleLine.appendChild(span);
            }
            const flags = [];
            if (options.showDiagnostics && node.hasError) flags.push(t('flagError'));
            if (options.showDiagnostics && node.hasWarning) flags.push(t('flagWarning'));
            const keyLine = document.createElement('div');
            keyLine.className = 'tooltip-dim';
            keyLine.textContent = flags.length
                ? `${node.titleKey || node.id} · ${flags.join(', ')}`
                : node.titleKey || node.id;
            tooltip.append(titleLine, keyLine);
        }
        tooltip.style.left = `${Math.min(clientX + 12, window.innerWidth - 380)}px`;
        tooltip.style.top = `${Math.min(clientY + 12, window.innerHeight - 100)}px`;
        tooltip.classList.add('visible');
        tooltip.setAttribute('aria-hidden', 'false');
    }

    function hideTooltip() {
        if (!tooltip) {
            return;
        }
        tooltip.classList.remove('visible');
        tooltip.setAttribute('aria-hidden', 'true');
    }

    function focusNode(index, step = 1) {
        if (!preview || preview.nodes.length === 0) {
            return;
        }
        keyboardIndex = (index + preview.nodes.length) % preview.nodes.length;
        // Keyboard focus steps over hidden-series nodes in the move direction.
        for (let i = 0; i < preview.nodes.length && !isTreeVisible(preview.nodes[keyboardIndex].tree); i += 1) {
            keyboardIndex = (keyboardIndex + step + preview.nodes.length) % preview.nodes.length;
        }
        const node = preview.nodes[keyboardIndex];
        hovered = { kind: 'node', index: keyboardIndex, node, rect: null };
        canvas.setAttribute('aria-label', t('missionAria', nodeLabel(node)));
        scheduleDraw();
    }

    window.addEventListener('message', (event) => {
        const message = event.data;
        if (message.type === 'i18n') {
            Object.assign(strings, message.strings);
            document.documentElement.lang = message.language;
            applyStaticStrings();
            runSearch();
            renderSeriesList();
            renderSummary();
        } else if (message.type === 'preview') {
            setPreview(message.payload);
            switchDocument(message.payload.documentUri);
            setFileBadge(message.payload.documentUri);
            syncSeriesState();
            keyboardIndex = -1;
            hideStatus();
            scheduleDraw();
            runSearch();
            renderSeriesList();
            renderSummary();
        } else if (message.type === 'empty' || message.type === 'error') {
            preview = null;
            fileBadge.hidden = true;
            runSearch();
            renderSeriesList();
            hideTooltip();
            showStatus(message.message || t('noPreview'));
        } else if (message.type === 'assets') {
            setAssets(message);
        } else if (message.type === 'options') {
            options = {
                zoomSensitivity: Math.min(2, Math.max(0.5, Number(message.zoomSensitivity) || 1)),
                showTextures: message.showTextures !== false,
                showExternalPrerequisites: message.showExternalPrerequisites !== false,
                showDiagnostics: message.showDiagnostics !== false,
                gameFonts: message.gameFonts !== false,
            };
            renderSummary();
            scheduleDraw();
        }
    });

    // Press then decide: a move beyond the threshold pans (even when the
    // press started on a node), a release without movement jumps to source.
    const DRAG_THRESHOLD_PX = 3;

    canvas.addEventListener('mousedown', (event) => {
        const rect = canvas.getBoundingClientRect();
        const hit = nodeAt(event.clientX - rect.left, event.clientY - rect.top);
        press = {
            clientX: event.clientX,
            clientY: event.clientY,
            hit,
            panning: false,
            panX: pan.x,
            panY: pan.y,
        };
    });

    window.addEventListener('mousemove', (event) => {
        if (press) {
            const dx = event.clientX - press.clientX;
            const dy = event.clientY - press.clientY;
            if (!press.panning && Math.hypot(dx, dy) > DRAG_THRESHOLD_PX) {
                press.panning = true;
            }
            if (press.panning) {
                pan.x = press.panX + dx;
                pan.y = press.panY + dy;
                scheduleDraw();
                return;
            }
        }
        const rect = canvas.getBoundingClientRect();
        const next = nodeAt(event.clientX - rect.left, event.clientY - rect.top);
        const changed = (hovered === null) !== (next === null) ||
            (hovered && next && hovered.index !== next.index);
        if (changed) {
            hovered = next;
            scheduleDraw();
        }
        if (next) showTooltip(next, event.clientX, event.clientY);
        else hideTooltip();
    });

    window.addEventListener('mouseup', () => {
        if (press && !press.panning && press.hit) {
            postJump(press.hit);
        }
        press = null;
    });

    canvas.addEventListener('mouseleave', hideTooltip);

    canvas.addEventListener('wheel', (event) => {
        event.preventDefault();
        const rect = canvas.getBoundingClientRect();
        const sx = event.clientX - rect.left;
        const sy = event.clientY - rect.top;
        const factor = Math.pow(1.0015, -event.deltaY * options.zoomSensitivity);
        const nextZoom = Math.min(2.5, Math.max(0.35, zoom * factor));
        const world = toWorld(sx, sy);
        zoom = nextZoom;
        pan.x = sx - world.x * zoom;
        pan.y = sy - world.y * zoom;
        scheduleDraw();
    }, { passive: false });

    canvas.addEventListener('dblclick', () => {
        fitView();
        scheduleDraw();
    });

    canvas.addEventListener('keydown', (event) => {
        if (!preview) {
            return;
        }
        if (event.key === 'ArrowDown' || event.key === 'ArrowRight') {
            event.preventDefault();
            focusNode(keyboardIndex + 1, 1);
        } else if (event.key === 'ArrowUp' || event.key === 'ArrowLeft') {
            event.preventDefault();
            focusNode(keyboardIndex - 1, -1);
        } else if (event.key === 'Enter' && keyboardIndex >= 0) {
            event.preventDefault();
            postJump({ kind: 'node', node: preview.nodes[keyboardIndex], index: keyboardIndex });
        } else if (event.key === '+' || event.key === '=') {
            event.preventDefault();
            zoomBy(1.15);
        } else if (event.key === '-') {
            event.preventDefault();
            zoomBy(1 / 1.15);
        } else if (event.key.toLowerCase() === 'f') {
            event.preventDefault();
            fitView();
            scheduleDraw();
        }
    });

    document.getElementById('fit')?.addEventListener('click', () => { fitView(); scheduleDraw(); });
    document.getElementById('zoom-in')?.addEventListener('click', () => zoomBy(1.15));
    document.getElementById('zoom-out')?.addEventListener('click', () => zoomBy(1 / 1.15));

    window.addEventListener('resize', resize);
    resize();
})();
