import * as fs from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import * as path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

/** Stage complete bytes beside the backing file, then publish them only if it is still current. */
export async function replaceFile(file: string, bytes: Uint8Array, validate: () => Promise<void>): Promise<void> {
    const target = await fs.realpath(file);
    const temporary = path.join(path.dirname(target), '.paradoxcode-' + randomUUID() + '.tmp');
    const mode = (await fs.stat(target)).mode;
    const handle = await fs.open(temporary, 'wx', mode);
    try {
        try {
            await handle.writeFile(bytes);
        } finally {
            await handle.close();
        }
        // A scanner or watcher on Windows can briefly hold the destination without
        // delete sharing. Recheck the original before every atomic replacement attempt.
        for (let attempt = 0; ; attempt++) {
            await validate();
            try {
                await fs.rename(temporary, target);
                break;
            } catch (error) {
                const code = (error as NodeJS.ErrnoException).code;
                if (process.platform !== 'win32' || attempt >= 4 || (code !== 'EPERM' && code !== 'EBUSY')) throw error;
                await delay(20 * 2 ** attempt);
            }
        }
    } finally {
        await fs.rm(temporary, { force: true });
    }
}
