// node lab/perf/vscode/transcode-stdio.cjs source.yml old-pdc serial-pdc parallel-pdc
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

function convert(executable, direction, bytes) {
    return new Promise((resolve, reject) => {
        const child = spawn(executable, ['--transcode-stdio', 'localisation', direction], { windowsHide: true });
        const output = [];
        const errors = [];
        child.stdout.on('data', chunk => output.push(chunk));
        child.stderr.on('data', chunk => errors.push(chunk));
        child.on('error', reject);
        child.stdin.on('error', error => { if (error.code !== 'EPIPE') reject(error); });
        child.on('close', code => {
            if (code === 0) resolve(Buffer.concat(output));
            else reject(new Error(executable + ' exited with ' + code + ': ' + Buffer.concat(errors).toString()));
        });
        child.stdin.end(bytes);
    });
}

async function main() {
    const [source, ...paths] = process.argv.slice(2);
    if (!source || paths.length < 2) throw new Error('Supply a YML file and at least two codec executables, baseline first.');
    const binaries = paths.map(file => path.resolve(file));
    const original = fs.readFileSync(source);
    const results = [];
    for (const copies of [0, 1, 4, 19]) {
        const raw = Buffer.concat(Array(copies).fill(original));
        const readable = await convert(binaries[0], 'decode', raw);
        for (const [direction, input] of [['decode', raw], ['encode', readable]]) {
            const expected = direction === 'decode' ? readable : await convert(binaries[0], direction, input);
            const times = binaries.map(() => []);
            for (let trial = 0; trial <= 9; trial++) {
                for (let offset = 0; offset < binaries.length; offset++) {
                    const index = (trial + offset) % binaries.length;
                    const start = performance.now();
                    const bytes = await convert(binaries[index], direction, input);
                    const elapsed = performance.now() - start;
                    assert.ok(bytes.equals(expected), 'output mismatch: ' + binaries[index]);
                    if (trial > 0) times[index].push(elapsed);
                }
            }
            results.push({ copies, direction, inputBytes: input.length, binaries: binaries.map((file, index) =>
                ({ file, medianMs: [...times[index]].sort((a, b) => a - b)[4], samplesMs: times[index] })) });
        }
    }
    console.log(JSON.stringify(results, null, 2));
}

main().catch(error => { console.error(error); process.exitCode = 1; });
