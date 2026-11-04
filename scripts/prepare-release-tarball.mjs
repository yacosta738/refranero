#!/usr/bin/env node
// Assemble one platform release tarball and its SHA-256 checksum from a prebuilt Rust binary.
//
// Example:
//   node scripts/prepare-release-tarball.mjs aarch64-apple-darwin target/aarch64-apple-darwin/release/refranero 0.4.0 release-dist
//
// The archive contains a single entry named `refranero` at its root, with mode 0755.
// The checksum file uses the conventional `<hex>  <asset>` layout, so `sha256sum -c` accepts it.
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { gzipSync } from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { isSemver } from './semver.mjs';

const root = resolve(fileURLToPath(new URL('../', import.meta.url)));
const supportedTargets = [
  'aarch64-apple-darwin',
  'x86_64-apple-darwin',
  'aarch64-unknown-linux-gnu',
  'x86_64-unknown-linux-gnu',
];
const blockSize = 512;

const [target, binaryPath, version, outputRoot = join(root, 'release-dist')] = process.argv.slice(2);
if (!supportedTargets.includes(target) || !binaryPath || !isSemver(version)) {
  process.stderr.write(
    'Usage: node scripts/prepare-release-tarball.mjs <rust-target> <binary-path> <version> [output-directory]\n' +
      `Supported targets: ${supportedTargets.join(', ')}\n`,
  );
  process.exit(2);
}

const sourceBinary = resolve(binaryPath);
let binary;
try {
  if (statSync(sourceBinary).size === 0) {
    throw new Error('file is empty');
  }
  binary = readFileSync(sourceBinary);
} catch (error) {
  process.stderr.write(`Invalid native binary ${sourceBinary}: ${error.message}\n`);
  process.exit(2);
}

function octal(value, length) {
  return `${value.toString(8).padStart(length - 1, '0')}\0`;
}

// Build one ustar header block for a regular file.
function headerBlock(name, size, mode) {
  const block = Buffer.alloc(blockSize);
  block.write(name, 0, 100, 'utf8');
  block.write(octal(mode, 8), 100, 8, 'ascii');
  block.write(octal(0, 8), 108, 8, 'ascii');
  block.write(octal(0, 8), 116, 8, 'ascii');
  block.write(octal(size, 12), 124, 12, 'ascii');
  block.write(octal(0, 12), 136, 12, 'ascii');
  block.write('        ', 148, 8, 'ascii');
  block.write('0', 156, 1, 'ascii');
  block.write('ustar\0', 257, 6, 'ascii');
  block.write('00', 263, 2, 'ascii');
  block.write('root', 265, 32, 'ascii');
  block.write('root', 297, 32, 'ascii');
  block.write(octal(0, 8), 329, 8, 'ascii');
  block.write(octal(0, 8), 337, 8, 'ascii');
  let checksum = 0;
  for (const byte of block) {
    checksum += byte;
  }
  block.write(`${checksum.toString(8).padStart(6, '0')}\0 `, 148, 8, 'ascii');
  return block;
}

// Assemble a deterministic tar archive that holds one executable entry at its root.
function buildTarball(name, contents) {
  const padding = (blockSize - (contents.length % blockSize)) % blockSize;
  return Buffer.concat([
    headerBlock(name, contents.length, 0o755),
    contents,
    Buffer.alloc(padding),
    Buffer.alloc(blockSize * 2),
  ]);
}

const asset = `refranero-${version}-${target}.tar.gz`;
const archive = gzipSync(buildTarball('refranero', binary), { level: 9 });
const digest = createHash('sha256').update(archive).digest('hex');

const outputDir = resolve(outputRoot);
mkdirSync(outputDir, { recursive: true });
writeFileSync(join(outputDir, asset), archive);
writeFileSync(join(outputDir, `${asset}.sha256`), `${digest}  ${asset}\n`);

process.stdout.write(`${join(outputDir, asset)}\n${join(outputDir, `${asset}.sha256`)}\n`);
