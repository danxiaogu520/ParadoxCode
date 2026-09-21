import * as vscode from 'vscode';

/**
 * Localised UI strings for the two webviews (mission preview, mission icon
 * picker). Each table's keys mirror the `DEFAULT_STRINGS` fallback table in
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
