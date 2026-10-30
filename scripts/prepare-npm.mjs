#!/usr/bin/env node
// Assemble one platform-specific npm package from a prebuilt Rust binary.
// Example: node scripts/prepare-npm.mjs aarch64-apple-darwin target/aarch64-apple-darwin/release/refranero
import { copyFileSync, mkdirSync, readFileSync, statSync, writeFileSync, chmodSync } from 'node:fs';
import { basename, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(fileURLToPath(new URL('../', import.meta.url)));
const platforms = {
  'aarch64-apple-darwin': ['darwin', 'arm64'],
  'x86_64-apple-darwin': ['darwin', 'x64'],
  'x86_64-unknown-linux-gnu': ['linux', 'x64'],
  'aarch64-unknown-linux-gnu': ['linux', 'arm64'],
  'x86_64-pc-windows-msvc': ['win32', 'x64'],
  'aarch64-pc-windows-msvc': ['win32', 'arm64'],
};
const [target, binaryPath, version = '0.1.0', outputRoot = join(root, 'npm', 'dist')] = process.argv.slice(2);
if (!platforms[target] || !binaryPath || !/^\d+\.\d+\.\d+(?:-[a-z\d.-]+)?$/.test(version)) {
  process.stderr.write('Usage: node scripts/prepare-npm.mjs <rust-target> <binary-path> [version] [output-directory]\n');
  process.exit(2);
}
const sourceBinary = resolve(binaryPath);
try {
  if (statSync(sourceBinary).size === 0) {
    throw new Error('file is empty');
  }
} catch (error) {
  process.stderr.write(`Invalid native binary ${sourceBinary}: ${error.message}\n`);
  process.exit(2);
}
const [os, cpu] = platforms[target];
const packageDir = join(resolve(outputRoot), `${os}-${cpu}`);
const binaryDir = join(packageDir, 'bin');
const filename = os === 'win32' ? 'refranero.exe' : 'refranero';
mkdirSync(binaryDir, { recursive: true });
copyFileSync(sourceBinary, join(binaryDir, filename));
if (os !== 'win32') chmodSync(join(binaryDir, filename), 0o755);
let manifest = readFileSync(join(root, 'npm/platform/package.json.template'), 'utf8');
manifest = manifest.replaceAll('__OS__', os).replaceAll('__CPU__', cpu).replaceAll('__VERSION__', version);
writeFileSync(join(packageDir, 'package.json'), manifest);
console.log(`Prepared ${basename(packageDir)} at ${packageDir}`);
