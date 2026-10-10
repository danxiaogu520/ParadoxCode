import * as vscode from 'vscode';

/**
 * Localised UI strings for the mission preview, icon picker and search panel.
 * Each table's keys mirror the `DEFAULT_STRINGS` fallback table in
 * the matching media script; the contract test keeps key sets and English
 * values in sync between the two halves, so a key added here without the
 * media twin (or vice versa) fails `npm run check`.
 *
 * The webview starts on its English defaults (what the static HTML shows
 * before the first message lands) and swaps in this table when the panel
 * posts `i18n` as its first message. Placeholders are `{0}`-style indices
 * formatted client-side.
 */

export interface WebviewI18nMessage {
    type: 'i18n';
    language: string;
    strings: Record<string, string>;
}

export function missionPreviewStrings(): Record<string, string> {
    return {
        panelTitle: vscode.l10n.t('Mission Tree Preview'),
        toolbarAria: vscode.l10n.t('Mission preview controls'),
        canvasAria: vscode.l10n.t('Mission tree preview'),
        fit: vscode.l10n.t('Fit'),
        fitTitle: vscode.l10n.t('Fit mission tree (F)'),
        zoomOutTitle: vscode.l10n.t('Zoom out (-)'),
        zoomInTitle: vscode.l10n.t('Zoom in (+)'),
        searchPlaceholder: vscode.l10n.t('Search missions…'),
        searchAria: vscode.l10n.t('Search missions by title or id'),
        resultsAria: vscode.l10n.t('Matching missions'),
        series: vscode.l10n.t('Series'),
        seriesCount: vscode.l10n.t('Series ({0}/{1})'),
        seriesAria: vscode.l10n.t('Mission series visibility'),
        all: vscode.l10n.t('All'),
        none: vscode.l10n.t('None'),
        seriesHiddenSuffix: vscode.l10n.t(' · series hidden'),
        slot: vscode.l10n.t('Slot {0}'),
        missionAria: vscode.l10n.t('Mission {0}'),
        noPreview: vscode.l10n.t('No preview available.'),
        statusMissions: vscode.l10n.t('{0} missions'),
        errorSingular: vscode.l10n.t('{0} error'),
        errorPlural: vscode.l10n.t('{0} errors'),
        warningSingular: vscode.l10n.t('{0} warning'),
        warningPlural: vscode.l10n.t('{0} warnings'),
        flagError: vscode.l10n.t('error'),
        flagWarning: vscode.l10n.t('warning'),
    };
}

export function missionIconPickerStrings(): Record<string, string> {
    return {
        panelTitle: vscode.l10n.t('Mission Icons'),
        toolbarAria: vscode.l10n.t('Mission icon picker controls'),
        searchPlaceholder: vscode.l10n.t('Search icons…'),
        searchAria: vscode.l10n.t('Search icons by sprite name'),
        tabsAria: vscode.l10n.t('Sprite scope'),
        tabMission: vscode.l10n.t('Mission icons'),
        tabAll: vscode.l10n.t('All sprites'),
        gridAria: vscode.l10n.t('Sprite tiles'),
        hint: vscode.l10n.t('Click a tile to write it into the focused editor and close this panel.'),
        copy: vscode.l10n.t('copy'),
        copyTitle: vscode.l10n.t('Copy "{0}"'),
        frames: vscode.l10n.t('{0} frames'),
        countAll: vscode.l10n.t('{0} sprites'),
        countFiltered: vscode.l10n.t('{0} / {1} sprites'),
        waiting: vscode.l10n.t('Waiting for the sprite catalog…'),
        noMatch: vscode.l10n.t('No sprites match the current search and tab.'),
        originVanilla: vscode.l10n.t('vanilla'),
        originMod: vscode.l10n.t('mod'),
    };
}

/** First message every ParadoxCode webview receives: the UI language and its
 * localised strings, before any data message that could paint visible text. */
export function webviewI18nMessage(strings: Record<string, string>): WebviewI18nMessage {
    return { type: 'i18n', language: vscode.env.language, strings };
}

export function searchPanelStrings(): Record<string, string> {
    return {
        panelTitle: vscode.l10n.t("ParadoxCode Search"),
        tabs: vscode.l10n.t("Search categories"),
        rules: vscode.l10n.t("Rules"),
        definitions: vscode.l10n.t("Definitions"),
        localisation: vscode.l10n.t("Localisation"),
        text: vscode.l10n.t("Full text"),
        query: vscode.l10n.t("Query"),
        placeholder: vscode.l10n.t("Enter text to search…"),
        search: vscode.l10n.t("Search"),
        cancel: vscode.l10n.t("Cancel"),
        language: vscode.l10n.t("Language"),
        searchIn: vscode.l10n.t("Search in"),
        value: vscode.l10n.t("Readable value"),
        key: vscode.l10n.t("Key"),
        versions: vscode.l10n.t("Versions"),
        allVersions: vscode.l10n.t("All versions"),
        active: vscode.l10n.t("Active"),
        overridden: vscode.l10n.t("Overridden"),
        kind: vscode.l10n.t("Definition type"),
        context: vscode.l10n.t("Context"),
        scope: vscode.l10n.t("Allowed scope"),
        sources: vscode.l10n.t("Sources"),
        all: vscode.l10n.t("All"),
        project: vscode.l10n.t("Current mod"),
        vanilla: vscode.l10n.t("Vanilla"),
        dependency: vscode.l10n.t("Dependency · {0}"),
        unavailable: vscode.l10n.t("{0} (unavailable)"),
        back: vscode.l10n.t("Back to search"),
        retry: vscode.l10n.t("Retry"),
        results: vscode.l10n.t("Search results"),
        more: vscode.l10n.t("Load more"),
        hint: vscode.l10n.t("Enter a query. No source files are searched while the query is empty."),
        searching: vscode.l10n.t("Searching…"),
        cancelled: vscode.l10n.t("Search cancelled."),
        noResults: vscode.l10n.t("No results match this query and its filters."),
        partialEmpty: vscode.l10n.t("No matches in the available data. Search coverage is incomplete."),
        counts: vscode.l10n.t("{0} / {1} groups loaded · {2} / {3} confirmed locations loaded"),
        copy: vscode.l10n.t("Copy name"),
        references: vscode.l10n.t("Find references"),
        otherVersions: vscode.l10n.t("{0} other matching versions"),
        fullValue: vscode.l10n.t("Show full value"),
        details: vscode.l10n.t("Details"),
        noDocumentation: vscode.l10n.t("No documentation or examples are available."),
        anyScope: vscode.l10n.t("Any scope"),
        unsaved: vscode.l10n.t("Unsaved"),
        entryPosition: vscode.l10n.t("Opens the entry"),
        referencesTitle: vscode.l10n.t("References to {0}"),
        keyReferences: vscode.l10n.t("References are to this key, independent of the selected language."),
        removeFilter: vscode.l10n.t("Clear {0}"),
        refreshing: vscode.l10n.t("Workspace changed; refreshing search…"),
        caseSensitive: vscode.l10n.t("Match case"),
        include: vscode.l10n.t("Include paths"),
        exclude: vscode.l10n.t("Exclude paths"),
        paths: vscode.l10n.t("Path filters"),
        pathHint: vscode.l10n.t("Source-relative globs, separated by commas"),
        occurrences: vscode.l10n.t("Occurrences: {0}…{1}"),
        deprecated: vscode.l10n.t("Deprecated"),
        sourceCount: vscode.l10n.t("{0} / {1} sources selected"),
        detailTruncated: vscode.l10n.t("The value exceeds the display budget. Open the source to inspect the remainder."),
        showContext: vscode.l10n.t("Show nearby lines"),
        line: vscode.l10n.t("Line {0}"),
        fileMatches: vscode.l10n.t("{0} confirmed matches"),
        incomplete: vscode.l10n.t("Search coverage is incomplete."),
        indexing: vscode.l10n.t("Indexing… Search will refresh when the workspace is ready."),
        copyKey: vscode.l10n.t("Copy key"),
        copyId: vscode.l10n.t("Copy ID"),
        copyRule: vscode.l10n.t("Copy rule name"),
        searchText: vscode.l10n.t("Search this name in full text"),
        ruleSource: vscode.l10n.t("First-party rules · {0} · {1}"),
        navigationNotice: vscode.l10n.t("Opened the source line because this view represents the text differently."),
        valueUnavailable: vscode.l10n.t("Full readable value unavailable"),
        pushScope: vscode.l10n.t("Nested scope: {0}"),
    };
}
