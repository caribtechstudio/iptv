// EPG grid: channels in rows, programmes positioned on a horizontal timeline.

export const PX_PER_MINUTE = 4;

export function gridWindow(now, hours = 6) {
  const from = Math.floor(now / 1800) * 1800 - 1800;
  return { from, to: from + hours * 3600 };
}

export function programBox(program, from, to) {
  const start = Math.max(program.start, from);
  const stop = Math.min(program.stop, to);
  return {
    left: Math.round((start - from) / 60 * PX_PER_MINUTE),
    width: Math.max(0, Math.round((stop - start) / 60 * PX_PER_MINUTE)),
  };
}

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

const clock = (seconds) => new Date(seconds * 1000).toLocaleTimeString('fr-FR', { hour: '2-digit', minute: '2-digit' });

/**
 * rows: [{ channel, programs }]; onProgram(channel, program, state) where state is
 * 'live' | 'past' | 'future'; canReplay(channel, program) tells if catch-up is possible.
 */
export function renderGuideGrid(container, { rows, from, to, now, onProgram, onChannel, canReplay }) {
  container.replaceChildren();
  const width = (to - from) / 60 * PX_PER_MINUTE;
  const grid = el('div', 'gg');
  grid.style.setProperty('--gg-width', `${width}px`);
  const head = el('div', 'gg-row gg-head');
  head.append(el('div', 'gg-channel gg-corner', 'Chaînes'));
  const ticks = el('div', 'gg-track');
  for (let time = from; time < to; time += 1800) {
    const tick = el('span', 'gg-tick', clock(time));
    tick.style.left = `${(time - from) / 60 * PX_PER_MINUTE}px`;
    ticks.append(tick);
  }
  head.append(ticks);
  grid.append(head);
  for (const { channel, programs } of rows) {
    const row = el('div', 'gg-row');
    const name = el('button', 'gg-channel');
    name.title = channel.name;
    if (channel.logo?.startsWith('http')) {
      const img = document.createElement('img');
      img.src = channel.logo; img.alt = ''; img.onerror = () => img.remove();
      name.append(img);
    }
    name.append(el('span', '', channel.name));
    name.onclick = () => onChannel(channel);
    const track = el('div', 'gg-track');
    for (const program of programs) {
      const { left, width: size } = programBox(program, from, to);
      if (size < 2) continue;
      const state = program.stop <= now ? 'past' : program.start <= now ? 'live' : 'future';
      const replay = state === 'past' && canReplay(channel, program);
      const block = el('button', `gg-program ${state}${replay ? ' replay' : ''}`);
      block.style.left = `${left}px`;
      block.style.width = `${size - 2}px`;
      block.append(el('strong', '', program.title), el('small', '', `${clock(program.start)} – ${clock(program.stop)}${replay ? ' · replay' : ''}`));
      block.title = `${program.title}\n${clock(program.start)} – ${clock(program.stop)}${program.description ? `\n\n${program.description}` : ''}`;
      block.onclick = () => onProgram(channel, program, state);
      track.append(block);
    }
    row.append(name, track);
    grid.append(row);
  }
  const line = el('div', 'gg-now');
  line.style.left = `calc(var(--gg-channel-width) + ${(now - from) / 60 * PX_PER_MINUTE}px)`;
  grid.append(line);
  container.append(grid);
  container.scrollLeft = Math.max(0, (now - from) / 60 * PX_PER_MINUTE - 120);
}
