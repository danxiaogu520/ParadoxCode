// Mission-tree canvas renderer for the ParadoxCode preview webview.
//
// Consumes the `pdx/missionPreview` wire contract: world-space node/group
// positions, arrow glyph placements, byte spans for jump-to-source, and
// mission-scoped diagnostics. All geometry is presentation-only; the server
// owns layout semantics — including every arrow segment (the renderer maps
// glyph kinds to drawings and never recomputes layout).

(function () {
    'use strict';

    const vscode = acquireVsCodeApi();

    const NODE_WIDTH = 104;
    const NODE_HEIGHT = 122;

    // Arrow glyph world metrics, mirroring `pdx-mission-model::geometry`:
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

    const COLORS = {
        border: '#39414c',
        borderSelected: '#4d7cfe',
        error: '#d55c5c',
        warning: '#d9a13b',
        root: '#5fd58a',
        text: getComputedStyle(document.body).color || '#d4d8dd',
        dim: '#8a919c',
        groupBg: 'rgba(42, 50, 64, 0.85)',
        arrow: '#4d7cfe',
        canvas: 'transparent',
    };

    const canvas = document.getElementById('tree');
    const status = document.getElementById('status');
    const ctx = canvas.getContext('2d');

    let preview = null;
    let hovered = null; // { kind: 'node'|'group', index, rect }
    let pan = { x: 0, y: 0 };
    let zoom = 1;
    let dragging = null; // { startX, startY, panX, panY }

    function resize() {
        const dpr = window.devicePixelRatio || 1;
        canvas.width = Math.round(canvas.clientWidth * dpr);
        canvas.height = Math.round(canvas.clientHeight * dpr);
        ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        draw();
    }

    function toScreen(wx, wy) {
        return { x: wx * zoom + pan.x, y: wy * zoom + pan.y };
    }

    function toWorld(sx, sy) {
        return { x: (sx - pan.x) / zoom, y: (sy - pan.y) / zoom };
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
        if (node.hasError) {
            return COLORS.error;
        }
        if (node.hasWarning) {
            return COLORS.warning;
        }
        if (node.isRoot) {
            return COLORS.root;
        }
        return COLORS.border;
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
            const w = Math.max(ctx.measureText(group.label).width + 16, 60) * zoom;
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
        if (!preview || !preview.textures || !name || failedTextures.has(name)) {
            return null;
        }
        const url = preview.textures[name];
        if (!url) {
            return null;
        }
        let image = textureImages.get(name);
        if (!image) {
            image = new Image();
            image.onload = () => draw();
            image.onerror = () => {
                failedTextures.add(name);
                textureImages.delete(name);
                draw();
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
    function drawArrowSegment(segment) {
        if (textureImage(segment.texture)) {
            drawImageWorld(textureImage(segment.texture), segment.x, segment.y);
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
            if (textureImage(segment.texture)) {
                chain = null;
                drawArrowSegment(segment);
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
                drawArrowSegment(segment);
            } else {
                chain = null; // heads and horizontal tiles end any vertical chain
                drawArrowSegment(segment);
            }
        }
    }

    function drawGroups() {
        if (!preview) {
            return;
        }
        for (const group of preview.groups) {
            const pos = toScreen(group.x, group.y);
            const width = ctx.measureText(group.label).width + 16;
            const hoveredGroup = hovered && hovered.kind === 'group' && hovered.node === group;
            ctx.fillStyle = COLORS.groupBg;
            roundRect(pos.x, pos.y, width, 18, 4);
            ctx.fill();
            ctx.strokeStyle = hoveredGroup ? COLORS.borderSelected : COLORS.border;
            ctx.lineWidth = 1;
            ctx.stroke();
            ctx.fillStyle = COLORS.dim;
            ctx.font = `11px ${getComputedStyle(document.body).fontFamily}`;
            ctx.textAlign = 'left';
            ctx.textBaseline = 'middle';
            ctx.fillText(group.label, pos.x + 8, pos.y + 10);
        }
    }

    function drawExternal(node, pos) {
        if (!preview) {
            return;
        }
        for (const ext of preview.external) {
            if (ext.tree !== node.tree || ext.mission !== node.mission) {
                continue;
            }
            const label = `↥ ${ext.label}`;
            ctx.fillStyle = 'rgba(27, 30, 35, 0.8)';
            const width = ctx.measureText(label).width + 8;
            roundRect(pos.x, pos.y - 22 * zoom, width, 16, 3);
            ctx.fill();
            ctx.fillStyle = COLORS.warning;
            ctx.font = `10px ${getComputedStyle(document.body).fontFamily}`;
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
            ctx.fillStyle = '#ffffff';
            ctx.font = `bold ${Math.max(8, 10 * zoom)}px ${getComputedStyle(document.body).fontFamily}`;
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
        ctx.font = `${Math.max(9, 11 * zoom)}px ${getComputedStyle(document.body).fontFamily}`;
        const lines = wrapText(label, w - 12).slice(0, 2);
        const lineHeight = 13 * zoom;
        const blockHeight = lines.length * lineHeight;
        const startY = pos.y + h / 2 - blockHeight / 2;
        lines.forEach((line, lineIndex) => {
            ctx.fillText(line, pos.x + w / 2, startY + lineIndex * lineHeight);
        });
        if (title) {
            ctx.fillStyle = COLORS.dim;
            ctx.font = `${Math.max(8, 9 * zoom)}px ${getComputedStyle(document.body).fontFamily}`;
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
        if (isHovered || node.hasError || node.hasWarning || node.isRoot) {
            ctx.strokeStyle = isHovered ? COLORS.borderSelected : nodeColor(node);
            ctx.lineWidth = isHovered ? 3 : 2;
            roundRect(pos.x, pos.y, w, h, 4 * zoom);
            ctx.stroke();
        }
        drawNodeTitle(node, pos, w, true);
        drawExternal(node, pos);
    }

    function drawNode(node, i) {
        const pos = toScreen(node.x, node.y);
        const frame = textureImage('GFX_mission_icons_frame');
        if (frame) {
            drawNodeTextured(node, i, pos, frame);
            return;
        }
        // Texture-less fallback: a schematic card with color-coded borders.
        const w = NODE_WIDTH * zoom;
        const h = NODE_HEIGHT * zoom;
        const isHovered = hovered && hovered.kind === 'node' && hovered.index === i;
        ctx.fillStyle = node.hasError ? 'rgba(58, 31, 31, 0.9)' : node.isRoot ? 'rgba(29, 58, 42, 0.9)' : '#23282f';
        roundRect(pos.x, pos.y, w, h, 6 * zoom);
        ctx.fill();
        ctx.strokeStyle = isHovered ? COLORS.borderSelected : nodeColor(node);
        ctx.lineWidth = isHovered ? 2 : 1;
        ctx.stroke();
        drawNodeTitle(node, pos, w, false);
        drawExternal(node, pos);
    }

    function draw() {
        ctx.clearRect(0, 0, canvas.clientWidth, canvas.clientHeight);
        if (!preview) {
            return;
        }
        drawArrows();
        drawGroups();
        preview.nodes.forEach(drawNode);
    }

    function showStatus(message) {
        status.textContent = message;
        status.classList.add('visible');
        draw();
    }

    function hideStatus() {
        status.classList.remove('visible');
    }

    function renderSummary() {
        if (!preview) {
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

    window.addEventListener('message', (event) => {
        const message = event.data;
        if (message.type === 'preview') {
            preview = message.payload;
            fitView();
            hideStatus();
            draw();
            renderSummary();
        } else if (message.type === 'empty' || message.type === 'error') {
            preview = null;
            showStatus(message.message || 'No preview available.');
        }
    });

    canvas.addEventListener('mousedown', (event) => {
        const rect = canvas.getBoundingClientRect();
        const sx = event.clientX - rect.left;
        const sy = event.clientY - rect.top;
        const hit = nodeAt(sx, sy);
        if (hit && hit.kind === 'node') {
            vscode.postMessage({ type: 'jump', start: hit.node.start, end: hit.node.end });
        } else if (hit && hit.kind === 'group') {
            vscode.postMessage({ type: 'openGroup', start: hit.node.start, end: hit.node.end });
        } else {
            dragging = { startX: event.clientX, startY: event.clientY, panX: pan.x, panY: pan.y };
        }
    });

    window.addEventListener('mousemove', (event) => {
        if (dragging) {
            pan.x = dragging.panX + (event.clientX - dragging.startX);
            pan.y = dragging.panY + (event.clientY - dragging.startY);
            draw();
            return;
        }
        const rect = canvas.getBoundingClientRect();
        const next = nodeAt(event.clientX - rect.left, event.clientY - rect.top);
        const changed = (hovered === null) !== (next === null) ||
            (hovered && next && hovered.index !== next.index);
        if (changed) {
            hovered = next;
            draw();
        }
    });

    window.addEventListener('mouseup', () => {
        dragging = null;
    });

    canvas.addEventListener('wheel', (event) => {
        event.preventDefault();
        const rect = canvas.getBoundingClientRect();
        const sx = event.clientX - rect.left;
        const sy = event.clientY - rect.top;
        const factor = Math.pow(1.0015, -event.deltaY);
        const nextZoom = Math.min(2.5, Math.max(0.35, zoom * factor));
        const world = toWorld(sx, sy);
        zoom = nextZoom;
        pan.x = sx - world.x * zoom;
        pan.y = sy - world.y * zoom;
        draw();
    }, { passive: false });

    canvas.addEventListener('dblclick', () => {
        fitView();
        draw();
    });

    window.addEventListener('resize', resize);
    resize();
})();
