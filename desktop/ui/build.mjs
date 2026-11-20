import { build } from 'esbuild';
await build({
  entryPoints: ['src/app.js'],
  outfile: '../dist/app.js',
  bundle: true,
  minify: true,
  legalComments: 'inline',
  target: 'chrome110',
  loader: { '.svg': 'text', '.woff2': 'dataurl' },
});
