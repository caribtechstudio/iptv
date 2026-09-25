import test from 'node:test';
import assert from 'node:assert/strict';
import { alternativesFor, channelKey, dedupeByUrl, facet, filterChannels, normalizeChannelName, resolutionOf } from '../src/catalog.js';

test('normalises names like the Rust core', () => {
  assert.equal(normalizeChannelName('TF1 HD (1080p) [Geo-blocked]'), 'tf1');
  assert.equal(normalizeChannelName('France 2 FHD'), 'france2');
  assert.equal(normalizeChannelName('Télé Matin 720p'), 'telematin');
  assert.equal(channelKey({ tvgId: 'M6.fr@HD', name: 'x' }), 'id:m6.fr');
  assert.equal(channelKey({ name: 'M6 HD' }), 'name:m6');
});

test('lists other sources of the same channel, healthy and sharp first', () => {
  const channels = [
    { name: 'TF1 (1080p)', tvgId: 'TF1.fr@SD', streamUrl: 'a', kind: 'live' },
    { name: 'TF1 HD (720p)', tvgId: 'TF1.fr@HD', streamUrl: 'b', kind: 'live' },
    { name: 'TF1', streamUrl: 'c', kind: 'live' },
    { name: 'TF1 Séries Films', tvgId: 'TF1SeriesFilms.fr', streamUrl: 'd', kind: 'live' },
  ];
  const health = { b: { ok: true }, c: { ok: false } };
  assert.deepEqual(alternativesFor(channels[0], channels, health).map((c) => c.streamUrl), ['a', 'b', 'c']);
  assert.deepEqual(alternativesFor({ ...channels[0], kind: 'movie' }, channels).length, 1);
  assert.equal(resolutionOf('M6 HD (1080p)'), 1080);
});

test('filters, deduplicates and counts facets', () => {
  const channels = [
    { name: 'Canal A', group: 'News', streamUrl: '1', country: 'fr' },
    { name: 'Canal B', group: 'Sport', streamUrl: '2', country: 'be' },
    { name: 'Canal A', group: 'News', streamUrl: '1', country: 'fr' },
  ];
  const unique = dedupeByUrl(channels);
  assert.equal(unique.length, 2);
  assert.deepEqual(filterChannels(unique, { country: 'be' }).map((c) => c.name), ['Canal B']);
  assert.deepEqual(filterChannels(unique, { hiddenGroups: ['News'] }).map((c) => c.name), ['Canal B']);
  assert.deepEqual(filterChannels(unique, { query: 'canal b' }).map((c) => c.name), ['Canal B']);
  assert.deepEqual(filterChannels(unique, { hideOffline: true, health: { 1: { ok: false } } }).map((c) => c.name), ['Canal B']);
  assert.deepEqual(facet(channels, 'country'), [['fr', 2], ['be', 1]]);
});
