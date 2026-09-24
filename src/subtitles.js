function seconds(timestamp) {
  const parts = timestamp.replace(',', '.').split(':').map(Number);
  if (parts.some((part) => !Number.isFinite(part)) || parts.length < 2 || parts.length > 3) return null;
  return parts.reduce((total, part) => total * 60 + part, 0);
}

export function parseSubtitles(input) {
  const blocks = input.replace(/^\uFEFF/, '').replace(/\r\n?/g, '\n').split(/\n\s*\n/);
  const cues = [];
  for (const block of blocks) {
    const lines = block.trim().split('\n');
    if (!lines.length || lines[0] === 'WEBVTT') continue;
    const index = lines.findIndex((line) => line.includes('-->'));
    if (index < 0) continue;
    const match = lines[index].match(/^\s*(\d{1,2}:)?\d{2}:\d{2}[.,]\d{3}\s*-->\s*(\d{1,2}:)?\d{2}:\d{2}[.,]\d{3}/);
    if (!match) continue;
    const [startText, endText] = match[0].split('-->').map((part) => part.trim());
    const start = seconds(startText); const end = seconds(endText);
    if (start === null || end === null || end <= start) continue;
    const text = lines.slice(index + 1).join('\n').replace(/<[^>]*>/g, '').trim();
    if (text) cues.push({ start, end, text });
  }
  return cues.sort((a, b) => a.start - b.start);
}
