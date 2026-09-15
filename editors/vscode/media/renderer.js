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
    const nodeList = document.getElementById('node-list');
    const ctx = canvas.getContext('2d');

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
            minX = Math.min(minX, group.x);
            minY = Math.min(minY, group.y);
            maxX = Math.max(maxX, group.x + 200);
            maxY = Math.max(maxY, group.y + 18);
        }
        for (const node of preview.nodes) {
            minX = Math.min(minX, node.x);
            minY = Math.min(minY, node.y);
            maxX = Math.max(maxX, node.x + NODE_WIDTH);
            maxY = Math.max(maxY, node.y + NODE_HEIGHT);
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

    // Snapshot the outgoing document's viewport and adopt the incoming one
    // (remembered view, or a fit for a first look). Call after `setPreview`
    // so `fitView` can measure the new payload.
    function switchDocument(documentUri) {
        if (documentUri === lastDocumentUri) {
            return;
        }
        if (lastDocumentUri !== null) {
            viewportsByDocument.set(lastDocumentUri, { x: pan.x, y: pan.y, zoom });
        }
        lastDocumentUri = documentUri;
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

    function wrapText(text, maxWidth) {
        if (!text) {
            return [''];
        }
        const words = text.split(/(\s+)/).filter((w) => w.trim().length > 0);
        const lines = [];
        let line = '';
        for (const word of words) {
            const candidate = line ? `${line} ${word}` : word;
            if (ctx.measureText(candidate).width <= maxWidth || !line) {
                line = candidate;
            } else {
                lines.push(line);
                line = word;
            }
        }
        if (line) {
            lines.push(line);
        }
        return lines.slice(0, 3);
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

    // Returns the loaded image for a sprite, or null when the server did not
    // supply one. Images decode asynchronously; drawing happens on `load`.
    // A failed decode is cached as a miss so the schematic fallback stays
    // reachable (a failed `Image` still reports `complete === true`).
    function textureImage(name) {
        if (!options.showTextures || !preview || !preview.textures || !name || failedTextures.has(name)) {
            return null;
        }
        const url = preview.textures[name];
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

    // Draws one server-placed arrow segment: its game texture when the server
    // supplied one (EMT-style tile assembly), or a schematic stroke otherwise.
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

    // Draws all arrow segments. When the server supplies textures every glyph
    // is drawn as its tile (vertical runs become continuous automatically);
    // without textures, vertical runs chain into one continuous stroke.
    function drawArrows() {
        if (!preview) {
            return;
        }
        let chain = null; // { x, y } of the previous vertical glyph in this run
        for (const segment of preview.arrows) {
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

    // Draws the node title centered inside the cell. In textured mode it sits
    // in the frame's lower slot like the game UI; the raw id stays visible
    // (dimmed) when a localised title is shown.
    function drawNodeTitle(node, pos, w, textured) {
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        const title = node.title ? node.title.value : '';
        const label = title || node.id;
        if (textured) {
            const slotX = pos.x + EMT_TITLE_X * zoom;
            const slotY = pos.y + EMT_TITLE_Y * zoom;
            const slotW = EMT_TITLE_WIDTH * zoom;
            ctx.fillStyle = COLORS.texturedText;
            ctx.font = `bold ${Math.max(8, 10 * zoom)}px ${FONT_FAMILY}`;
            // Top-aligned like the game's title label: first line sits at the
            // slot's top edge, subsequent lines follow.
            const lines = wrapText(label, slotW).slice(0, 2);
            const lineHeight = 11 * zoom;
            lines.forEach((line, lineIndex) => {
                ctx.fillText(line, slotX + slotW / 2, slotY + lineHeight / 2 + lineIndex * lineHeight);
            });
            return;
        }
        const h = NODE_HEIGHT * zoom;
        ctx.fillStyle = COLORS.text;
        ctx.font = `${Math.max(9, 11 * zoom)}px ${FONT_FAMILY}`;
        const lines = wrapText(label, w - 12).slice(0, 2);
        const lineHeight = 13 * zoom;
        const blockHeight = lines.length * lineHeight;
        const startY = pos.y + h / 2 - blockHeight / 2;
        lines.forEach((line, lineIndex) => {
            ctx.fillText(line, pos.x + w / 2, startY + lineIndex * lineHeight);
        });
        if (title) {
            ctx.fillStyle = COLORS.dim;
            ctx.font = `${Math.max(8, 9 * zoom)}px ${FONT_FAMILY}`;
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
        preview.nodes.forEach((node, index) => drawNode(node, index, frame));
    }

    function showStatus(message) {
        status.textContent = message;
        status.classList.add('visible');
        scheduleDraw();
    }

    function hideStatus() {
        status.classList.remove('visible');
    }

    function renderSummary() {
        if (!preview || !options.showDiagnostics) {
            hideStatus();
            return;
        }
        const errors = preview.diagnostics.filter((d) => d.severity === 1).length;
        const warnings = preview.diagnostics.filter((d) => d.severity === 2).length;
        if (errors + warnings > 0) {
            showStatus(
                `${preview.nodes.length} missions · ${errors} error${errors === 1 ? '' : 's'} · ${warnings} warning${warnings === 1 ? '' : 's'}`,
            );
        } else {
            hideStatus();
        }
    }

    function nodeLabel(node) {
        const title = node.title && node.title.value ? node.title.value : node.id;
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

    function renderNodeList() {
        if (!nodeList) {
            return;
        }
        if (!preview) {
            nodeList.replaceChildren();
            return;
        }
        const fragment = document.createDocumentFragment();
        preview.nodes.forEach((node, index) => {
            const button = document.createElement('button');
            button.type = 'button';
            button.className = 'node-entry';
            if (options.showDiagnostics && node.hasError) button.classList.add('error');
            else if (options.showDiagnostics && node.hasWarning) button.classList.add('warning');
            button.textContent = nodeLabel(node);
            button.title = node.titleKey || node.id;
            button.setAttribute('role', 'listitem');
            button.dataset.nodeIndex = String(index);
            fragment.appendChild(button);
        });
        nodeList.replaceChildren(fragment);
    }

    // Delegate list events once instead of allocating two closures per node
    // on every preview refresh. This matters for large mission files and also
    // lets the browser replace the list in one DOM operation.
    nodeList?.addEventListener('focusin', (event) => {
        const button = event.target.closest?.('button[data-node-index]');
        const index = button ? Number(button.dataset.nodeIndex) : -1;
        if (!preview || !Number.isInteger(index) || index < 0 || index >= preview.nodes.length) {
            return;
        }
        keyboardIndex = index;
        hovered = { kind: 'node', index, node: preview.nodes[index], rect: null };
        scheduleDraw();
    });

    nodeList?.addEventListener('click', (event) => {
        const button = event.target.closest?.('button[data-node-index]');
        const index = button ? Number(button.dataset.nodeIndex) : -1;
        if (!preview || !Number.isInteger(index) || index < 0 || index >= preview.nodes.length) {
            return;
        }
        postJump({ kind: 'node', node: preview.nodes[index], index });
    });

    function showTooltip(hit, clientX, clientY) {
        if (!tooltip || !hit) {
            return;
        }
        const node = hit.node;
        tooltip.textContent = hit.kind === 'node'
            ? `${nodeLabel(node)}\n${node.titleKey || node.id}`
            : node.label;
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

    function focusNode(index) {
        if (!preview || preview.nodes.length === 0) {
            return;
        }
        keyboardIndex = (index + preview.nodes.length) % preview.nodes.length;
        const node = preview.nodes[keyboardIndex];
        hovered = { kind: 'node', index: keyboardIndex, node, rect: null };
        canvas.setAttribute('aria-label', `Mission ${nodeLabel(node)}`);
        scheduleDraw();
    }

    window.addEventListener('message', (event) => {
        const message = event.data;
        if (message.type === 'preview') {
            setPreview(message.payload);
            switchDocument(message.payload.documentUri);
            keyboardIndex = -1;
            hideStatus();
            scheduleDraw();
            renderNodeList();
            renderSummary();
        } else if (message.type === 'empty' || message.type === 'error') {
            preview = null;
            renderNodeList();
            hideTooltip();
            showStatus(message.message || 'No preview available.');
        } else if (message.type === 'options') {
            options = {
                zoomSensitivity: Math.min(2, Math.max(0.5, Number(message.zoomSensitivity) || 1)),
                showTextures: message.showTextures !== false,
                showExternalPrerequisites: message.showExternalPrerequisites !== false,
                showDiagnostics: message.showDiagnostics !== false,
            };
            renderNodeList();
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
            focusNode(keyboardIndex + 1);
        } else if (event.key === 'ArrowUp' || event.key === 'ArrowLeft') {
            event.preventDefault();
            focusNode(keyboardIndex - 1);
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
