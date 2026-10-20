// Builds library.js into a component: node build.mjs <output.wasm>
import { readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const output = process.argv[2];
if (!output) {
  console.error('usage: node build.mjs <output.wasm>');
  process.exit(1);
}
const outputPath = resolve(output);
const source = await readFile(new URL('./library.js', import.meta.url), 'utf8');

// componentize-js reads the WIT through WASI, which can only open files under
// the directory current when it loads, so load it from wasm/.
process.chdir(fileURLToPath(new URL('../..', import.meta.url)));
const { componentize } = await import('@bytecodealliance/componentize-js');

const { component } = await componentize(source, {
  witPath: 'wit',
  worldName: 'library',
  // Leave out the WASI features the library does not use.
  disableFeatures: ['stdio', 'random', 'clocks', 'http', 'fetch-event'],
});
await writeFile(outputPath, component);
