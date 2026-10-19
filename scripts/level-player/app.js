'use strict';

const $ = id => document.getElementById(id);
const directions = { U: [0, -1, 1], D: [0, 1, 2], L: [-1, 0, 4], R: [1, 0, 8] };
let levels = [], selected = null, player = null, boxes = new Map(), history = [], moves = 0, pushes = 0;
let loading = false;

function identity(level) {
  return JSON.stringify([level.id, level.steps, level.player, level.exit, level.walls, level.boxes, level.solution]);
}

function loadLevel(level) {
  selected = structuredClone(level);
  player = level.player;
  boxes = new Map(level.boxes.map(box => [box.position, box.mask]));
  history = []; moves = 0; pushes = 0;
  $('empty').hidden = true;
  $('level-title').textContent = `Root ${level.id}`;
  $('prefix').textContent = level.prefix;
  const x = level.exit % level.width + 1, y = Math.floor(level.exit / level.width) + 1;
  $('level-meta').textContent = `${level.width} × ${level.height} · Exit ${x},${y}`;
  $('record').textContent = level.steps;
  $('new-record').hidden = true;
  renderRoots(); renderBoard();
}

function renderRoots() {
  const query = $('filter').value.trim().toUpperCase();
  const visible = levels.filter(level => String(level.id).includes(query) || level.prefix.includes(query));
  $('level-count').textContent = `${levels.length} ${levels.length === 1 ? 'level' : 'levels'}`;
  $('roots').replaceChildren(...visible.map(level => {
    const button = document.createElement('button');
    button.className = 'root' + (selected?.id === level.id ? ' selected' : '');
    button.setAttribute('aria-current', selected?.id === level.id ? 'true' : 'false');
    const top = document.createElement('span'); top.className = 'root-top';
    const title = document.createElement('span'); title.textContent = `Root ${level.id}`;
    const score = document.createElement('span'); score.className = 'root-score'; score.textContent = `${level.steps} moves`;
    top.append(title, score);
    const prefix = document.createElement('div'); prefix.className = 'root-prefix'; prefix.textContent = level.prefix;
    button.append(top, prefix);
    button.addEventListener('click', () => { loadLevel(level); button.blur(); });
    return button;
  }));
}

function renderBoard() {
  if (!selected) return;
  const { width, height, exit, walls } = selected;
  const board = $('board');
  board.style.gridTemplateColumns = `repeat(${width}, minmax(0, 1fr))`;
  board.style.gridTemplateRows = `repeat(${height}, minmax(0, 1fr))`;
  board.style.setProperty('--board-ratio', `${width} / ${height}`);
  board.style.setProperty('--ratio-number', width / height);
  board.style.setProperty('--board-width', `${Math.min(620, width * 86)}px`);
  const cells = [];
  for (let position = 0; position < width * height; position++) {
    const cell = document.createElement('div'); cell.className = 'cell'; cell.dataset.position = position;
    let name = 'Floor';
    if (walls.includes(position)) { cell.classList.add('wall'); name = 'Wall'; }
    if (position === exit) { cell.classList.add('exit'); name = 'Exit'; }
    if (boxes.has(position)) {
      cell.classList.add('box');
      const mask = boxes.get(position); name = `Box, push ${Object.keys(directions).filter(dir => mask & directions[dir][2]).join(' ') || 'none'}`;
      // A push up requires the player below the box, and likewise for other directions.
      for (const [bit, side] of [[1, 'bottom'], [2, 'top'], [4, 'right'], [8, 'left']]) {
        if (mask & bit) { const edge = document.createElement('span'); edge.className = `edge ${side}`; cell.append(edge); }
      }
    }
    if (position === player) {
      const token = document.createElement('span'); token.className = 'player'; cell.append(token); name = position === exit ? 'Player at exit' : 'Player';
    }
    if (position === player || (position === exit && !boxes.has(position))) {
      const number = document.createElement('span'); number.className = 'number'; number.textContent = '0'; cell.append(number);
    }
    cell.setAttribute('aria-label', `${position % width + 1},${Math.floor(position / width) + 1}: ${name}`);
    cells.push(cell);
  }
  board.replaceChildren(...cells);
  $('moves').textContent = moves;
  $('pushes').textContent = pushes;
  $('result').textContent = player === exit ? 'Solved' : '';
  $('undo').disabled = history.length === 0;
  $('restart').disabled = moves === 0;
  for (const button of document.querySelectorAll('[data-direction]')) button.disabled = player === exit;
}

function neighbor(position, dx, dy) {
  const x = position % selected.width + dx, y = Math.floor(position / selected.width) + dy;
  return x >= 0 && x < selected.width && y >= 0 && y < selected.height ? y * selected.width + x : null;
}

function move(direction) {
  if (!selected || player === selected.exit) return false;
  const [dx, dy, bit] = directions[direction], dest = neighbor(player, dx, dy);
  if (dest === null || selected.walls.includes(dest)) return false;
  let beyond = null;
  if (boxes.has(dest)) {
    beyond = neighbor(dest, dx, dy);
    if (!(boxes.get(dest) & bit) || beyond === null || selected.walls.includes(beyond) || boxes.has(beyond)) return false;
  }
  history.push({ player, boxes: new Map(boxes), pushes });
  if (beyond !== null) { boxes.set(beyond, boxes.get(dest)); boxes.delete(dest); pushes++; }
  player = dest; moves++; renderBoard(); return true;
}

function undo() {
  const previous = history.pop();
  if (!previous) return;
  player = previous.player; boxes = previous.boxes; pushes = previous.pushes; moves--; renderBoard();
}

async function refresh() {
  if (loading) return;
  loading = true;
  try {
    const response = await fetch('/api/levels', { cache: 'no-store' });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const data = await response.json(); levels = data.levels;
    if (!selected && levels.length) loadLevel(levels[0]);
    else renderRoots();
    const latest = selected && levels.find(level => level.id === selected.id);
    $('new-record').hidden = !latest || identity(latest) === identity(selected);
    $('connection').textContent = data.warnings.length ? 'Some records are being updated' : `Updated ${new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`;
    $('connection').title = data.source + (data.skipped ? `\n${data.skipped} roots without concrete witnesses` : '') + (data.warnings.length ? '\n' + data.warnings.join('\n') : '');
    if (!levels.length && !selected) $('empty').textContent = 'No concrete root levels saved yet.';
  } catch (error) {
    $('connection').textContent = 'Disconnected'; $('connection').title = error.message;
    if (!selected) $('empty').textContent = 'Could not load records.';
  } finally { loading = false; }
}

document.addEventListener('keydown', event => {
  if (event.ctrlKey || event.metaKey || event.altKey || /^(INPUT|TEXTAREA|SELECT)$/.test(event.target.tagName)) return;
  const key = event.key.toLowerCase();
  const direction = { arrowup: 'U', w: 'U', arrowdown: 'D', s: 'D', arrowleft: 'L', a: 'L', arrowright: 'R', d: 'R' }[key];
  if (direction) { event.preventDefault(); move(direction); }
  else if (key === 'u' || key === 'z') { event.preventDefault(); undo(); }
  else if (key === 'r' && selected) { event.preventDefault(); loadLevel(selected); }
});
$('undo').addEventListener('click', undo);
$('restart').addEventListener('click', () => selected && loadLevel(selected));
$('new-record').addEventListener('click', () => { const latest = levels.find(level => level.id === selected?.id); if (latest) loadLevel(latest); });
$('filter').addEventListener('input', renderRoots);
$('refresh').addEventListener('click', refresh);
for (const button of document.querySelectorAll('[data-direction]')) button.addEventListener('click', () => move(button.dataset.direction));
lucide.createIcons();
refresh(); setInterval(refresh, 30000);
