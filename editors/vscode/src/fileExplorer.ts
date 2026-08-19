import * as vscode from 'vscode';
import { LanguageClient } from 'vscode-languageclient/lib/node/main';

export interface WorkspaceFileRoot {
    id: number;
    kind: 'vanilla' | 'dependency' | 'currentMod' | string;
    path: string;
    order: number;
    writable: boolean;
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
        case 'currentMod':
            return 'Current Mod';
        case 'dependency':
            return `Dependency · ${root.path}`;
        case 'vanilla':
            return 'Vanilla';
        default:
            return root.kind;
    }
}

/** Read-only source-root view. It intentionally exposes logical paths and active resolution
 * state, while opening the physical URI only on explicit user action. */
export class LoadedFilesProvider implements vscode.TreeDataProvider<ExplorerNode> {
    private readonly changed = new vscode.EventEmitter<ExplorerNode | undefined | null | void>();
    private roots: WorkspaceFileRoot[] = [];
    private files: WorkspaceFileItem[] = [];

    public readonly onDidChangeTreeData = this.changed.event;

    public dispose(): void {
        this.changed.dispose();
    }

    public clear(): void {
        this.roots = [];
        this.files = [];
        this.changed.fire();
    }

    public setData(response: WorkspaceFilesResponse): void {
        this.roots = Array.isArray(response.roots) ? response.roots : [];
        this.files = Array.isArray(response.files) ? response.files : [];
        this.changed.fire();
    }

    public getTreeItem(node: ExplorerNode): vscode.TreeItem {
        if (node.kind === 'root') {
            const item = new vscode.TreeItem(rootLabel(node.root), vscode.TreeItemCollapsibleState.Expanded);
            item.description = node.root.writable ? 'editable' : 'read-only';
            item.contextValue = `paradoxcode.root.${node.root.kind}`;
            item.iconPath = new vscode.ThemeIcon(
                node.root.kind === 'currentMod' ? 'folder-opened' : 'library',
            );
            return item;
        }
        const item = new vscode.TreeItem(node.file.logicalPath, vscode.TreeItemCollapsibleState.None);
        item.description = node.file.active ? 'active definition' : node.file.category ?? '';
        item.tooltip = `${node.file.logicalPath}\n${node.file.uri}`;
        item.contextValue = node.file.active ? 'paradoxcode.activeFile' : 'paradoxcode.loadedFile';
        item.iconPath = new vscode.ThemeIcon(node.file.active ? 'file-code' : 'file');
        item.command = {
            command: 'vscode.open',
            title: 'Open Loaded File',
            arguments: [vscode.Uri.parse(node.file.uri)],
        };
        return item;
    }

    public getChildren(node?: ExplorerNode): ExplorerNode[] {
        if (!node) {
            return this.roots.map((root) => ({ kind: 'root', root }));
        }
        if (node.kind === 'root') {
            return this.files
                .filter((file) => file.rootId === node.root.id)
                .map((file) => ({ kind: 'file', file }));
        }
        return [];
    }

    public async refresh(client: LanguageClient | undefined): Promise<void> {
        if (!client) {
            this.clear();
            return;
        }
        try {
            const response = await client.sendRequest<WorkspaceFilesResponse>('pdx/workspaceFiles');
            this.setData(response);
        } catch {
            // A server that is stopping or an older binary without this optional request should
            // not make the Explorer noisy. The empty state explains itself in the view title.
            this.clear();
        }
    }
}
