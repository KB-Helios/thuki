import { existsSync, mkdirSync } from 'node:fs';
import { join, resolve } from 'node:path';

const repoRoot = resolve(import.meta.dirname, '..');
const engineGoDir = join(repoRoot, 'external', 'rag-engine', 'engine', 'go');
const binariesDir = join(repoRoot, 'src-tauri', 'binaries');
const goCacheDir = join(repoRoot, 'src-tauri', 'target', 'rag-engine-go-build');

if (!existsSync(engineGoDir)) {
  throw new Error(
    'rag-engine submodule is missing. Run: git submodule update --init --recursive',
  );
}

const rustc = Bun.spawnSync({
  cmd: ['rustc', '-vV'],
  stdout: 'pipe',
  stderr: 'inherit',
});
if (rustc.exitCode !== 0) {
  throw new Error(`rustc -vV failed with exit code ${rustc.exitCode}`);
}
const rustcOutput = new TextDecoder().decode(rustc.stdout);
const targetTriple = rustcOutput
  .split(/\r?\n/)
  .find((line) => line.startsWith('host:'))
  ?.replace(/^host:\s*/, '')
  .trim();
if (!targetTriple) {
  throw new Error('Could not determine Rust target triple from rustc -vV.');
}

const binaryStem = `ai-engine-server-${targetTriple}`;
const binaryName =
  process.platform === 'win32' ? `${binaryStem}.exe` : binaryStem;
const outputPath = join(binariesDir, binaryName);

mkdirSync(binariesDir, { recursive: true });
mkdirSync(goCacheDir, { recursive: true });

const result = Bun.spawnSync({
  cmd: ['go', 'build', '-buildvcs=false', '-o', outputPath, './cmd/server'],
  cwd: engineGoDir,
  env: {
    ...process.env,
    GOCACHE: goCacheDir,
  },
  stdout: 'inherit',
  stderr: 'inherit',
});

if (result.exitCode !== 0) {
  throw new Error(
    `rag-engine sidecar build failed with exit code ${result.exitCode}`,
  );
}

console.log(`Built rag-engine sidecar: ${outputPath}`);
