export function isWebUrl(value) {
  try { return ['http:', 'https:'].includes(new URL(value).protocol); }
  catch { return false; }
}

export function isYouTubePage(value) {
  if (!isWebUrl(value)) return false;
  return ['youtube.com', 'www.youtube.com', 'm.youtube.com', 'youtu.be'].includes(new URL(value).hostname.toLowerCase());
}
