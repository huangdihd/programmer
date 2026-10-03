// npm exec --offline --package=playwright -- sh -c 'NODE_PATH="$(dirname "$(dirname "$(command -v playwright)")")" node design-demos/architecture-ownership.test.cjs'
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const { chromium } = require('playwright');
const root = path.resolve(__dirname, '..');

function structBody(source, symbol) {
  const match = new RegExp(`\\bstruct\\s+${symbol}\\b[^\\{]*\\{`).exec(source);
  assert.ok(match, `Missing struct ${symbol}`);
  const start = match.index + match[0].length;
  let depth = 1;
  for (let index = start; index < source.length; index++) {
    if (source[index] === '{') depth++;
    if (source[index] === '}' && --depth === 0) return source.slice(start, index);
  }
  throw new Error(`Unclosed struct ${symbol}`);
}

async function main() {
  const browser = await chromium.launch();
  const errors = [];
  const network = [];
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1100 }, offline: true });
    const page = await context.newPage();
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    page.on('request', request => { if (/^https?:/.test(request.url())) network.push(request.url()); });
    await page.goto(pathToFileURL(path.join(__dirname, 'architecture-ownership.html')).href);
    const data = await page.evaluate(() => window.architecture);
    assert.equal(data.nodes.length, 15);
    assert.equal(new Set(data.nodes.map(node => node.id)).size, data.nodes.length);
    for (const node of data.nodes) {
      const source = fs.readFileSync(path.join(root, node.source), 'utf8');
      const body = structBody(source, node.symbol);
      for (const field of node.fields) assert.match(body, new RegExp(`\\b${field}\\s*:`), `${node.symbol}.${field} must still exist`);
    }
    for (const edge of data.edges) {
      assert.ok(data.positions[edge.view][edge.from], `Missing ${edge.from}`);
      assert.ok(data.positions[edge.view][edge.to], `Missing ${edge.to}`);
      const source = fs.readFileSync(path.join(root, edge.source), 'utf8');
      assert.ok(source.includes(edge.evidence), `Stale edge source: ${edge.source} :: ${edge.evidence}`);
      assert.ok(edge.label.length > 0);
    }
    const runner = fs.readFileSync(path.join(root, 'src/runner/mod.rs'), 'utf8');
    assert.doesNotMatch(structBody(runner, 'TurnRunner'), /\b(?:App|UiState)\b/);
    // Integration tests exercise TuiSurface; only production code is UI-independent.
    assert.doesNotMatch(runner.split('#[cfg(test)]\nmod tests')[0], /(?:use\s+crate::(?:app|ui)|crate::(?:app|ui)::)/);
    for (const view of ['ownership', 'execution']) {
      await page.locator(`[data-view="${view}"]`).click();
      assert.equal(await page.locator('.node').count(), Object.keys(data.positions[view]).length);
      assert.equal(await page.locator('.edge').count(), data.edges.filter(edge => edge.view === view).length);
      const geometryProblems = await page.evaluate(() => {
        const problems = [];
        const nodes = [...document.querySelectorAll('.node')].map(node => ({ id: node.dataset.node, rectangle: node.getBoundingClientRect() }));
        const inside = (point, rectangle) => point.x > rectangle.left + 1 && point.x < rectangle.right - 1 && point.y > rectangle.top + 1 && point.y < rectangle.bottom - 1;
        const onBoundary = (point, rectangle) => point.x >= rectangle.left - 1 && point.x <= rectangle.right + 1 && point.y >= rectangle.top - 1 && point.y <= rectangle.bottom + 1 && !inside(point, rectangle);
        for (const edge of document.querySelectorAll('.edge')) {
          const path = edge.querySelector('path');
          const matrix = path.getScreenCTM();
          const length = path.getTotalLength();
          const first = path.getPointAtLength(0).matrixTransform(matrix);
          const last = path.getPointAtLength(length).matrixTransform(matrix);
          if (!onBoundary(first, nodes.find(node => node.id === edge.dataset.from).rectangle)) problems.push(`Start: ${edge.dataset.from} → ${edge.dataset.to}`);
          if (!onBoundary(last, nodes.find(node => node.id === edge.dataset.to).rectangle)) problems.push(`End: ${edge.dataset.from} → ${edge.dataset.to}`);
          for (let position = 2; position < length - 2; position += 3) {
            const point = path.getPointAtLength(position).matrixTransform(matrix);
            if (nodes.some(node => inside(point, node.rectangle))) {
              problems.push(`Path crosses node: ${edge.dataset.from} → ${edge.dataset.to}`); break;
            }
          }
          const label = edge.querySelector('text').getBoundingClientRect();
          for (const node of nodes) {
            const rectangle = node.rectangle;
            if (label.left < rectangle.right && label.right > rectangle.left && label.top < rectangle.bottom && label.bottom > rectangle.top) problems.push(`Label overlaps ${node.id}: ${edge.dataset.from} → ${edge.dataset.to}`);
          }
          if (getComputedStyle(path).markerEnd === 'none') problems.push('Arrow missing');
          const expectedDash = edge.classList.contains('own') ? 'none' : '6px, 5px';
          if (getComputedStyle(path).strokeDasharray !== expectedDash) problems.push('Wrong line kind');
        }
        return problems;
      });
      assert.deepEqual(geometryProblems, [], `${view}: diagram geometry`);
      for (const id of Object.keys(data.positions[view])) {
        await page.locator(`[data-node="${id}"]`).click();
        const node = data.nodes.find(node => node.id === id);
        assert.equal(await page.locator('#detail-title').textContent(), node.name);
        assert.ok((await page.locator('#detail-sources').textContent()).includes(node.source));
        assert.equal(await page.locator('.node[aria-pressed="true"]').count(), 1);
      }
      await page.locator('[data-node="app"]').focus();
      await page.keyboard.press('End');
      const lastId = Object.keys(data.positions[view]).at(-1);
      assert.equal(await page.locator('.node:focus').getAttribute('data-node'), lastId);
      await page.keyboard.press('Home');
      assert.equal(await page.locator('#detail-title').textContent(), 'App');
      await page.keyboard.press('ArrowRight');
      await page.keyboard.press('Enter');
      await page.locator('#node-picker').selectOption(view === 'ownership' ? 'session' : 'runner');
      await page.evaluate(() => { document.querySelector('.graph-frame').scrollTo(0, 0); window.scrollTo(0, 0); });
      await page.screenshot({ path: path.join(__dirname, `architecture-ownership-${view}.png`), fullPage: true });
    }
    await page.setViewportSize({ width: 390, height: 844 });
    for (const view of ['ownership', 'execution']) {
      await page.locator(`[data-view="${view}"]`).click();
      await page.locator('#node-picker').selectOption(view === 'ownership' ? 'peer' : 'registry');
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth));
      assert.ok(await page.evaluate(() => document.querySelector('.graph-frame').scrollLeft > 0));
      await page.evaluate(() => window.scrollTo(0, 0));
      await page.screenshot({ path: path.join(__dirname, `architecture-ownership-${view}-mobile.png`), fullPage: true });
    }
    await page.reload();
    assert.equal(await page.locator('[data-view="ownership"]').getAttribute('aria-pressed'), 'true');
    assert.equal(await page.locator('#detail-title').textContent(), 'SessionState');
    assert.equal(await page.evaluate(() => localStorage.length), 0);
    assert.deepEqual(errors, []);
    assert.deepEqual(network, []);
    console.log(`PASS: ${data.nodes.length} source-backed nodes, ${data.edges.length} directed edges; exact struct fields, source evidence, endpoints, no node/label obstruction, click/keyboard/select/reload, 390px + 1440px, zero console/page errors, zero network. Four screenshots saved.`);
  } finally {
    await browser.close();
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
