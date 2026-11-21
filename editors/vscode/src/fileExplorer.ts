import * as vscode from 'vscode';
import { LanguageClient } from 'vscode-languageclient/node';

export interface WorkspaceFileRoot {
    id: number;
    kind: 'vanilla' | 'dependency' | 'project' | string;
    path: string;
    order: number;
    writable: boolean;
    /** Optional display name from the existing dependency configuration. */
    name?: string;
}

export interface WorkspaceFileItem {
    id: number;
    rootId: number;
    logicalPath: string;
    uri: string;
    category?: string | null;
    active: boolean;
}

export interface WorkspaceFilesResponse {
    revision: number;
    roots: WorkspaceFileRoot[];
    files: WorkspaceFileItem[];
}

type ExplorerNode = RootNode | FileNode;

interface RootNode {
    kind: 'root';
    root: WorkspaceFileRoot;
}

interface FileNode {
    kind: 'file';
    file: WorkspaceFileItem;
}

function rootLabel(root: WorkspaceFileRoot): string {
    switch (root.kind) {
        case 'project':
            return vscode.l10n.t('Project');
        case 'dependency':
            return vscode.l10n.t('Dependency · {0}', root.path);
        case 'vanilla':
            return vscode.l10n.t('Vanilla');
        default:
            return root.kind;
    }
}

/** Read-only source-root view. It intentionally exposes logical paths and active resolution
 * state, while opening the physical URI only on explicit user action. */
export class LoadedFilesProvider implements vscode.TreeDataProvider<ExplorerNode> {
    private readonly changed = new vscode.EventEmitter<ExplorerNode | undefined | null | void>();
    private readonly view: vscode.TreeView<ExplorerNode>;
    private client?: LanguageClient;
    private data?: WorkspaceFilesResponse;
    private pending?: Promise<WorkspaceFilesResponse | undefined>;

    public readonly onDidChangeTreeData = this.changed.event;

    public constructor() {
        this.view = vscode.window.createTreeView('paradoxcode.loadedFiles', { treeDataProvider: this });
        this.view.onDidChangeVisibility(({ visible }) => {
            if (visible) {
                void this.loadIfVisible();
            }
        });
    }

    public dispose(): void {
        this.client = undefined;
        this.pending = undefined;
        this.view.dispose();
        this.changed.dispose();
    }

    public setClient(client: LanguageClient | undefined): void {
        if (this.client === client) {
            return;
        }
        this.client = client;
        this.pending = undefined;
        this.data = undefined;
        this.changed.fire();
        void this.loadIfVisible();
    }

    public getTreeItem(node: ExplorerNode): vscode.TreeItem {
        if (node.kind === 'root') {
            const item = new vscode.TreeItem(rootLabel(node.root), vscode.TreeItemCollapsibleState.Expanded);
            item.description = node.root.writable
                ? vscode.l10n.t('editable')
                : vscode.l10n.t('read-only');
            item.contextValue = `paradoxcode.root.${node.root.kind}`;
            item.iconPath = new vscode.ThemeIcon(
                node.root.kind === 'project' ? 'folder-opened' : 'library',
            );
            return item;
        }
        const item = new vscode.TreeItem(node.file.logicalPath, vscode.TreeItemCollapsibleState.None);
        item.description = node.file.active
            ? vscode.l10n.t('active definition')
            : node.file.category ?? '';
        item.tooltip = `${node.file.logicalPath}\n${node.file.uri}`;
        item.contextValue = node.file.active ? 'paradoxcode.activeFile' : 'paradoxcode.loadedFile';
        item.iconPath = new vscode.ThemeIcon(node.file.active ? 'file-code' : 'file');
        item.command = {
            command: 'vscode.open',
            title: vscode.l10n.t('Open Loaded File'),
            arguments: [vscode.Uri.parse(node.file.uri)],
        };
        return item;
    }

    public getChildren(node?: ExplorerNode): ExplorerNode[] {
        if (!node) {
            const roots = this.data?.roots;
            return Array.isArray(roots) ? roots.map((root) => ({ kind: 'root', root })) : [];
        }
        if (node.kind === 'root' && Array.isArray(this.data?.files)) {
            return this.data.files
                .filter((file) => file.rootId === node.root.id)
                .map((file) => ({ kind: 'file', file }));
        }
        return [];
    }

    public refresh(): Promise<void> {
        this.data = undefined;
        if (!this.view.visible) {
            // Fetch a fresh snapshot on reveal even if an older request is still finishing.
            this.pending = undefined;
        }
        return this.loadIfVisible();
    }

    private async loadIfVisible(): Promise<void> {
        if (!this.view.visible || !this.client || this.data) {
            return;
        }
        const request = this.pending ??= this.client.sendRequest<WorkspaceFilesResponse>('pdc/workspaceFiles')
            .catch(() => undefined);
        const response = await request;
        // A stopped or replaced client must not repopulate the tree with an old listing.
        if (this.pending === request) {
            this.pending = undefined;
            this.data = response;
            this.changed.fire();
        }
    }
}
