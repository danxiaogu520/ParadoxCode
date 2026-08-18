import * as fs from 'fs';
import * as path from 'path';

/** Extra extensions tried for extensionless command names on Windows, mirroring
 * the PATHEXT search `child_process.spawn` performs via libuv. Kept to the
 * built-in set so resolution is deterministic. */
const WINDOWS_EXTENSIONS = ['.com', '.exe', '.bat', '.cmd'];

/** Resolves `name` to an existing file on `$PATH`, treating empty entries as
 * the current directory the way the OS search rules do. Returns `undefined`
 * when no candidate exists.
 *
 * This is a pre-launch warning helper only: the extension still lets the real
 * spawn happen, so platform resolution this model does not cover (for example
 * Windows App Paths) keeps working. */
export function findExecutableOnPath(name: string): string | undefined {
    const pathValue = process.env.PATH ?? process.env.Path ?? '';
    const extensions =
        process.platform === 'win32' && path.extname(name) === ''
            ? WINDOWS_EXTENSIONS
            : [''];
    for (const entry of pathValue.split(path.delimiter)) {
        const dir = entry.trim() === '' ? process.cwd() : entry;
        for (const extension of extensions) {
            const candidate = path.join(dir, name + extension);
            if (fs.existsSync(candidate)) {
                return candidate;
            }
        }
    }
    return undefined;
}
