const fs = require('node:fs');
const { spawnSync } = require('node:child_process');

const cleanup = process.env.STATE_cleanup === 'true';
if (!cleanup) {
  fs.appendFileSync(process.env.GITHUB_STATE, 'cleanup=true\n');
}
const result = spawnSync('bash', ['scripts/android-preview-signing.sh', cleanup ? 'cleanup' : 'prepare'], {
  cwd: process.env.GITHUB_WORKSPACE,
  stdio: 'inherit',
});
if (result.error) throw result.error;
process.exit(result.status ?? 1);
