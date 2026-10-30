#!/usr/bin/env node
// npm only dispatches to a platform-native Rust executable. No postinstall downloads.
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';

const require = createRequire(import.meta.url);
const pkg = `@yacosta738/refranero-${process.platform}-${process.arch}`;
const exe = process.platform === 'win32' ? 'refranero.exe' : 'refranero';
let binary;
try {
  const root = dirname(require.resolve(`${pkg}/package.json`));
  binary = join(root, 'bin', exe);
} catch (error) {
  process.stderr.write(`Missing native package ${pkg}. Reinstall @yacosta738/refranero with optional dependencies enabled.\n`);
  process.stderr.write(`${error.message}\n`);
  process.exit(1);
}

const child = spawnSync(binary, process.argv.slice(2), { stdio: 'inherit', windowsHide: true });
if (child.error) {
  process.stderr.write(`Cannot execute ${binary}: ${child.error.message}\n`);
  process.exit(1);
}
if (child.signal) {
  process.stderr.write(`refranero exited on signal ${child.signal}\n`);
  process.exit(1);
}
process.exit(child.status ?? 1);
