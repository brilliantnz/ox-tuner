// Remove node_modules so the next `pnpm install --frozen-lockfile` rebuilds it
// from the committed lockfile. Cross-platform (rmSync, no shell rm -rf).
//
// Destructive: deletion only happens with --force. With --dry-run (or no flag
// at all) the script prints what it would remove and changes nothing.
import { existsSync, realpathSync, rmSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const args = new Set(process.argv.slice(2));
const force = args.has('--force');

const target = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../node_modules');

if (!existsSync(target)) {
  console.log(`Nothing to remove: ${target} does not exist.`);
  process.exit(0);
}

// Guard rails: only delete a directory literally named `node_modules` whose
// parent looks like this repository (it contains package.json). Resolving the
// real path first keeps a stray symlink from redirecting the deletion.
const real = realpathSync(target);
const looksLikeRepoRoot = existsSync(path.join(path.dirname(real), 'package.json'));

if (path.basename(real) !== 'node_modules' || !looksLikeRepoRoot) {
  console.error(`Refusing to delete unexpected path: ${real}`);
  process.exit(1);
}

if (!force) {
  console.log(`Would remove ${real} (pass --force to delete).`);
  process.exit(0);
}

rmSync(real, { recursive: true, force: true });
console.log(`Removed ${real}`);
