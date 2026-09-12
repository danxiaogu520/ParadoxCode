/**
 * Path spelling helpers shared by the diagnostic and sweep tooling.
 */

import { relative, sep } from 'node:path';

/**
 * Returns `file` relative to `root` in the forward-slash logical spelling the
 * server uses for `LogicalPath` values.
 */
export function logicalRelative(root, file) {
    return relative(root, file).split(sep).join('/');
}
