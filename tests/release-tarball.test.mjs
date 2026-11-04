import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { chmodSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const root = resolve(fileURLToPath(new URL('../', import.meta.url)));
const prepareScript = join(root, 'scripts/prepare-release-tarball.mjs');

function withTempDir(run) {
  const tempDir = mkdtempSync(join(tmpdir(), 'refranero-release-test-'));
  try {
    run(tempDir);
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
  }
}

function prepare(target, binaryPath, version, outputDir) {
  const args = [prepareScript, target, binaryPath];
  if (version !== undefined) {
    args.push(version);
  }
  if (outputDir !== undefined) {
    args.push(outputDir);
  }
  return spawnSync(process.execPath, args, { encoding: 'utf8' });
}

test('writes the tarball and the checksum file with the expected names', () => {
  withTempDir((tempDir) => {
    const binaryPath = join(tempDir, 'refranero');
    const outputDir = join(tempDir, 'release-dist');
    writeFileSync(binaryPath, 'native binary fixture');

    const result = prepare('aarch64-apple-darwin', binaryPath, '1.2.3', outputDir);

    assert.equal(result.status, 0, result.stderr);
    const asset = 'refranero-1.2.3-aarch64-apple-darwin.tar.gz';
    assert.ok(readFileSync(join(outputDir, asset)));
    assert.ok(readFileSync(join(outputDir, `${asset}.sha256`)));
  });
});

test('the checksum file follows the conventional <hex>  <asset> layout', () => {
  withTempDir((tempDir) => {
    const binaryPath = join(tempDir, 'refranero');
    const outputDir = join(tempDir, 'release-dist');
    writeFileSync(binaryPath, 'native binary fixture');

    const result = prepare('x86_64-unknown-linux-gnu', binaryPath, '0.4.0', outputDir);

    assert.equal(result.status, 0, result.stderr);
    const asset = 'refranero-0.4.0-x86_64-unknown-linux-gnu.tar.gz';
    const checksum = readFileSync(join(outputDir, `${asset}.sha256`), 'utf8');
    const expected = createHash('sha256')
      .update(readFileSync(join(outputDir, asset)))
      .digest('hex');
    assert.equal(checksum, `${expected}  ${asset}\n`);
  });
});

test('the archive holds one executable entry named refranero at its root', (t) => {
  withTempDir((tempDir) => {
    const binaryPath = join(tempDir, 'refranero');
    const outputDir = join(tempDir, 'release-dist');
    writeFileSync(binaryPath, 'native binary fixture');

    const result = prepare('aarch64-unknown-linux-gnu', binaryPath, '0.4.0', outputDir);

    assert.equal(result.status, 0, result.stderr);
    const asset = 'refranero-0.4.0-aarch64-unknown-linux-gnu.tar.gz';
    const archive = join(outputDir, asset);
    const listing = spawnSync('tar', ['-tvf', archive], { encoding: 'utf8' });
    if (listing.error) {
      t.skip(`tar is not available: ${listing.error.message}`);
      return;
    }
    assert.equal(listing.status, 0, listing.stderr);
    const entries = listing.stdout
      .split('\n')
      .filter((line) => line.trim().length > 0)
      .map((line) => line.trim());
    assert.equal(entries.length, 1);
    assert.match(entries[0], /^-rwxr-xr-x/);
    assert.match(entries[0], /refranero$/);
  });
});

test('the extracted archive preserves the binary content and the executable bit', (t) => {
  withTempDir((tempDir) => {
    const binaryPath = join(tempDir, 'refranero');
    const outputDir = join(tempDir, 'release-dist');
    const extractDir = join(tempDir, 'extract');
    writeFileSync(binaryPath, 'native binary fixture');
    chmodSync(binaryPath, 0o755);

    const result = prepare('x86_64-apple-darwin', binaryPath, '0.4.0', outputDir);

    assert.equal(result.status, 0, result.stderr);
    const asset = 'refranero-0.4.0-x86_64-apple-darwin.tar.gz';
    mkdirSync(extractDir, { recursive: true });
    const extract = spawnSync('tar', ['-xf', join(outputDir, asset), '-C', extractDir], {
      encoding: 'utf8',
    });
    if (extract.error) {
      t.skip(`tar is not available: ${extract.error.message}`);
      return;
    }
    assert.equal(extract.status, 0, extract.stderr);
    assert.equal(readFileSync(join(extractDir, 'refranero'), 'utf8'), 'native binary fixture');
  });
});

test('the checksum verifies with sha256sum', (t) => {
  withTempDir((tempDir) => {
    const binaryPath = join(tempDir, 'refranero');
    const outputDir = join(tempDir, 'release-dist');
    writeFileSync(binaryPath, 'native binary fixture');

    const result = prepare('aarch64-apple-darwin', binaryPath, '0.4.0', outputDir);

    assert.equal(result.status, 0, result.stderr);
    const asset = 'refranero-0.4.0-aarch64-apple-darwin.tar.gz';
    const verify = spawnSync('sha256sum', ['-c', `${asset}.sha256`], {
      cwd: outputDir,
      encoding: 'utf8',
    });
    if (verify.error) {
      t.skip(`sha256sum is not available: ${verify.error.message}`);
      return;
    }
    assert.equal(verify.status, 0, verify.stderr);
    assert.match(verify.stdout, /: OK/);
  });
});

test('accepts valid SemVer and rejects invalid core or prerelease numbers', () => {
  withTempDir((tempDir) => {
    const binaryPath = join(tempDir, 'refranero');
    const outputDir = join(tempDir, 'release-dist');
    writeFileSync(binaryPath, 'native binary fixture');

    const valid = prepare('aarch64-apple-darwin', binaryPath, '1.2.3-RC.1+build.5', outputDir);
    assert.equal(valid.status, 0, valid.stderr);

    for (const version of ['01.2.3', '1.2.3-01', '1.2.3-alpha..1', '1.2.3+']) {
      const invalid = prepare('aarch64-apple-darwin', binaryPath, version, outputDir);
      assert.equal(invalid.status, 2, `expected ${version} to be rejected`);
    }
  });
});

test('rejects an unsupported target, a missing version, and an empty binary', () => {
  withTempDir((tempDir) => {
    const binaryPath = join(tempDir, 'refranero');
    const outputDir = join(tempDir, 'release-dist');
    const emptyBinary = join(tempDir, 'empty');
    writeFileSync(binaryPath, 'native binary fixture');
    writeFileSync(emptyBinary, '');

    const unsupported = prepare('x86_64-pc-windows-msvc', binaryPath, '0.4.0', outputDir);
    assert.equal(unsupported.status, 2);
    assert.match(unsupported.stderr, /Usage: node scripts\/prepare-release-tarball\.mjs/);

    const missingVersion = prepare('aarch64-apple-darwin', binaryPath);
    assert.equal(missingVersion.status, 2);

    const invalidVersion = prepare('aarch64-apple-darwin', binaryPath, 'not-a-version', outputDir);
    assert.equal(invalidVersion.status, 2);

    const empty = prepare('aarch64-apple-darwin', emptyBinary, '0.4.0', outputDir);
    assert.equal(empty.status, 2);
    assert.match(empty.stderr, /Invalid native binary/);
  });
});
