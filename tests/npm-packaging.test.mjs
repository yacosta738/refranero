import assert from 'node:assert/strict';
import { chmodSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const root = resolve(fileURLToPath(new URL('../', import.meta.url)));
const prepareScript = join(root, 'scripts/prepare-npm.mjs');
const wrapperScript = join(root, 'npm/wrapper/bin/refranero.mjs');

function withTempDir(run) {
  const tempDir = mkdtempSync(join(tmpdir(), 'refranero-npm-test-'));
  try {
    run(tempDir);
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
  }
}

test('prepares a platform package in the requested output directory', () => {
  withTempDir((tempDir) => {
    const binaryPath = join(tempDir, 'refranero');
    const outputDir = join(tempDir, 'packages');
    writeFileSync(binaryPath, 'native binary fixture');

    const result = spawnSync(process.execPath, [prepareScript, 'aarch64-apple-darwin', binaryPath, '0.1.0', outputDir], {
      encoding: 'utf8',
    });

    assert.equal(result.status, 0, result.stderr);
    const packageDir = join(outputDir, 'darwin-arm64');
    assert.equal(readFileSync(join(packageDir, 'bin/refranero'), 'utf8'), 'native binary fixture');
    assert.equal(JSON.parse(readFileSync(join(packageDir, 'package.json'), 'utf8')).name, '@yacosta738/refranero-darwin-arm64');
  });
});

test('rejects a missing or empty native binary without producing a package', () => {
  withTempDir((tempDir) => {
    const outputDir = join(tempDir, 'packages');
    const emptyBinary = join(tempDir, 'empty');
    writeFileSync(emptyBinary, '');

    for (const binaryPath of [join(tempDir, 'missing'), emptyBinary]) {
      const result = spawnSync(process.execPath, [prepareScript, 'aarch64-apple-darwin', binaryPath, '0.1.0', outputDir], {
        encoding: 'utf8',
      });
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, /binary|file/i);
    }
  });
});

test('wrapper forwards arguments and preserves the native exit code', () => {
  withTempDir((tempDir) => {
    const wrapperDir = join(tempDir, 'wrapper');
    const platformPackage = `@yacosta738/refranero-${process.platform}-${process.arch}`;
    const packageDir = join(wrapperDir, 'node_modules', platformPackage);
    const binDir = join(packageDir, 'bin');
    const binaryName = process.platform === 'win32' ? 'refranero.exe' : 'refranero';
    mkdirSync(join(wrapperDir, 'bin'), { recursive: true });
    mkdirSync(binDir, { recursive: true });
    writeFileSync(join(packageDir, 'package.json'), JSON.stringify({ name: platformPackage, version: '0.1.0' }));
    writeFileSync(join(wrapperDir, 'package.json'), JSON.stringify({ type: 'module' }));
    const argsPath = join(tempDir, 'args.json');
    const binaryPath = join(binDir, binaryName);
    writeFileSync(binaryPath, `#!/usr/bin/env node\nimport { writeFileSync } from 'node:fs';\nwriteFileSync(${JSON.stringify(argsPath)}, JSON.stringify(process.argv.slice(2)));\nprocess.exit(7);\n`);
    chmodSync(binaryPath, 0o755);
    const wrapperCopy = join(wrapperDir, 'bin/refranero.mjs');
    writeFileSync(wrapperCopy, readFileSync(wrapperScript));

    const result = spawnSync(process.execPath, [wrapperCopy, '--count', '5', '--browser'], { encoding: 'utf8' });

    assert.equal(result.status, 7, result.stderr);
    assert.deepEqual(JSON.parse(readFileSync(argsPath, 'utf8')), ['--count', '5', '--browser']);
  });
});
