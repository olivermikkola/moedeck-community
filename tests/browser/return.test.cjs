const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const net = require('node:net');
const { spawn } = require('node:child_process');
const { chromium, webkit } = require('playwright');

const binary = process.env.MOEDECK_BIN || path.resolve(__dirname, '../../target/debug/moedeck');
const artifacts = process.env.MOEDECK_BROWSER_ARTIFACTS || path.join(__dirname, 'artifacts');

// Native protocol fixtures: no model, personal home or live daemon is used.
const cli = `#!/usr/bin/env node
const fs=require('node:fs'), emit=m=>console.log(JSON.stringify(m));
if(process.argv.includes('--version')) {console.log('return fixture');process.exit(0)}
if(process.argv[1].endsWith('/keel')) {
  emit({type:'capabilities',approval_protocol:'stdio-line-v1'});
  emit({type:'approval_request',protocol:'stdio-line-v1',id:'return-approval',tool:'shell',arguments:{command:'npm test'}});
  require('node:readline').createInterface({input:process.stdin}).once('line',decision=>{
    fs.appendFileSync(process.env.MOEDECK_RETURN_DECISIONS,decision+'\\n');
    emit({type:'done',status:'ok',answer:'Decision received: '+decision});process.exit(0);
  });
} else {
  const rl=require('node:readline').createInterface({input:process.stdin});
  rl.on('line',line=>{
    const m=JSON.parse(line),p=m.params||{};
    if(m.method==='initialize') emit({id:m.id,result:{userAgent:'return-fixture'}});
    if(m.method==='thread/start'||m.method==='thread/resume') emit({id:m.id,result:{thread:{id:p.threadId||require('node:crypto').randomUUID()}}});
    if(m.method!=='turn/start') return;
    const prompt=p.input.find(i=>i.type==='text').text;
    emit({id:m.id,result:{turn:{id:'return-turn',status:'inProgress'}}});
    const text=prompt==='hold'?'Checking the phone layout…':'Settings now remember the selected project.\\n\\n'+Array(60).fill('Verified the saved selection after reopening the cockpit.').join('\\n\\n');
    emit({method:'item/agentMessage/delta',params:{itemId:'return-message',delta:text}});
    if(prompt==='hold') return;
    emit({method:'item/completed',params:{item:{type:'agentMessage',id:'return-message',text}}});
    emit({method:'turn/completed',params:{turn:{id:'return-turn',status:'completed'}}});
  });
  rl.on('close',()=>process.exit(0));
}
`;

for (const engine of (process.env.MOEDECK_BROWSERS || 'chromium,webkit').split(',')) {
  test(`${engine}: return to approvals and unseen changes`, { timeout: 90000 }, async t => {
    const fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'moedeck-return-'));
    const home = path.join(fixture, 'home'), config = path.join(fixture, 'config');
    const bin = path.join(home, '.local/bin'), projects = path.join(home, 'Projects');
    const viewer = path.join(projects, 'turku-viewer'), deck = path.join(projects, 'moe-deck');
    [bin, viewer, deck, config].forEach(p => fs.mkdirSync(p, { recursive: true }));
    for (const name of ['codex', 'keel']) fs.writeFileSync(path.join(bin, name), cli, { mode: 0o755 });
    const socket = net.createServer();
    await new Promise(resolve => socket.listen(0, '127.0.0.1', resolve));
    const port = socket.address().port;
    await new Promise(resolve => socket.close(resolve));
    const origin = `http://127.0.0.1:${port}`, decisions = path.join(fixture, 'decisions');
    const child = spawn(binary, [], { env: { ...process.env, HOME: home, CODEX_HOME: path.join(home, '.codex'),
      MOEDECK_CONFIG_DIR: config, MOEDECK_PORT: String(port), MOEDECK_PASSCODE: 'return-proof',
      MOEDECK_RETURN_DECISIONS: decisions }, stdio: 'ignore' });
    let browser;
    t.after(async () => {
      await browser?.close();
      child.kill('SIGTERM');
      if (child.exitCode === null && child.signalCode === null) await new Promise(resolve => child.once('exit', resolve));
      console.log(`${engine} return evidence: ${fixture}`);
    });
    for (let i = 0; ; i++) {
      try { if ((await fetch(origin + '/api/health')).ok) break; } catch {}
      if (i > 100) throw new Error('Gateway did not start');
      await new Promise(resolve => setTimeout(resolve, 100));
    }
    browser = await ({ chromium, webkit }[engine]).launch({ headless: true });
    const context = await browser.newContext({ viewport: { width: 393, height: 852 }, isMobile: true, hasTouch: true, serviceWorkers: 'block' });
    await context.request.post(origin + '/api/auth/login', { data: { passcode: 'return-proof' } });
    const create = async (title, harness, workdir) => {
      const response = await context.request.post(origin + '/api/conversations', { data: { title, harness, workdir } });
      assert(response.ok(), await response.text());
      return (await response.json()).id;
    };
    const first = await create('Turku viewer', 'codex', viewer);
    const updated = await create('Remember the selected project', 'codex', deck);
    const working = await create('Check the phone layout', 'codex', deck);
    const approval = await create('Run the project tests', 'keel', deck);
    const page = await context.newPage(), errors = [];
    page.on('pageerror', e => errors.push(e.message));
    page.setDefaultTimeout(12000);
    // Only select the initial thread once; reloads use the browser's saved state.
    await page.goto(origin);
    await page.evaluate(id => localStorage.setItem('helm_active_id', id), first);
    await page.reload();
    await page.waitForFunction(id => state.detail?.summary.id === id, first);
    const view = name => page.locator(`.deck-view[data-view="${name}"]`);
    const count = async name => Number(await view(name).locator('strong').textContent());
    const menu = () => page.getByRole('button', { name: 'Menu', exact: true }).tap();
    await menu();
    assert.deepEqual(await Promise.all(['needs', 'updated', 'working'].map(count)), [0, 0, 0]);
    const send = async (id, text) => {
      const response = await context.request.post(`${origin}/api/conversations/${encodeURIComponent(id)}/messages`, { data: { text } });
      assert(response.ok(), await response.text());
    };
    await send(working, 'hold');
    await send(updated, 'update');
    await send(approval, 'approval');
    await page.waitForFunction(() => ['needs', 'updated', 'working'].every(v => document.querySelector(`.deck-view[data-view="${v}"] strong`).textContent === '1'));
    assert.equal(await page.locator('#deckBadge').textContent(), '2');
    assert.equal(fs.existsSync(decisions), false);
    fs.mkdirSync(artifacts, { recursive: true });
    await page.screenshot({ path: path.join(artifacts, `${engine}-return-overview.png`) });
    await page.setViewportSize({ width: 320, height: 640 });
    assert.equal(await page.locator('#drawer').evaluate(el => el.scrollWidth <= el.clientWidth), true);
    assert.equal(await page.locator('.drawer-brand').evaluate(el => el.getBoundingClientRect().height < 40), true);
    await page.screenshot({ path: path.join(artifacts, `${engine}-return-narrow.png`) });
    await page.setViewportSize({ width: 852, height: 393 });
    // Short screens scroll the entire drawer, so both conversations and settings remain reachable.
    await page.locator(`.conv-item[data-conv-id="${first}"]`).scrollIntoViewIfNeeded();
    assert.equal(await page.locator(`.conv-item[data-conv-id="${first}"]`).isVisible(), true);
    await page.locator('#settingsBtn').scrollIntoViewIfNeeded();
    assert.equal(await page.locator('#settingsBtn').isVisible(), true);
    await page.screenshot({ path: path.join(artifacts, `${engine}-return-landscape.png`) });
    await page.setViewportSize({ width: 393, height: 852 });

    // Opening a detail behind the drawer is not reading it.
    await page.evaluate(id => loadConversation(id, false), updated);
    assert.equal(await count('updated'), 1);
    // Save another active chat, then reload: an unseen update survives the page closing.
    await page.evaluate(id => loadConversation(id, false), first);
    await page.reload();
    await page.waitForFunction(id => state.detail?.summary.id === id, first);
    await menu();
    assert.equal(await count('updated'), 1);
    await view('updated').tap();
    assert.deepEqual(await page.locator('#convList .conv-item').evaluateAll(els => els.map(el => el.dataset.convId)), [updated]);
    await page.locator('#drawerSearchInput').fill('does-not-match');
    await page.locator('#convList').getByText('No matching conversations', { exact: true }).waitFor();
    assert.equal(await count('updated'), 0);
    await page.getByRole('button', { name: 'Show all threads', exact: true }).tap();
    assert.equal(await page.locator('#drawerSearchInput').inputValue(), '');
    assert.equal(await count('updated'), 1);
    await view('working').tap();
    assert.deepEqual(await page.locator('#convList .conv-item').evaluateAll(els => els.map(el => el.dataset.convId)), [working]);
    await view('needs').tap();
    assert.deepEqual(await page.locator('#convList .conv-item').evaluateAll(els => els.map(el => el.dataset.convId)), [approval]);
    assert.match(await page.locator('#convList .conv-item-preview').textContent(), /shell: npm test/);
    await page.locator('#drawerFilters [data-filter="codex"]').tap();
    assert.equal(await count('needs'), 0);
    await page.getByRole('button', { name: 'Show all threads', exact: true }).tap();

    // Failed detail fetches leave the update unread.
    await page.evaluate(() => { pollPaused = true; clearTimeout(pollTimer); });
    await page.route('**/api/conversations/**', route => route.fulfill({ status: 500, json: { error: 'Fixture unavailable' } }));
    await page.locator(`.conv-item[data-conv-id="${updated}"]`).tap();
    await page.locator('#chatScroll').getByText(/Conversation could not load/).waitFor();
    await menu();
    assert.equal(await count('updated'), 1);
    await page.unroute('**/api/conversations/**');
    await page.evaluate(id => loadConversation(id, true), updated);
    assert.equal(await count('updated'), 1);
    // A hidden phone page must not acknowledge a successfully loaded result.
    await page.evaluate(() => {
      Object.defineProperty(document, 'hidden', { configurable: true, get: () => true });
      closeOverlays();
    });
    await page.evaluate(id => loadConversation(id, true), updated);
    assert.equal(await count('updated'), 1);
    await menu();
    await page.evaluate(() => { delete document.hidden; });
    // Closing the drawer while scrolled back still doesn't expose the latest result.
    await page.evaluate(() => { document.getElementById('chatScroll').scrollTop = 0; closeOverlays(); });
    await page.waitForFunction(() => !document.querySelector('#backdrop').classList.contains('open'));
    assert.equal(await count('updated'), 1);
    await page.evaluate(() => { const el = document.getElementById('chatScroll'); el.scrollTop = el.scrollHeight; });
    await page.waitForFunction(() => document.querySelector('.deck-view[data-view="updated"] strong').textContent === '0');
    await page.reload();
    await page.waitForFunction(id => state.detail?.summary.id === id, updated);
    await menu();
    assert.equal(await count('updated'), 0);
    await view('updated').tap();
    await page.locator('#convList').getByText('You’re caught up', { exact: true }).waitFor();

    // Only an explicit native decision releases the real waiting fixture subprocess.
    await view('needs').tap();
    await page.locator(`.conv-item[data-conv-id="${approval}"]`).tap();
    await page.locator('#approvalDock').getByRole('button', { name: '✕ Deny', exact: true }).tap();
    await page.locator('#chatScroll').getByText('Decision received: deny', { exact: true }).waitFor();
    await page.waitForFunction(() => document.querySelector('.deck-view[data-view="needs"] strong').textContent === '0');
    assert.equal(fs.readFileSync(decisions, 'utf8'), 'deny\n');

    // Archived changes are still accessible, but don't add to the return badge.
    await menu();
    await send(updated, 'another update');
    await page.waitForFunction(() => document.querySelector('.deck-view[data-view="updated"] strong').textContent === '1');
    const archive = await context.request.patch(`${origin}/api/conversations/${encodeURIComponent(updated)}`, { data: { archived: true } });
    assert(archive.ok(), await archive.text());
    await page.waitForFunction(() => document.querySelector('.deck-view[data-view="updated"] strong').textContent === '0');
    assert.equal(await page.locator('#deckBadge').isVisible(), false);
    await page.locator('.drawer-archived-toggle').tap();
    await page.locator(`.conv-item[data-conv-id="${updated}"]`).waitFor();

    // Bad saved state recovers to a quiet baseline without breaking the cockpit.
    await page.evaluate(() => localStorage.setItem('moedeck_seen_conversations_v1', '{broken-json'));
    await page.reload();
    await page.waitForFunction(() => state.detail && state.seenConversations instanceof Map);
    assert.equal(await count('updated'), 0);
    assert.deepEqual(errors, []);
  });
}
