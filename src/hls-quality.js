export function isHlsMediaUrl(value) {
  try {
    const url = new URL(value);
    return ['http:', 'https:'].includes(url.protocol) && /\.m3u8$/i.test(url.pathname);
  } catch { return false; }
}

function attributes(line) {
  const result = {};
  for (const match of line.matchAll(/([A-Z0-9-]+)=("[^"]*"|[^,]*)/g)) {
    result[match[1]] = match[2].startsWith('"') ? match[2].slice(1, -1) : match[2];
  }
  return result;
}

export function parseHlsQualities(text, baseUrl) {
  const lines = text.replace(/^\uFEFF/, '').split(/\r?\n/).map((line) => line.trim());
  if (lines[0] !== '#EXTM3U') return [];

  const externalGroups = new Set();
  for (const line of lines) {
    if (!line.startsWith('#EXT-X-MEDIA:')) continue;
    const media = attributes(line.slice('#EXT-X-MEDIA:'.length));
    if (media.URI && media['GROUP-ID']) externalGroups.add(`${media.TYPE}:${media['GROUP-ID']}`);
  }

  const variants = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (!lines[index].startsWith('#EXT-X-STREAM-INF:')) continue;
    const info = attributes(lines[index].slice('#EXT-X-STREAM-INF:'.length));
    let next = index + 1;
    while (next < lines.length && !lines[next]) next += 1;
    const uri = lines[next];
    if (!uri || uri.startsWith('#')) continue;
    let url;
    try {
      url = new URL(uri, baseUrl);
      if (!['http:', 'https:'].includes(url.protocol)) continue;
    } catch { continue; }
    const resolution = /^(\d+)x(\d+)$/i.exec(info.RESOLUTION || '');
    const height = resolution ? Number(resolution[2]) : 0;
    if (!height && info.CODECS && !/(?:avc|hev|hvc|av01|vp\d|mp4v)/i.test(info.CODECS)) continue;
    const bandwidth = Number(info['AVERAGE-BANDWIDTH'] || info.BANDWIDTH) || 0;
    const separateTracks = ['AUDIO', 'VIDEO', 'SUBTITLES'].some((type) =>
      info[type] && externalGroups.has(`${type}:${info[type]}`));
    variants.push({ url: url.href, height, bandwidth, selectable: !separateTracks });
  }

  variants.sort((a, b) => b.height - a.height || b.bandwidth - a.bandwidth);
  const labelCounts = new Map();
  for (const variant of variants) {
    const base = variant.height >= 2160 && variant.height <= 2304
      ? '4K (2160p)'
      : variant.height ? `${variant.height}p` : variant.bandwidth ? `${(variant.bandwidth / 1_000_000).toFixed(1)} Mb/s` : 'Qualité';
    labelCounts.set(base, (labelCounts.get(base) || 0) + 1);
    variant.label = base;
  }
  for (const variant of variants) {
    if (labelCounts.get(variant.label) > 1 && variant.bandwidth) {
      variant.label += ` · ${(variant.bandwidth / 1_000_000).toFixed(1)} Mb/s`;
    }
  }
  return variants;
}
