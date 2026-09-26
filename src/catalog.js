// Catalogue helpers shared by the list, the guide and source failover.
// normalizeChannelName follows crates/iptv-core/src/names.rs.

const QUALITY_TOKENS = new Set(['hd', 'fhd', 'uhd', 'sd', '4k', '8k', 'hevc', 'h264', 'h265', 'backup', 'raw', 'hq', 'lq']);

export function normalizeChannelName(name = '') {
  let depth = 0;
  let cleaned = '';
  for (const ch of String(name)) {
    if (ch === '(' || ch === '[') depth += 1;
    else if (ch === ')' || ch === ']') depth = Math.max(0, depth - 1);
    else if (depth === 0) cleaned += ch;
  }
  return cleaned.toLowerCase().normalize('NFD').replace(/[̀-ͯ]/g, '')
    .split(/[^\p{L}\p{N}]+/u)
    .filter((token) => token && !QUALITY_TOKENS.has(token) && !/^\d{3,}p$/.test(token))
    .join('');
}

export function baseTvgId(id = '') { return String(id).split('@')[0].trim().toLowerCase(); }

// Keys are computed once per channel object: zapping compares the channel with the whole
// catalogue, and normalising 100 000 names at every change of channel took a quarter second.
const keyCache = new WeakMap();
function keysOf(channel) {
  let keys = keyCache.get(channel);
  if (!keys) {
    const name = `name:${normalizeChannelName(channel.name)}`;
    const id = channel.tvgId && baseTvgId(channel.tvgId);
    keys = { key: id ? `id:${id}` : name, name };
    keyCache.set(channel, keys);
  }
  return keys;
}

/** Key grouping the sources of one channel: guide id without feed, otherwise its name. */
export function channelKey(channel) { return keysOf(channel).key; }

export function dedupeByUrl(channels) {
  const seen = new Set();
  return channels.filter((channel) => !seen.has(channel.streamUrl) && seen.add(channel.streamUrl));
}

export function resolutionOf(name = '') {
  const match = /(\d{3,4})p/i.exec(name);
  if (match) return Number(match[1]);
  if (/\b(4k|uhd)\b/i.test(name)) return 2160;
  if (/\bfhd\b/i.test(name)) return 1080;
  if (/\bhd\b/i.test(name)) return 720;
  return 0;
}

function healthRank(health, url) {
  const entry = health?.[url];
  if (!entry) return 1;
  return entry.ok ? 0 : 2;
}

/** The channel first, then the other sources of the same channel, healthy and sharp first. */
export function alternativesFor(channel, channels, health = {}, limit = 6) {
  if (channel.kind && channel.kind !== 'live') return [channel];
  const { key, name } = keysOf(channel);
  const others = channels.filter((item) => {
    if (item.streamUrl === channel.streamUrl || (item.kind && item.kind !== 'live')) return false;
    const keys = keysOf(item);
    return keys.key === key || keys.name === name;
  });
  others.sort((a, b) => healthRank(health, a.streamUrl) - healthRank(health, b.streamUrl)
    || resolutionOf(b.name) - resolutionOf(a.name));
  return [channel, ...dedupeByUrl(others)].slice(0, limit);
}

const searchCache = new WeakMap();
export function searchText(channel) {
  let text = searchCache.get(channel);
  if (!text) {
    text = `${channel.name} ${channel.group}`.toLocaleLowerCase('fr').normalize('NFD').replace(/[̀-ͯ]/g, '');
    searchCache.set(channel, text);
  }
  return text;
}

export function normalizeQuery(query = '') {
  return query.trim().toLocaleLowerCase('fr').normalize('NFD').replace(/[̀-ͯ]/g, '');
}

export function isOffline(channel, health) {
  return health?.[channel.streamUrl]?.ok === false;
}

/** Filters shared by every list view. */
export function filterChannels(channels, { group = 'Tous', query = '', country = '', language = '', hiddenGroups = [], hideOffline = false, health = {}, keepHidden = false } = {}) {
  const needle = normalizeQuery(query);
  const hidden = new Set(hiddenGroups);
  return channels.filter((item) => (keepHidden || !hidden.has(item.group))
    && (group === 'Tous' || item.group === group)
    && (!country || item.country === country)
    && (!language || item.language === language)
    && (!hideOffline || !isOffline(item, health))
    && (!needle || searchText(item).includes(needle)));
}

export function flag(code = '') {
  if (!/^[a-z]{2}$/i.test(code)) return '';
  return String.fromCodePoint(...code.toUpperCase().split('').map((ch) => 0x1f1e6 + ch.charCodeAt(0) - 65));
}

let regionNames;
export function countryLabel(code = '') {
  try {
    regionNames ??= new Intl.DisplayNames(['fr'], { type: 'region' });
    return `${flag(code)} ${regionNames.of(code.toUpperCase())}`.trim();
  } catch { return code.toUpperCase(); }
}

/** Distinct values with counts, most frequent first. */
export function facet(channels, field) {
  const counts = new Map();
  for (const channel of channels) if (channel[field]) counts.set(channel[field], (counts.get(channel[field]) || 0) + 1);
  return [...counts].sort((a, b) => b[1] - a[1] || String(a[0]).localeCompare(String(b[0]), 'fr'));
}
