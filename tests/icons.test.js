import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { hasIcon } from '../src/icons.js';

// An unknown name makes `hydrateIcons` throw and leaves the interface without its handlers.
test('every icon named by the interface exists', () => {
  const sources = ['main.js', 'multiview.js', 'filmstrip.js'].map((name) => readFileSync(new URL(`../src/${name}`, import.meta.url), 'utf8')).join('\n');
  const names = new Set();
  for (const pattern of [/data-icon="([\w-]+)"/g, /\b(?:icon|iconButton|emptyIcon)\(\s*'([\w-]+)'/g, /\)\s*,\s*'([\w-]+)'\)/g, /\[\s*'([\w-]+)',\s*'[^']*',\s*\(\)/g]) {
    for (const match of sources.matchAll(pattern)) names.add(match[1]);
  }
  // `toast(message, 'error')` has the same shape as `withIcon(button, 'name')`.
  names.delete('error');
  for (const literal of ['play-circle', 'search', 'plus', 'guide', 'volume-off', 'volume-low', 'volume-high', 'pause', 'fullscreen-exit', 'player-mode-exit', 'pin-off']) names.add(literal);
  assert.ok(names.size > 30, `found ${names.size} names`);
  const missing = [...names].filter((name) => !hasIcon(name));
  assert.deepEqual(missing, []);
});
