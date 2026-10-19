const assert = require('node:assert/strict');
const path = require('node:path');
const fs = require('node:fs');
let playwright;
try { playwright = require('playwright'); }
catch {
  playwright = require(path.join(process.env.USERPROFILE, '.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright'));
}
const { chromium } = playwright;
const output = path.join(__dirname, '../scratch/level-player-check');
fs.mkdirSync(output, { recursive: true });

(async () => {
  const browser = await chromium.launch({ headless: true, channel: 'msedge' });
  try {
    const page = await browser.newPage({ viewport: { width: 1280, height: 960 } });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto(process.env.LEVEL_PLAYER_URL || 'http://127.0.0.1:8765');
    await page.locator('.root').first().waitFor();
    const levels = await page.evaluate(async () => (await (await fetch('/api/levels')).json()).levels);
    assert.ok(levels.length > 0);
    for (const level of levels) {
      const result = await page.evaluate(level => {
        loadLevel(level);
        const applied = [...level.solution].every(direction => move(direction));
        const solved = document.getElementById('result').textContent;
        const steps = Number(document.getElementById('moves').textContent);
        undo();
        const undone = document.getElementById('result').textContent === '';
        move(level.solution.at(-1));
        return { applied, solved, steps, undone, solvedAgain: document.getElementById('result').textContent };
      }, level);
      assert.deepEqual(result, { applied: true, solved: 'Solved', steps: level.steps, undone: true, solvedAgain: 'Solved' });
    }
    await page.locator('.root').first().click();
    const first = levels[0];
    const keys = { U: 'ArrowUp', D: 's', L: 'a', R: 'ArrowRight' };
    await page.keyboard.press(keys[first.solution[0]]);
    assert.equal(await page.locator('#moves').textContent(), '1');
    await page.keyboard.press('z');
    assert.equal(await page.locator('#moves').textContent(), '0');
    await page.keyboard.press(keys[first.solution[0]]);
    await page.keyboard.press('u');
    assert.equal(await page.locator('#moves').textContent(), '0');
    await page.keyboard.press(keys[first.solution[0]]);
    await page.keyboard.press('r');
    assert.equal(await page.locator('#moves').textContent(), '0');
    await page.locator('#filter').fill(String(levels.at(-1).id));
    await page.locator('.root').last().click();
    assert.equal(await page.locator('#level-title').textContent(), `Root ${levels.at(-1).id}`);
    await page.locator('#filter').fill('');
    await page.locator('#filter').blur();
    const synthetic = { id: 999, prefix: 'U', status: 'running', steps: 3, solution: 'RRR', width: 5, height: 1, exit: 3, player: 0, walls: [], boxes: [{ position: 1, mask: 8 }] };
    await page.evaluate(level => loadLevel(level), synthetic);
    assert.equal(await page.locator('.edge.left').count(), 1);
    await page.keyboard.press('d');
    assert.equal(await page.locator('#pushes').textContent(), '1');
    await page.keyboard.press('z');
    assert.equal(await page.locator('#pushes').textContent(), '0');
    await page.evaluate(level => loadLevel({ ...level, boxes: [{ position: 1, mask: 1 }] }), synthetic);
    await page.keyboard.press('d');
    assert.equal(await page.locator('#moves').textContent(), '0');
    await page.evaluate(level => loadLevel(level), first);
    for (const width of [1280, 390, 320]) {
      await page.setViewportSize({ width, height: 960 });
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false, `Overflow at ${width}px`);
      const bounds = await page.locator('#board').boundingBox();
      assert.ok(bounds.width > 200 && bounds.height > 200);
      assert.ok(await page.locator('#undo svg').count());
      await page.screenshot({ path: path.join(output, `level-player-${width}.png`), fullPage: true });
    }
    assert.deepEqual(errors, []);
    console.log(`Passed ${levels.length} saved solutions, win/undo, masks, keyboard, restart, sidebar and desktop/mobile layouts.`);
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
