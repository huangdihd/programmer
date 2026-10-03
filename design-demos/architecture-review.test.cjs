// Offline preview regression. Uses the same cached Playwright setup as sibling previews.
// npm exec --offline --package=playwright -- sh -c 'NODE_PATH="$(dirname "$(dirname "$(command -v playwright)")")" node design-demos/architecture-review.test.cjs'
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const { chromium } = require('playwright');

async function main() {
  const browser = await chromium.launch();
  const failures = [];
  try {
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
    page.on('pageerror', error => failures.push(error.message));
    page.on('request', request => {
      assert.ok(!/^https?:/.test(request.url()), 'Preview must not request the network');
    });
    const location = pathToFileURL(path.resolve(__dirname, 'architecture-review.html')).href;
    await page.goto(location);
    assert.equal(await page.locator('#findings article').count(), 6);
    assert.equal(await page.locator('#findings #risk-filter').count(), 0);
    assert.equal(await page.locator('#implementation-risks #risk-filter').count(), 1);
    assert.ok(!(await page.locator('body').textContent()).includes('\uFFFD'), 'No corrupted text');
    const dataset = await page.evaluate(() => window.architectureReview);
    assert.equal(dataset.modules.length, 31); // 30 main.rs modules plus the main composition root.
    assert.equal(new Set(dataset.modules.map(module => module.id)).size, 31);
    assert.equal(dataset.flows.length, 8);
    assert.equal(dataset.findings.length, 16);
    assert.equal(dataset.sourceInventory.length, 169);
    const declarations = require('node:child_process').execFileSync('git', ['show', '0715627:src/main.rs'], { cwd: path.resolve(__dirname, '..'), encoding: 'utf8' })
      .match(/^mod (\w+);$/gm).map(line => line.slice(4, -1));
    assert.equal(declarations.length, 30);
    for (const name of declarations) {
      assert.ok(dataset.modules.some(module => module.id === name), `Missing module ${name}`);
    }
    const sources = dataset.modules.flatMap(module => module.sources)
      .concat(dataset.flows.flatMap(flow => flow.sources))
      .concat(dataset.findings.flatMap(finding => finding.sources))
      .concat(Object.values(dataset.abstractionTypes).map(type => type.source));
    for (const source of sources) {
      const [file, line] = source.split(':');
      const lines = require('node:child_process').execFileSync('git', ['show', `0715627:${file}`], { cwd: path.resolve(__dirname, '..'), encoding: 'utf8' }).split('\n');
      assert.ok(Number(line) > 0 && Number(line) <= lines.length, `Invalid reference ${source}`);
    }
    assert.equal(await page.locator('.architecture-graph').count(), 3);
    assert.ok(await page.locator('#map-view .graph-edge path[marker-end]').count() >= 8);
    await page.locator('#map-view [data-module="tool-provider"]').click();
    assert.equal(await page.locator('#map-view .graph-edge.highlighted').count(), 7);
    assert.equal(await page.locator('#map-view .graph-edge.impl').count(), 5);
    assert.match(await page.locator('#detail').textContent(), /approval/);
    assert.ok(await page.locator('#map-view .graph-edge.faded').count() > 0);
    await page.locator('#graph-reset').click();
    assert.equal(await page.locator('#map-view .faded').count(), 0);
    await page.locator('#graph-fullscreen').click();
    await page.waitForFunction(() => Boolean(document.fullscreenElement));
    await page.locator('#graph-fullscreen').click();
    await page.waitForFunction(() => !document.fullscreenElement);
    await page.locator('#map-view [data-module="runner"]').focus();
    await page.keyboard.press('Enter');
    assert.equal(await page.locator('#map-view [data-module="runner"]').getAttribute('aria-pressed'), 'true');
    assert.match(await page.locator('#detail').textContent(), /TurnRunner/);
    await page.locator('[data-view="dependencies"]').click();
    assert.equal(await page.locator('#dependencies-view').isVisible(), true);
    assert.equal(await page.locator('#map-view').isVisible(), false);
    await page.locator('#dependencies-view [data-module="security"]').click();
    assert.match(await page.locator('#detail').textContent(), /RunnerPolicy/);
    await page.locator('[data-view="ownership"]').click();
    assert.equal(await page.locator('#ownership-view').isVisible(), true);
    await page.locator('[data-view="map"]').click();
    for (let index = 0; index < dataset.flows.length; index++) {
      await page.locator(`[data-flow="${index}"]`).click();
      assert.equal(await page.locator('#flow-title').textContent(), dataset.flows[index].title);
      assert.equal(await page.locator('#flow-steps .step').count(), dataset.flows[index].steps.length);
    }
    await page.locator('#search').fill('Conversation');
    const matchCount = await page.locator('.module-card').count();
    assert.ok(matchCount > 0 && matchCount < 31);
    await page.locator('#search').fill('unlikely-no-match-3493');
    assert.equal(await page.locator('#module-empty').isVisible(), true);
    assert.equal(await page.locator('#finding-empty').isVisible(), true);
    await page.locator('#search').press('Escape');
    assert.equal(await page.locator('.module-card').count(), 31);
    await page.locator('#risk-filter').selectOption('P0');
    assert.equal(await page.locator('.finding').count(), 1);
    await page.locator('[data-status="rewind"]').selectOption('需要验证');
    await page.locator('[data-note="rewind"]').fill('保留 recovery；先做故障注入 <test>');
    await page.reload();
    assert.equal(await page.locator('[data-status="rewind"]').inputValue(), '需要验证');
    assert.equal(await page.locator('[data-note="rewind"]').inputValue(), '保留 recovery；先做故障注入 <test>');
    await page.locator('#expand').click();
    assert.equal(await page.locator('.finding[open]').count(), 16);
    await page.locator('#collapse').click();
    assert.equal(await page.locator('.finding[open]').count(), 0);
    const downloadPromise = page.waitForEvent('download');
    await page.locator('#export').click();
    const download = await downloadPromise;
    assert.equal(download.suggestedFilename(), 'programmer-architecture-notes.json');
    const stream = await download.createReadStream();
    let text = '';
    for await (const chunk of stream) text += chunk.toString();
    const exported = JSON.parse(text);
    assert.equal(exported.notes.find(note => note.id === 'rewind').status, '需要验证');
    await page.locator('#theme').click();
    assert.equal(await page.locator('body').evaluate(body => body.classList.contains('theme-dark')), true);
    await page.locator('#theme').click();
    await page.locator('[data-inspect="runner"]').click();
    assert.equal(new URL(page.url()).hash, '#system');
    assert.match(await page.locator('#detail').textContent(), /TurnRunner/);
    await page.waitForFunction(() => document.querySelector('nav a.active')?.hash === '#system');
    const selectionColors = await page.locator('#map-view [data-module="runner"]').evaluate(element => {
      const style = getComputedStyle(element.querySelector('.graph-title'));
      return { color: style.fill, background: style.backgroundColor };
    });
    assert.notEqual(selectionColors.color, 'rgb(255, 255, 255)', 'Selected light-theme node must remain readable');
    await page.locator('#graph-reset').click();
    await page.locator('#graph-fit').click();
    assert.equal(await page.locator('#map-view .graph-scroll').evaluate(element => element.scrollWidth <= element.clientWidth), true);
    await page.locator('#graph-fit').click();
    await page.screenshot({ path: path.resolve(__dirname, 'architecture-review-desktop.png') });
    await page.locator('#expand').click();
    await page.evaluate(() => { location.hash = 'findings'; });
    await page.screenshot({ path: path.resolve(__dirname, 'architecture-review-findings.png') });
    await page.setViewportSize({ width: 390, height: 844 });
    await page.evaluate(() => { location.hash = 'system'; });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, 'No mobile horizontal overflow');
    await page.screenshot({ path: path.resolve(__dirname, 'architecture-review-mobile.png') });
    await page.setViewportSize({ width: 2100, height: 1500 });
    for (const view of ['map', 'dependencies', 'ownership']) {
      await page.locator(`[data-view="${view}"]`).click();
      await page.locator(`#${view}-view`).screenshot({ path: path.resolve(__dirname, `architecture-review-${view}.png`) });
    }
    await page.emulateMedia({ media: 'print' });
    assert.equal(await page.locator('.sidebar').isVisible(), false);
    assert.deepEqual(failures, []);
    console.log('PASS: coverage, source references, maps, 8 flows, search, filters, notes, export, theme, narrow layout, print; no browser errors or network requests.');
  } finally {
    await browser.close();
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
