/* Editor search webview. File content is only ever assigned through text nodes. */
(() => {
    'use strict';
    const vscode = acquireVsCodeApi();
    const DEFAULT_STRINGS = {
        panelTitle: 'ParadoxCode Search',
        tabs: 'Search categories',
        rules: 'Rules',
        definitions: 'Definitions',
        localisation: 'Localisation',
        text: 'Full text',
        query: 'Query',
        placeholder: 'Enter text to search…',
        search: 'Search',
        cancel: 'Cancel',
        language: 'Language',
        searchIn: 'Search in',
        value: 'Readable value',
        key: 'Key',
        versions: 'Versions',
        allVersions: 'All versions',
        active: 'Active',
        overridden: 'Overridden',
        kind: 'Definition type',
        context: 'Context',
        scope: 'Allowed scope',
        sources: 'Sources',
        all: 'All',
        project: 'Current mod',
        vanilla: 'Vanilla',
        dependency: 'Dependency · {0}',
        unavailable: '{0} (unavailable)',
        back: 'Back to search',
        retry: 'Retry',
        results: 'Search results',
        more: 'Load more',
        hint: 'Enter a query. No source files are searched while the query is empty.',
        searching: 'Searching…',
        cancelled: 'Search cancelled.',
        noResults: 'No results match this query and its filters.',
        partialEmpty: 'No matches in the available data. Search coverage is incomplete.',
        counts: '{0} / {1} groups loaded · {2} / {3} confirmed locations loaded',
        copy: 'Copy name',
        references: 'Find references',
        otherVersions: '{0} other matching versions',
        fullValue: 'Show full value',
        details: 'Details',
        noDocumentation: 'No documentation or examples are available.',
        anyScope: 'Any scope',
        unsaved: 'Unsaved',
        entryPosition: 'Opens the entry',
        referencesTitle: 'References to {0}',
        keyReferences: 'References are to this key, independent of the selected language.',
        removeFilter: 'Clear {0}',
        refreshing: 'Workspace changed; refreshing search…',
        caseSensitive: 'Match case',
        include: 'Include paths',
        exclude: 'Exclude paths',
        paths: 'Path filters',
        pathHint: 'Source-relative globs, separated by commas',
        occurrences: 'Occurrences: {0}…{1}',
        deprecated: 'Deprecated',
        sourceCount: '{0} / {1} sources selected',
        detailTruncated: 'The value exceeds the display budget. Open the source to inspect the remainder.',
        showContext: 'Show nearby lines',
        line: 'Line {0}',
        fileMatches: '{0} confirmed matches',
        incomplete: 'Search coverage is incomplete.',
        indexing: 'Indexing… Search will refresh when the workspace is ready.',
        copyKey: 'Copy key',
        copyId: 'Copy ID',
        copyRule: 'Copy rule name',
        searchText: 'Search this name in full text',
        ruleSource: 'First-party rules · {0} · {1}',
        navigationNotice: 'Opened the source line because this view represents the text differently.',
        valueUnavailable: 'Full readable value unavailable',
        pushScope: 'Nested scope: {0}',
    };
    let strings = { ...DEFAULT_STRINGS };
    const t = (key, ...values) => (strings[key] || DEFAULT_STRINGS[key] || key).replace(/\{(\d+)\}/g, (_, i) => String(values[Number(i)] ?? ''));
    const byId = (id) => document.getElementById(id);
    const state = Object.fromEntries(['rules', 'definitions', 'localisation', 'text'].map(tab => [tab, { query: '', language: '', mode: 'value', status: 'all', kind: '', context: '', scope: '', include: '', exclude: '', caseSensitive: false, response: undefined }]));
    let tab = 'localisation';
    let sequence = 0;
    let timer;
    let sources;
    const sourceNames = new Map();
    let selectedRoots;
    let context;
    let contextLoaded = false;
    let references = false;
    let referenceResponse;
    let referenceName;
    const detailsById = new Map();
    let pending = false;
    const fields = ['language', 'mode', 'status', 'kind', 'context', 'scope', 'include', 'exclude', 'caseSensitive'];
    const element = (tag, text, className) => { const node = document.createElement(tag); if (text != null) node.textContent = text; if (className) node.className = className; return node; };
    const button = (label, action, className) => { const node = element('button', label, className); node.type = 'button'; node.addEventListener('click', action); return node; };
    const status = (message) => { byId('status-message').textContent = message; };
    function localise() {
        for (const attr of ['data-i18n', 'data-i18n-placeholder', 'data-i18n-aria-label']) {
            for (const node of document.querySelectorAll(`[${attr}]`)) {
                if (attr === 'data-i18n') node.textContent = t(node.getAttribute(attr));
                else node.setAttribute(attr === 'data-i18n-placeholder' ? 'placeholder' : 'aria-label', t(node.getAttribute(attr)));
            }
        }
    }
    function save() {
        state[tab].query = byId('query').value;
        for (const field of fields) state[tab][field] = field === 'caseSensitive' ? byId(field).checked : byId(field).value;
    }
    function stop() {
        clearTimeout(timer);
        sequence++;
        pending = false;
        byId('cancel').hidden = true;
        vscode.postMessage({ type: 'cancel' });
    }
    function schedule() {
        save(); stop(); references = false; byId('back').hidden = true; byId('search-text').hidden = true;
        state[tab].response = undefined;
        byId('results').replaceChildren(); byId('more').hidden = true;
        renderFilters();
        if (!state[tab].query.trim()) { status(t('hint')); return; }
        status(t('searching'));
        timer = setTimeout(() => search(), 180);
    }
    function search(offset = 0) {
        if (!context) { vscode.postMessage({ type: 'context' }); return; }
        save();
        if (!state[tab].query.trim()) { status(t('hint')); return; }
        stop(); pending = true; byId('cancel').hidden = false; byId('retry').hidden = true; byId('more').hidden = true;
        status(t('searching'));
        const current = state[tab];
        vscode.postMessage({ type: 'search', sequence, params: {
            tab, query: current.query, roots: tab === 'rules' ? undefined : [...selectedRoots],
            language: tab === 'localisation' ? current.language || undefined : undefined,
            searchKeys: tab === 'localisation' && current.mode === 'key',
            status: ['definitions', 'localisation'].includes(tab) ? current.status : 'all',
            kind: tab === 'definitions' ? current.kind || undefined : undefined,
            context: tab === 'rules' ? current.context || undefined : undefined,
            scope: tab === 'rules' ? current.scope || undefined : undefined,
            caseSensitive: tab === 'text' && current.caseSensitive,
            include: tab === 'text' ? current.include.split(',').map(path => path.trim()).filter(Boolean) : [],
            exclude: tab === 'text' ? current.exclude.split(',').map(path => path.trim()).filter(Boolean) : [],
            offset, revision: offset ? current.response?.revision : undefined, snapshot: offset ? current.response?.snapshot : undefined,
        }});
    }
    function setTab(next) {
        if (next === tab) return;
        save(); stop(); tab = next; references = false; byId('back').hidden = true; byId('search-text').hidden = true;
        byId('query').value = state[tab].query;
        for (const field of fields) { if (field === 'caseSensitive') byId(field).checked = state[tab][field]; else byId(field).value = state[tab][field]; }
        for (const node of byId('tabs').querySelectorAll('button')) {
            const selected = node.dataset.tab === tab;
            node.setAttribute('aria-selected', String(selected)); node.tabIndex = selected ? 0 : -1;
        }
        for (const field of fields) byId(`${field === 'mode' ? 'mode' : field}-filter`).hidden = !({ localisation: ['language', 'mode', 'status'], definitions: ['kind', 'status'], rules: ['context', 'scope'], text: ['include', 'exclude', 'caseSensitive'] }[tab].includes(field));
        byId('sources').hidden = tab === 'rules';
        byId('rule-source').hidden = tab !== 'rules';
        byId('paths').hidden = tab !== 'text';
        renderFilters();
        if (state[tab].response && state[tab].response.revision === context?.revision) render(state[tab].response);
        else schedule();
    }
    function rootLabel(root) { return root.kind === 'dependency' ? t('dependency', root.name || root.path) : t(root.kind === 'project' ? 'project' : root.kind === 'vanilla' ? 'vanilla' : root.kind); }
    function renderRoots() {
        byId('roots').replaceChildren();
        for (const root of sources) {
            const label = element('label');
            const input = element('input'); input.type = 'checkbox'; input.checked = selectedRoots.has(root.id);
            input.addEventListener('change', () => { input.checked ? selectedRoots.add(root.id) : selectedRoots.delete(root.id); for (const current of Object.values(state)) current.response = undefined; schedule(); });
            label.append(input, element('span', rootLabel(root))); byId('roots').append(label);
        }
        for (const id of selectedRoots) {
            if (!sources.some(root => root.id === id)) {
                byId('roots').append(button(t('unavailable', sourceNames.get(id) || String(id)), () => { selectedRoots.delete(id); renderRoots(); schedule(); }));
            }
        }
    }
    function options(id, values, selected) {
        const select = byId(id); select.replaceChildren();
        const all = element('option', t('all')); all.value = ''; select.append(all);
        for (const value of values) { const node = element('option', value); node.value = value; select.append(node); }
        if (selected && !values.includes(selected)) { const node = element('option', t('unavailable', selected)); node.value = selected; select.append(node); }
        select.value = selected;
    }
    function renderFilters() {
        const box = byId('active-filters'); box.replaceChildren();
        for (const field of fields) {
            const value = state[tab][field];
            if (!value || (field === 'status' && value === 'all') || (field === 'mode' && value === 'value')) continue;
            if (!({ localisation: ['language', 'mode', 'status'], definitions: ['kind', 'status'], rules: ['context', 'scope'], text: ['include', 'exclude', 'caseSensitive'] }[tab].includes(field))) continue;
            box.append(button(t('removeFilter', `${t(field === 'mode' ? 'searchIn' : field === 'status' ? 'versions' : field)}: ${value}`), () => { if (field === 'caseSensitive') byId(field).checked = false; else byId(field).value = field === 'status' ? 'all' : field === 'mode' ? 'value' : ''; schedule(); }));
        }
        if (tab !== 'rules' && selectedRoots) box.append(element('span', t('sourceCount', selectedRoots.size, sources?.length || 0), 'metadata'));
        if (tab !== 'rules' && selectedRoots && selectedRoots.size !== sources?.length) {
            box.append(button(t('removeFilter', t('sources')), () => { selectedRoots = new Set(sources.map(root => root.id)); renderRoots(); schedule(); }));
        }
    }
    function highlight(node, text) {
        const words = state[tab].query.trim().split(/\s+/).filter(Boolean).map(word => word.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'));
        if (!words.length) { node.textContent = text; return; }
        const matcher = new RegExp(words.join('|'), tab === 'text' && state.text.caseSensitive ? 'gu' : 'giu');
        let previous = 0;
        for (const match of text.matchAll(matcher)) {
            node.append(document.createTextNode(text.slice(previous, match.index)));
            node.append(element('mark', match[0])); previous = match.index + match[0].length;
        }
        node.append(document.createTextNode(text.slice(previous)));
    }
    function entryView(entry) {
        const node = element('article', undefined, 'result');
        const heading = element('div', undefined, 'heading');
        const name = entry.location ? button(tab === 'text' || references ? t('line', entry.location.range.start.line + 1) : entry.name, () => vscode.postMessage({ type: 'open', id: entry.id }), 'open') : element('strong', entry.name);
        if (name.tagName === 'BUTTON' || entry.location) { name.replaceChildren(); highlight(name, tab === 'text' || references ? t('line', entry.location.range.start.line + 1) : entry.name); }
        heading.append(name, element('span', entry.kind, 'metadata')); if (entry.title) heading.append(element('span', entry.title)); node.append(heading);
        const root = sources?.find(root => root.id === entry.rootId);
        const labels = [root ? rootLabel(root) : '', entry.language || '', entry.active === undefined ? '' : t(entry.active ? 'active' : 'overridden'), entry.dirty ? t('unsaved') : '', entry.fullValueAvailable === false ? t('valueUnavailable') : '', entry.precision === 'entry' ? t('entryPosition') : ''].filter(Boolean);
        if (entry.location) labels.push(`${entry.location.path}:${entry.location.range.start.line + 1}`);
        node.append(element('div', labels.join(' · '), 'metadata'));
        const value = entry.text || (tab === 'rules' ? t('noDocumentation') : '');
        if (value) {
            const snippet = element('div', undefined, 'snippet');
            const firstWord = state[tab].query.trim().split(/\s+/)[0]?.toLowerCase();
            const hit = value.toLowerCase().indexOf(firstWord || '');
            const start = Math.max(0, hit - 60);
            const preview = value.length > 240 ? `${start ? '…' : ''}${value.slice(start, start + 240)}${start + 240 < value.length ? '…' : ''}` : value;
            highlight(snippet, preview); node.append(snippet);
            if (entry.hasFullText) { const details = element('details'); details.append(element('summary', t('fullValue'))); const full = element('div', undefined, 'snippet'); details.append(full); detailsById.set(entry.id, full); details.addEventListener('toggle', () => { if (details.open && !full.textContent) vscode.postMessage({ type: 'detail', id: entry.id }); }); node.append(details); }
            else if (preview !== value) { const details = element('details'); details.append(element('summary', t('fullValue'))); const full = element('div', undefined, 'snippet'); highlight(full, value); details.append(full); node.append(details); }
        }
        if (tab === 'rules') {
            const details = element('details'); details.append(element('summary', t('details')));
            details.append(element('p', `${entry.valueRequirement || ''} · ${entry.allowedScopes?.join(', ') || t('anyScope')}`));
            details.append(element('p', t('occurrences', entry.cardinality?.min ?? 0, entry.cardinality?.max ?? '∞')));
            if (entry.pushScope) details.append(element('p', t('pushScope', entry.pushScope)));
            if (entry.deprecated) details.append(element('p', t('deprecated'))); node.append(details);
        }
        if (entry.context) { const details = element('details'); details.append(element('summary', t('showContext'))); const context = element('pre'); highlight(context, entry.context); details.append(context); node.append(details); }
        const actions = element('div', undefined, 'actions');
        if (tab !== 'text' && !references) actions.append(button(t(tab === 'localisation' ? 'copyKey' : tab === 'definitions' ? 'copyId' : 'copyRule'), () => vscode.postMessage({ type: 'copy', id: entry.id })));
        if (['definitions', 'localisation'].includes(tab) && !references) actions.append(button(t('references'), () => { stop(); status(t('searching')); vscode.postMessage({ type: 'references', id: entry.id, roots: [...selectedRoots] }); }));
        node.append(actions); return node;
    }
    function render(response) {
        byId('results').replaceChildren(); byId('limitations').replaceChildren(); detailsById.clear();
        for (const note of response.notes || []) byId('limitations').append(element('li', note));
        for (const limitation of response.limitations || []) byId('limitations').append(element('li', limitation));
        for (const group of response.groups) {
            if (!group.versions.length) continue;
            if (tab === 'text' || references) { const file = element('section', undefined, 'result'); const root = sources?.find(root => root.id === group.versions[0].rootId); file.append(element('strong', `${root ? rootLabel(root) + ' · ' : ''}${group.versions[0].location.path} · ${t('fileMatches', group.matchCount ?? group.versions.length)}`)); for (const entry of group.versions) file.append(entryView(entry)); byId('results').append(file); continue; }
            const view = entryView(group.versions[0]);
            if (group.versions.length > 1) { const details = element('details'); details.append(element('summary', t('otherVersions', group.versions.length - 1))); for (const entry of group.versions.slice(1)) details.append(entryView(entry)); view.append(details); }
            byId('results').append(view);
        }
        byId('more').hidden = response.nextOffset == null;
        status(response.groups.length ? `${t('counts', response.loaded, response.totalGroups, response.groups.reduce((sum, group) => sum + group.versions.length, 0), response.totalMatches)}${response.limitations?.length ? ' · ' + t('incomplete') : ''}` : t(response.limitations?.length ? 'partialEmpty' : 'noResults'));
    }
    byId('query').addEventListener('input', schedule);
    for (const field of fields) byId(field).addEventListener('change', schedule);
    byId('form').addEventListener('submit', event => { event.preventDefault(); save(); search(); });
    byId('cancel').addEventListener('click', () => { stop(); status(t('cancelled')); });
    byId('retry').addEventListener('click', () => { if (!context) vscode.postMessage({ type: 'context' }); else search(); });
    byId('more').addEventListener('click', () => { if (references) { stop(); status(t('searching')); vscode.postMessage({ type: 'moreReferences', offset: referenceResponse.nextOffset }); } else search(state[tab].response.nextOffset); });
    byId('search-text').addEventListener('click', () => { const name = referenceName; setTab('text'); byId('query').value = name || ''; schedule(); });
    byId('back').addEventListener('click', () => { stop(); references = false; byId('back').hidden = true; byId('search-text').hidden = true; render(state[tab].response); });
    const tabs = [...byId('tabs').querySelectorAll('button')];
    for (const node of tabs) node.addEventListener('click', () => setTab(node.dataset.tab));
    byId('tabs').addEventListener('keydown', event => {
        if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
        event.preventDefault(); const index = tabs.findIndex(node => node.dataset.tab === tab);
        const next = event.key === 'Home' ? 0 : event.key === 'End' ? tabs.length - 1 : (index + (event.key === 'ArrowRight' ? 1 : -1) + tabs.length) % tabs.length;
        setTab(tabs[next].dataset.tab); tabs[next].focus();
    });
    window.addEventListener('message', ({ data: message }) => {
        switch (message.type) {
            case 'i18n': strings = { ...DEFAULT_STRINGS, ...message.strings }; document.documentElement.lang = message.language; localise(); break;
            case 'initial': state.localisation.query = message.query || ''; byId('query').value = state.localisation.query; byId('query').focus(); break;
            case 'focus': byId('query').focus(); break;
            case 'context': {
                context = message.context; sources = context.roots; for (const root of sources) sourceNames.set(root.id, rootLabel(root));
                byId('rule-source').textContent = t('ruleSource', context.gameId || 'ParadoxCode', context.ruleVersion || '');
                byId('rule-source').title = context.ruleSet || '';
                if (selectedRoots === undefined) selectedRoots = new Set(sources.map(root => root.id));
                if (!contextLoaded) { state.localisation.language = context.preferredLanguage; contextLoaded = true; }
                options('language', context.languages, state[tab].language); options('kind', context.kinds, state[tab].kind); options('context', context.contexts, state[tab].context); options('scope', context.scopes, state[tab].scope);
                renderRoots(); renderFilters(); if (state[tab].query.trim()) search(); else status(t('hint')); break;
            }
            case 'results': {
                if (message.sequence !== sequence) break;
                pending = false; byId('cancel').hidden = true;
                const previous = state[tab].response;
                const response = message.response;
                if (previous && response.loaded > response.groups.length && previous.revision === response.revision) response.groups = [...previous.groups, ...response.groups];
                state[tab].response = response; if (response.languages && tab === 'localisation') { context.languages = response.languages; options('language', response.languages, state[tab].language); } render(response); break;
            }
            case 'references': references = true; referenceName = message.name; byId('back').hidden = false; byId('search-text').hidden = false; if (message.offset && referenceResponse?.revision === message.response.revision) message.response.groups = [...referenceResponse.groups, ...message.response.groups]; referenceResponse = message.response; render(message.response); status(`${t('referencesTitle', message.name)} · ${message.kind || ''} · ${message.response.totalMatches}`); if (tab === 'localisation') byId('limitations').append(element('li', t('keyReferences'))); break;
            case 'detail': { const node = detailsById.get(message.response.id); if (node) { node.replaceChildren(); highlight(node, message.response.text); if (message.response.truncated) node.append(element('p', t('detailTruncated'))); } break; }
            case 'invalidate': stop(); references = false; referenceResponse = undefined; context = undefined; byId('back').hidden = true; byId('search-text').hidden = true; byId('more').hidden = true; for (const current of Object.values(state)) current.response = undefined; byId('results').replaceChildren(); status(t('refreshing')); vscode.postMessage({ type: 'context' }); break;
            case 'navigationNotice': status(t('navigationNotice')); break;
            case 'indexing': pending = true; byId('cancel').hidden = false; status(t('indexing')); break;
            case 'cancelled': if (message.sequence !== undefined && message.sequence !== sequence) break; pending = false; byId('cancel').hidden = true; byId('retry').hidden = false; status(t('cancelled')); break;
            case 'error': if (message.sequence !== undefined && message.sequence !== sequence) break; pending = false; byId('cancel').hidden = true; byId('retry').hidden = false; status(message.message); break;
        }
    });
    localise();
    // Set all filter visibility before the first context response.
    tab = 'rules'; setTab('localisation');
    vscode.postMessage({ type: 'ready' });
})();
