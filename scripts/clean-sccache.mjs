// Wipe the local sccache disk cache and stop the server, so the next build
// restarts fresh with zeroed statistics. The cache directory is discovered
// from `sccache --show-stats`, so no OS-specific path is hardcoded and this
// works on macOS, Windows and Linux.
//
// Destructive: wiping only happens with --force. With --dry-run (or no flag
// at all) the script prints the cache location and changes nothing.
import { execFileSync } from 'node:child_process';
import { rmSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const args = new Set(process.argv.slice(2));
const force = args.has('--force');

let stats = null;
try {
  stats = execFileSync('sccache', ['--show-stats'], { encoding: 'utf8' });
} catch (error) {
  console.error(`sccache is not available: ${error.message}`);
  process.exit(1);
}

const match = stats.match(/Cache location\s+Local disk:\s*"([^"]+)"/);

if (!match) {
  console.log('No local sccache disk cache found; nothing wiped.');
  process.exit(0);
}

const cacheDir = path.resolve(match[1]);
const home = path.resolve(os.homedir());
const root = path.parse(cacheDir).root;

// Guard rails: never wipe the filesystem root or the home directory itself,
// whatever the stats output claims.
if (cacheDir === root || cacheDir === home) {
  console.error(`Refusing to wipe unexpected sccache location: ${cacheDir}`);
  process.exit(1);
}

if (!force) {
  console.log(`Would stop the sccache server and wipe ${cacheDir} (pass --force to proceed).`);
  process.exit(0);
}

try {
  execFileSync('sccache', ['--stop-server'], { stdio: 'inherit' });
} catch {
  // The server may already be stopped; wiping continues below.
}

rmSync(cacheDir, { recursive: true, force: true });
console.log(`Wiped sccache disk cache: ${cacheDir}`);
