// Lucide icons (https://lucide.dev), imported one by one so the bundle only carries those used.
// `icon('play')` builds an SVG; `<i data-icon="play"></i>` placeholders in HTML templates are
// replaced by `hydrateIcons()`; `setIcon(button, 'pause')` swaps the icon of a button.

import {
  Activity, Airplay, ArrowDown, ArrowRightLeft, ArrowUp, CalendarDays, Captions, Check, ChevronDown,
  ChevronLeft, ChevronRight, CircleAlert, CircleCheck, CircleDot, CirclePlay, Clock, Download,
  Ellipsis, FileVideo, Film, ListPlus, FolderOpen, Gauge, History, Info, LayoutGrid, Link, ListChecks, LoaderCircle,
  Maximize, Maximize2, Minimize, Minimize2, MonitorPlay, Pause, Pencil, PictureInPicture2, Pin,
  PinOff, Play, Plus, RefreshCw, RotateCcw, RotateCw, Search, Settings, SlidersHorizontal, Square,
  Star, Trash2, Tv, Volume1, Volume2, VolumeX, X,
} from 'lucide';

const SVG = 'http://www.w3.org/2000/svg';

/** Arrow around « 10 »: the usual « jump 10 seconds » symbol, drawn from the Lucide arrow. */
function jump(node) {
  return [...node, ['text', { x: '12', y: '15.2', 'text-anchor': 'middle', 'font-size': '7.5', 'font-weight': '700', fill: 'currentColor', stroke: 'none', 'font-family': '-apple-system, system-ui, sans-serif' }, '10']];
}

const ICONS = {
  activity: Activity, airplay: Airplay, 'arrow-down': ArrowDown, 'arrow-up': ArrowUp, back10: jump(RotateCcw),
  captions: Captions, check: Check, 'chevron-down': ChevronDown, 'chevron-left': ChevronLeft,
  'chevron-right': ChevronRight, 'circle-alert': CircleAlert, 'circle-check': CircleCheck, clock: Clock,
  convert: ArrowRightLeft, download: Download, edit: Pencil, ellipsis: Ellipsis, 'file-video': FileVideo, film: Film,
  'list-plus': ListPlus,
  filters: SlidersHorizontal, 'folder-open': FolderOpen, forward10: jump(RotateCw), fullscreen: Maximize,
  'fullscreen-exit': Minimize, gauge: Gauge, grid: LayoutGrid, guide: CalendarDays, history: History,
  info: Info, link: Link, 'list-checks': ListChecks, loader: LoaderCircle, 'player-mode': Maximize2,
  'player-mode-exit': Minimize2, multiview: MonitorPlay, pause: Pause, pin: Pin, 'pin-off': PinOff,
  pip: PictureInPicture2, play: Play, 'play-circle': CirclePlay, plus: Plus, record: CircleDot,
  refresh: RefreshCw, resume: RotateCcw, search: Search, settings: Settings, star: Star, stop: Square,
  trash: Trash2, tv: Tv, 'volume-high': Volume2, 'volume-low': Volume1, 'volume-off': VolumeX, x: X,
};

function build(tag, attrs, parent, text) {
  const el = document.createElementNS(SVG, tag);
  for (const [key, value] of Object.entries(attrs)) el.setAttribute(key, String(value));
  if (text !== undefined) el.textContent = text;
  parent.append(el);
  return el;
}

/** An inline SVG icon; `size` in pixels (defaults to the font size through CSS). */
export function icon(name, { size, className = '', label } = {}) {
  const node = ICONS[name];
  if (!node) throw new Error(`Icône inconnue : ${name}`);
  const svg = document.createElementNS(SVG, 'svg');
  const attrs = { xmlns: SVG, viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', 'stroke-width': 2, 'stroke-linecap': 'round', 'stroke-linejoin': 'round', class: `icon icon-${name}${className ? ` ${className}` : ''}` };
  if (size) Object.assign(attrs, { width: size, height: size });
  for (const [key, value] of Object.entries(attrs)) svg.setAttribute(key, String(value));
  if (label) { svg.setAttribute('role', 'img'); svg.setAttribute('aria-label', label); }
  else svg.setAttribute('aria-hidden', 'true');
  for (const [tag, props, text] of node) build(tag, props, svg, text);
  return svg;
}

export function hasIcon(name) { return Object.hasOwn(ICONS, name); }

/** Replaces the icon of `el` (its first SVG, or prepends one) without touching its text. */
export function setIcon(el, name, options) {
  if (!el) return;
  const current = el.querySelector(':scope > svg.icon');
  if (current?.classList.contains(`icon-${name}`)) return;
  const next = icon(name, options);
  if (current) current.replaceWith(next); else el.prepend(next);
}

/** Replaces every `<i data-icon="name">` placeholder below `root`. */
export function hydrateIcons(root = document) {
  for (const el of root.querySelectorAll('i[data-icon]')) {
    const svg = icon(el.dataset.icon, { className: el.className });
    el.replaceWith(svg);
  }
}

/** A button holding an icon, with the label as tooltip and accessible name. */
export function iconButton(name, label, className, action) {
  const el = document.createElement('button');
  el.type = 'button';
  if (className) el.className = className;
  el.title = label;
  el.setAttribute('aria-label', label);
  el.append(icon(name));
  if (action) el.onclick = action;
  return el;
}
