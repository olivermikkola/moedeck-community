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
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
fs.mkdirSync(artifacts, { recursive: true });
async function unusedPort() {
  const socket = net.createServer();
  await new Promise(resolve => socket.listen(0, '127.0.0.1', resolve));
  const port = socket.address().port;
  await new Promise(resolve => socket.close(resolve));
  return port;
}

// These scripted CLIs test transport and UI behavior, not coding ability.
const harnessFixture = `#!/usr/bin/env node
const fs=require('node:fs'), path=require('node:path'), crypto=require('node:crypto');
const args=process.argv.slice(2), emit=value=>console.log(JSON.stringify(value));
if(args.includes('--version')) { console.log('fixture 1.0'); process.exit(0); }
const kind=path.basename(process.argv[1]), keel=kind==='keel', prompt=args.at(-1);
const receipt=extra=>fs.appendFileSync(process.env.MOEDECK_FIXTURE_RECEIPTS,JSON.stringify({keel,kind,args,...extra,cwd:process.cwd(),pid:process.pid,tty:!!process.stdin.isTTY&&!!process.stdout.isTTY,profileConfig:process.env.CLAUDE_CONFIG_DIR||null,inheritedPrimaryToken:!!process.env.CLAUDE_CODE_OAUTH_TOKEN})+'\\n');
if(kind!=='codex') receipt({});
if(kind==='claude'&&args.includes('-p')) {
  const say=content=>emit({type:'assistant',message:{content}});
  say([{type:'text',text:'Checking the config.'}]);
  say([{type:'tool_use',id:'t1',name:'Read',input:{file_path:'helm.toml'}}]);
  emit({type:'user',message:{content:[{type:'tool_result',tool_use_id:'t1',content:'port = 7770'}]}});
  say([{type:'text',text:'The port is 7770.'}]);
  emit({type:'result',subtype:'success',result:'The port is 7770.'});
} else if(kind==='claude') {
  if(args[0]!=='remote-control'||!process.stdin.isTTY||!process.stdout.isTTY) {console.error('Expected native Remote Control in a real terminal');process.exit(2)}
  if(path.basename(process.cwd())==='rc-fail') {console.error('Remote Control requires a full-scope login token');process.exit(1)}
  console.log('Native terminal: trust this project? Type yes.');
  require('node:readline').createInterface({input:process.stdin}).once('line',answer=>{
    if(answer.trim()!=='yes') {console.error('Project trust declined');process.exit(1)}
    const worker=require('node:child_process').spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});
    fs.writeFileSync(process.env.MOEDECK_FIXTURE_RECEIPTS+'.rc-worker',String(worker.pid));
    const project=path.join(process.env.HOME,'.claude/projects',process.cwd().split('/').join('-'));
    fs.mkdirSync(project,{recursive:true});
    const session=crypto.randomUUID(), timestamp=new Date().toISOString();
    fs.writeFileSync(path.join(project,session+'.jsonl'),[
      {type:'user',sessionId:session,cwd:process.cwd(),timestamp,message:{role:'user',content:'Message from Claude app fixture'}},
      {type:'assistant',sessionId:session,cwd:process.cwd(),timestamp,message:{role:'assistant',model:'claude-fixture',content:[{type:'text',text:'Native app reply fixture'}]}}
    ].map(JSON.stringify).join(String.fromCharCode(10))+String.fromCharCode(10));
    console.log(String.fromCharCode(27)+'[32mConnected'+String.fromCharCode(27)+'[0m'); console.log('https://claude.ai/code/session_fixture?env=local');
  });
} else if(keel) {
  emit({type:'capabilities',approval_protocol:'stdio-line-v1'});
  emit({type:'approval_request',protocol:'stdio-line-v1',id:crypto.randomUUID(),tool:'shell',arguments:{command:'printf approval-check'}});
  require('node:readline').createInterface({input:process.stdin}).once('line',decision=>{
    fs.appendFileSync(process.env.MOEDECK_FIXTURE_DECISIONS,decision+'\\n');
    emit({type:'done',status:'ok',answer:'Permission response: '+decision}); process.exit(0);
  });
} else {
  // Codex app-server: JSON-RPC on stdin, one receipt per turn with what MoE Deck asked for.
  let model=null, resume=null;
  const rl=require('node:readline').createInterface({input:process.stdin});
  rl.on('line',raw=>{
    const m=JSON.parse(raw), p=m.params||{};
    if(m.method==='initialize') emit({id:m.id,result:{userAgent:'fixture'}});
    if(m.method==='thread/start'||m.method==='thread/resume') {
      resume=p.threadId||null; model=p.model||null;
      emit({id:m.id,result:{thread:{id:resume||crypto.randomUUID()}}});
    }
    if(m.method!=='turn/start') return;
    const text=p.input.find(i=>i.type==='text').text, say='Harness received: '+text;
    receipt({model,resume,prompt:text,images:p.input.filter(i=>i.type==='localImage').map(i=>i.path)});
    emit({id:m.id,result:{turn:{id:'turn-1',status:'inProgress'}}});
    emit({method:'item/agentMessage/delta',params:{itemId:'m1',delta:say.slice(0,9)}});
    emit({method:'item/agentMessage/delta',params:{itemId:'m1',delta:say.slice(9)}});
    emit({method:'item/completed',params:{item:{type:'agentMessage',id:'m1',text:say}}});
    const done=()=>emit({method:'turn/completed',params:{turn:{id:'turn-1',status:'completed'}}});
    if(text.includes('wait-for-stop')) setTimeout(done,30000); else done();
  });
  rl.on('close',()=>process.exit(0));
}
`;

for (const engineName of (process.env.MOEDECK_BROWSERS || 'chromium,webkit').split(',')) {
  test(`${engineName}: phone controls against a real gateway`, { timeout: 120000 }, async t => {
    const fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'moedeck-phone-'));
    const testHome = path.join(fixture, 'home'), config = path.join(fixture, 'config');
    const binDir = path.join(testHome, '.local/bin');
    const alpha = path.join(testHome, 'Projects/alpha'), beta = path.join(testHome, 'Projects/beta');
    [binDir, alpha, beta, config].forEach(dir => fs.mkdirSync(dir, { recursive: true }));
    for (const name of ['codex', 'keel', 'claude']) fs.writeFileSync(path.join(binDir, name), harnessFixture, { mode: 0o755 });
    fs.writeFileSync(path.join(config, 'settings.json'), JSON.stringify({ harnesses: {
      codex: { enabled: true, models: [{ id:'fixture-model', label:"Ada's model", default_effort:'low', efforts:['low','high'] }] }
    } }));
    const receipts = path.join(fixture, 'receipts.jsonl'), decisions = path.join(fixture, 'decisions.txt');
    const port = await unusedPort(), origin = `http://127.0.0.1:${port}`;
    const child = spawn(binary, [], { env: { ...process.env, HOME:testHome, MOEDECK_CONFIG_DIR:config,
      MOEDECK_PORT:String(port), MOEDECK_PASSCODE:'phone-proof', CLAUDE_CODE_OAUTH_TOKEN:'fixture-primary-only', CODEX_HOME:path.join(testHome,'.codex'),
      MOEDECK_FIXTURE_RECEIPTS:receipts, MOEDECK_FIXTURE_DECISIONS:decisions }, stdio:'ignore' });
    let browser, page;
    t.after(async () => {
      if (page && !page.isClosed()) await page.screenshot({path:path.join(artifacts,`${engineName}-phone.png`)}).catch(()=>{});
      await browser?.close();
      child.kill('SIGTERM');
      if (child.exitCode === null && child.signalCode === null) await new Promise(resolve => child.once('exit', resolve));
      console.log(`${engineName} subprocess evidence: ${fixture}`);
    });
    for (let i = 0; ; i++) {
      try { if ((await fetch(origin+'/api/health')).ok) break; } catch {}
      if (i > 100) throw new Error('Gateway did not start');
      await sleep(100);
    }
    const launch = { headless:true };
    if (engineName === 'webkit' && process.env.MOEDECK_WEBKIT_LIBS) {
      const base = path.join(path.dirname(webkit.executablePath()), 'minibrowser-wpe');
      launch.executablePath = path.join(base,'bin/MiniBrowser');
      launch.env = { ...process.env, LD_LIBRARY_PATH:`${process.env.MOEDECK_WEBKIT_LIBS}:${base}/lib:${base}/sys/lib`,
        WEBKIT_EXEC_PATH:`${base}/bin`, WEBKIT_INJECTED_BUNDLE_PATH:`${base}/lib`, WEBKIT_INSPECTOR_RESOURCES_PATH:`${base}/share` };
    }
    browser = await ({chromium,webkit}[engineName]).launch(launch);
    const context = await browser.newContext({viewport:{width:393,height:852},isMobile:true,hasTouch:true,serviceWorkers:'block'});
    page = await context.newPage();
    page.setDefaultTimeout(10000);
    const errors = [];
    page.on('pageerror', err => errors.push(err.message));
    await page.goto(origin);
    await page.locator('#loginPass').fill('phone-proof');
    await page.locator('#loginForm button').tap();
    await page.waitForFunction(()=>state.bootstrappedOnce);
    const closePanel = async () => {
      await page.getByRole('button',{name:'Close panel',exact:true}).tap();
      await page.waitForFunction(()=>!document.querySelector('#bottomSheet').classList.contains('open'));
    };
    const newThread = async (harness,project) => {
      await page.getByRole('button',{name:'New thread',exact:true}).tap();
      await page.locator('#newHarnessSelect').selectOption(harness);
      await page.locator('#projectSelect').selectOption(project);
      await page.getByRole('button',{name:'Start conversation',exact:true}).tap();
      await page.waitForFunction(({harness,project})=>state.detail?.summary.harness===harness&&state.detail.summary.workdir===project,{harness,project});
      return page.evaluate(()=>state.activeId);
    };
    const send = async text => {
      await page.locator('#composerInput').fill(text);
      await page.getByRole('button',{name:'Send',exact:true}).tap();
    };
    const idle = () => page.waitForFunction(()=>state.detail&&state.detail.summary.status!=='busy'&&!sendingTurn);
    const first = await newThread('codex',alpha);
    assert.equal(await page.locator('#convSubtitle').textContent(),'Codex · alpha');
    await page.locator('#modelPillBtn').tap();
    await page.getByRole('button',{name:"Ada's model",exact:false}).tap();
    assert.match(await page.locator('#pillModelName').textContent(),/Ada's model/);
    await send('hello-phone');
    await page.locator('#chatScroll').getByText('Harness received: hello-phone',{exact:true}).waitFor();
    await idle();
    const readReceipts = () => fs.readFileSync(receipts,'utf8').trim().split('\n').map(JSON.parse);
    assert.equal(readReceipts()[0].cwd,alpha);
    assert.equal(readReceipts()[0].model,'fixture-model');
    const native = JSON.parse(fs.readFileSync(path.join(config,'threads',first.replace(':','_')+'.json'),'utf8')).native_session;
    assert(native);
    await page.reload();
    await page.waitForFunction(id=>state.detail?.summary.id===id,first);
    await send('resume-phone');
    await page.locator('#chatScroll').getByText('Harness received: resume-phone',{exact:true}).waitFor();
    await idle();
    assert.equal(readReceipts()[1].resume,native);
    const second = await newThread('codex',beta);
    await page.evaluate(()=>{state.previousId='missing-thread'});
    await page.locator('#quickFlipBtn').tap();
    await page.waitForFunction(id=>state.detail?.summary.id===id,first);
    await page.evaluate(id=>loadConversation(id,true),second);
    await page.locator('#composerInput').fill('draft-beta');
    await page.evaluate(id=>loadConversation(id,true),first);
    await page.locator('#composerInput').fill('draft-alpha');
    await page.evaluate(id=>loadConversation(id,true),second);
    assert.equal(await page.locator('#composerInput').inputValue(),'draft-beta');
    await page.locator('#composerInput').fill('');
    let releasePost, postAccepted;
    const heldPost=new Promise(resolve=>{releasePost=resolve});
    const accepted=new Promise(resolve=>{postAccepted=resolve});
    await page.route('**/messages',async route=>{
      const response=await route.fetch(); postAccepted(); await heldPost;
      await route.fulfill({response});
    });
    await send('slow-post-phone');
    await accepted;
    await page.evaluate(id=>loadConversation(id,true),first);
    releasePost();
    await page.waitForFunction(()=>!sendingTurn);
    assert.equal(await page.evaluate(()=>state.activeId),first);
    assert.equal(await page.locator('#composerInput').inputValue(),'draft-alpha');
    await page.unroute('**/messages');
    await page.locator('#composerInput').fill('');
    await page.evaluate(id=>loadConversation(id,true),second);

    await page.evaluate(()=>{pollPaused=true;clearTimeout(pollTimer)});
    await page.route('**/api/conversations/**',async route=>{
      if(decodeURIComponent(new URL(route.request().url()).pathname).endsWith('/'+first)) await sleep(300);
      await route.continue();
    });
    await page.evaluate(async ({first,second})=>{await Promise.all([loadConversation(first,true),loadConversation(second,true)])},{first,second});
    assert.equal(await page.evaluate(()=>state.activeId===state.detail.summary.id),true);
    assert.equal(await page.evaluate(()=>state.selectedWorkdir),beta);
    await page.unroute('**/api/conversations/**');
    await page.evaluate(()=>{pollPaused=false;scheduleNextPoll()});
    await page.getByRole('button',{name:'Menu',exact:true}).tap();
    await page.waitForFunction(()=>Math.abs(document.querySelector('#drawer').getBoundingClientRect().left)<1);
    const row=page.locator('.conv-item').first(), handle=await row.elementHandle();
    if(engineName==='chromium') {
      const box=await row.boundingBox(), cdp=await context.newCDPSession(page);
      await cdp.send('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[{x:box.x+box.width/2,y:box.y+box.height/2}]});
      await page.evaluate(()=>{state.conversations[0].last_preview='new streamed preview';renderDrawerList()});
      assert.equal(await handle.evaluate(el=>el.isConnected),true);
      await cdp.send('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]});
    } else {
      await row.dispatchEvent('pointerdown',{pointerId:77});
      await page.evaluate(()=>{state.conversations[0].last_preview='new streamed preview';renderDrawerList()});
      assert.equal(await handle.evaluate(el=>el.isConnected),true);
      await row.dispatchEvent('pointerup',{pointerId:77});
      await row.tap();
    }
    await page.waitForFunction(()=>!document.querySelector('#drawer').classList.contains('open')&&state.detail?.summary.id===state.activeId);
    await page.locator('#headerTitleWrap').tap();
    await page.locator('#projectPathInput').fill('/does/not/exist');
    await page.getByRole('button',{name:'Use project',exact:true}).tap();
    await page.getByRole('alert').filter({hasText:'existing project folder'}).waitFor();
    assert.notEqual(await page.evaluate(()=>state.selectedWorkdir),'/does/not/exist');
    await closePanel();
    await page.route('**/messages',route=>route.fulfill({status:500,json:{error:'Fixture send rejected'}}));
    await send('do-not-lose-this');
    await page.waitForFunction(()=>document.querySelector('#composerInput').value==='do-not-lose-this'&&!sendingTurn);
    assert.match(await page.locator('#helmToast').textContent(),/Fixture send rejected/);
    await page.unroute('**/messages');
    await send('wait-for-stop');
    await page.waitForFunction(()=>state.detail?.summary.status==='busy');
    await page.locator('#runningStopBtn').tap();
    await idle();
    // The active runner can become idle before its cancellation transcript is
    // persisted. Wait for the actual visible terminal outcome, not that flag.
    await page.locator('#chatScroll').getByText(/Stopped by user/).waitFor();
    await context.setOffline(true);
    await page.waitForFunction(()=>!navigator.onLine);
    await send('offline-phone');
    await page.waitForFunction(()=>state.offlineQueue.length===1);
    await context.setOffline(false);
    await page.locator('#chatScroll').getByText('Harness received: offline-phone',{exact:true}).waitFor();
    await page.waitForFunction(()=>state.offlineQueue.length===0);
    assert.equal(readReceipts().filter(r=>r.prompt==='offline-phone').length,1);
    await page.route('**/messages',async route=>{await route.fetch();await route.abort('failed')});
    await send('lost-response-phone');
    await page.waitForFunction(()=>state.offlineQueue.length===1);
    await page.unroute('**/messages');
    await context.setOffline(true);
    await context.setOffline(false);
    await page.waitForFunction(()=>state.offlineQueue.length===0);
    await page.locator('#chatScroll').getByText('Harness received: lost-response-phone',{exact:true}).waitFor();
    assert.equal(readReceipts().filter(r=>r.prompt==='lost-response-phone').length,1);
    await page.locator('#fileInput').setInputFiles({name:'pixel.png',mimeType:'image/png',buffer:Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/Zl0AAAAASUVORK5CYII=','base64')});
    await page.waitForFunction(()=>state.queuedImages.length===1);
    await send('image-phone');
    await page.waitForFunction(()=>state.detail?.messages.some(m=>m.role==='assistant'&&m.text.includes('Harness received: image-phone')));
    await idle();
    assert.equal(readReceipts().at(-1).images.length,1);
    // An attachment sits under the user's line, not floating above it.
    assert.deepEqual(await page.locator('#chatScroll .msg-user-turn:has(.msg-images)').last().evaluate(t=>[...t.children].map(c=>c.className)),['t-user','msg-images']);

    // Archive hides a chat from the drawer without deleting it; unarchive brings it back.
    await page.locator('#openDrawerBtn').tap();
    await page.locator(`.conv-item[data-conv-id="${first}"]`).dispatchEvent('contextmenu');
    await page.locator('#archiveConvBtn').tap();
    await page.waitForFunction(id=>state.archived.has(id),first);
    assert.deepEqual(JSON.parse(fs.readFileSync(path.join(config,'archived.json'),'utf8')),[first]);
    await page.locator('#openDrawerBtn').tap();
    assert.equal(await page.locator(`.conv-item[data-conv-id="${first}"]`).count(),0);
    await page.locator('.drawer-archived-toggle').tap();
    assert.equal(await page.locator('#convListHeading').textContent(),'Archived');
    await page.locator(`.conv-item[data-conv-id="${first}"]`).dispatchEvent('contextmenu');
    await page.locator('#archiveConvBtn').tap();
    await page.waitForFunction(id=>!state.archived.has(id),first);
    assert.deepEqual(JSON.parse(fs.readFileSync(path.join(config,'archived.json'),'utf8')),[]);

    const keel=await newThread('keel',alpha);
    await send('approval-check');
    const allow=()=>page.locator('#approvalDock').getByRole('button',{name:'✓ Allow',exact:true});
    await allow().waitFor();
    assert.equal(fs.existsSync(decisions),false);
    await page.route('**/decision',route=>route.fulfill({status:500,json:{error:'Fixture decision rejected'}}));
    await allow().tap();
    await page.waitForFunction(()=>document.querySelector('#helmToast')?.textContent==='Fixture decision rejected');
    assert.equal(fs.existsSync(decisions),false);
    await page.unroute('**/decision');
    await allow().tap();
    await page.locator('#chatScroll').getByText('Permission response: allow',{exact:true}).waitFor();
    await idle();
    await send('approval-check-again');
    await page.locator('#approvalDock').getByRole('button',{name:'✕ Deny',exact:true}).tap();
    await page.locator('#chatScroll').getByText('Permission response: deny',{exact:true}).waitFor();
    await idle();
    assert.equal(fs.readFileSync(decisions,'utf8'),'allow\ndeny\n');
    assert.equal(await page.evaluate(()=>state.activeId),keel);
    // A turn reads in the order it happened, and a tool line opens to its output.
    await newThread('claude',alpha);
    await send('timeline-phone');
    await page.locator('#chatScroll').getByText('The port is 7770.',{exact:true}).waitFor();
    await idle();
    const turn=page.locator('#chatScroll .t-turn').last();
    assert.deepEqual(await turn.locator('.t-line').evaluateAll(ls=>ls.map(l=>l.firstElementChild.nextElementSibling.firstChild.textContent.trim())),
      ['Checking the config.','Read','The port is 7770.']);
    const tool=turn.locator('.t-tool');
    assert.equal(await tool.locator('.t-out').isVisible(),false);
    await tool.tap();
    assert.equal(await tool.locator('.t-out').textContent(),'port = 7770');
    assert.equal(await tool.locator('.t-out').isVisible(),true);
    await page.screenshot({path:path.join(artifacts,`${engineName}-turn-timeline.png`)});
    // A turn that has produced nothing yet shows only "Working…", no copy/speak row.
    assert.deepEqual(await page.evaluate(()=>{const d=document.createElement('div');
      d.innerHTML=renderTurn({role:'assistant',harness:'claude',text:'',streaming:true},0);
      const later=document.createElement('div');later.innerHTML=renderTurn({role:'assistant',harness:'claude',text:'done',streaming:true},0);
      return [d.querySelectorAll('.t-working').length,d.querySelectorAll('.t-meta').length,later.querySelectorAll('.t-meta').length];}),[1,0,1]);
    // When the phone keyboard scrolls the page, the app follows the visible area.
    const kb=await page.evaluate(()=>{const vv=window.visualViewport;
      Object.defineProperty(vv,'height',{configurable:true,get:()=>500});Object.defineProperty(vv,'offsetTop',{configurable:true,get:()=>336});
      syncVisualViewport();const shell=document.querySelector('.app-shell').getBoundingClientRect(),c=document.querySelector('#composerInput').getBoundingClientRect();
      delete vv.height;delete vv.offsetTop;syncVisualViewport();
      return {top:Math.round(shell.top),bottom:Math.round(shell.bottom),composerBottom:Math.round(c.bottom)};});
    assert.deepEqual([kb.top,kb.bottom],[336,836]);
    assert(kb.composerBottom<=836&&kb.composerBottom>700,JSON.stringify(kb));
    // iOS home-screen app: iOS keeps a strip at the bottom of the screen out of the page.
    // The app stays inside the page and drops the home-indicator gap above the composer.
    const pwaFill=await page.evaluate(()=>{const vv=window.visualViewport;
      const dock=()=>getComputedStyle(document.querySelector('.bottom-dock')).paddingBottom;
      Object.defineProperty(navigator,'standalone',{configurable:true,get:()=>true});
      Object.defineProperty(window,'innerHeight',{configurable:true,get:()=>793});
      Object.defineProperty(vv,'height',{configurable:true,get:()=>793});
      syncVisualViewport();const full=[Math.round(document.querySelector('.app-shell').getBoundingClientRect().bottom),dock()];
      Object.defineProperty(vv,'height',{configurable:true,get:()=>450});
      syncVisualViewport();const typing=[Math.round(document.querySelector('.app-shell').getBoundingClientRect().bottom),document.documentElement.classList.contains('keyboard-open')];
      delete navigator.standalone;delete window.innerHeight;delete vv.height;syncVisualViewport();
      return {full,typing,after:document.documentElement.classList.contains('keyboard-open'),short:document.documentElement.classList.contains('short-viewport')};});
    assert.deepEqual(pwaFill,{full:[793,'6px'],typing:[450,true],after:false,short:false});
    // The chat list is newest first; an old pin doesn't push today's chats down.
    assert.deepEqual(await page.evaluate(()=>{const saved=state.conversations;
      state.conversations=[{id:'old',harness:'agy',title:'old pinned',pinned:true,updated_at:1793740899998},
        {id:'new',harness:'claude',title:'newest',pinned:false,updated_at:Math.floor(Date.now()/1000)}];
      renderDrawerList();const order=[...document.querySelectorAll('#convList .conv-item-title')].map(t=>t.textContent.trim());
      state.conversations=saved;renderDrawerList();return order;}),['newest','📌 old pinned']);
    await page.getByRole('button',{name:'Menu',exact:true}).tap();
    await page.waitForFunction(()=>Math.abs(document.querySelector('#drawer').getBoundingClientRect().left)<1);
    await page.screenshot({path:path.join(artifacts,`${engineName}-drawer-recent.png`)});
    await page.evaluate(()=>closeOverlays());
    // Minimal by default: no window bar and no frame around the one visible pane.
    assert.equal(await page.locator('#tmuxBar').isVisible(),false);
    assert.equal(await page.locator('.pane-head').isVisible(),false);
    // Panes: each harness's newest chat plus chats opened on purpose, never a pile of old ones.
    assert.deepEqual(await page.evaluate(()=>{const saved={panes:state.panes,pinned:state.pinnedPanes,dismissed:state.dismissedPanes,active:state.activeId};
      const now=Date.now(),c=(id,harness,ago)=>({id,harness,updated_at:Math.floor((now-ago)/1000)});
      const convs=[c('c3','claude',1e3),c('c2','claude',2e3),c('c1','claude',3e3),c('x1','codex',5e3),c('k1','keel',30*864e5)];
      state.panes=['c1','c2','c3'];state.pinnedPanes=new Set();state.dismissedPanes=new Set();state.activeId='c3';
      syncPanes(convs,now);const fresh=[...state.panes];
      state.pinnedPanes.add('c1');state.panes.push('c1');syncPanes(convs,now);const pinned=[...state.panes];
      convs.unshift(c('c4','claude',0));state.pinnedPanes.clear();state.activeId='x1';syncPanes(convs,now);const replaced=[...state.panes];
      Object.assign(state,{panes:saved.panes,pinnedPanes:saved.pinned,dismissedPanes:saved.dismissed,activeId:saved.active});savePanes();
      return {fresh,pinned,replaced};}),
      {fresh:['c3','x1'],pinned:['c3','x1','c1'],replaced:['c4','x1']});
    // Multiplex: a phone shows one pane and swipes between them; the bar is opt-in.
    const panes=await page.evaluate(()=>state.panes), timelineId=await page.evaluate(()=>state.activeId);
    assert(panes.length>=3);
    assert.equal(await page.locator('.pane').count(),1);
    await page.getByRole('button',{name:'Menu',exact:true}).tap();
    await page.locator('#quickSettingsBtn').tap();
    await page.locator('.quick-row',{hasText:'Window bar'}).getByRole('button',{name:'off',exact:true}).tap();
    await closePanel();
    assert.equal(await page.locator('#tmuxBar').isVisible(),true);
    await page.evaluate(dx=>{
      const el=document.querySelector('#chatScroll'), r=el.getBoundingClientRect(), y=r.top+120, x=r.left+r.width/2;
      const at=(type,cx)=>el.dispatchEvent(new PointerEvent(type,{bubbles:true,pointerId:9,pointerType:'touch',isPrimary:true,clientX:cx,clientY:y}));
      at('pointerdown',x); at('pointerup',x+dx);
    },-120);
    const swiped=panes[(panes.indexOf(timelineId)+1)%panes.length];
    await page.waitForFunction(id=>state.detail?.summary.id===id&&document.querySelector('.pane.focused #chatScroll'),swiped);
    await page.locator('#tmuxBar').getByRole('button',{name:'Window '+panes.indexOf(timelineId)+': timeline-phone',exact:true}).tap();
    await page.waitForFunction(id=>state.detail?.summary.id===id,timelineId);
    await page.locator('#chatScroll').getByText('The port is 7770.',{exact:true}).waitFor();
    await page.setViewportSize({width:1280,height:800});
    await page.waitForFunction(n=>document.querySelectorAll('.pane').length===n&&document.querySelectorAll('.pane-scroll .t-turn').length>0,panes.length);
    await page.screenshot({path:path.join(artifacts,`${engineName}-multiplex-desktop.png`)});
    await page.setViewportSize({width:393,height:852});
    await page.waitForFunction(()=>document.querySelectorAll('.pane').length===1);
    await page.getByRole('button',{name:'Menu',exact:true}).tap();
    await page.locator('#settingsBtn').tap();
    await page.getByRole('heading',{name:'Harnesses & accounts'}).waitFor();
    await closePanel();
    assert.deepEqual(errors,[]);
    const bad=await context.request.post(origin+'/api/conversations',{data:'not-json',headers:{'Content-Type':'application/json'}});
    assert.equal(bad.status(),400);
    const invalid=await context.request.post(origin+'/api/conversations',{data:{harness:'codex',workdir:'/does/not/exist'}});
    assert.equal(invalid.status(),400);
    const pwaContext = await browser.newContext({viewport:{width:393,height:852},isMobile:true,hasTouch:true});
    await pwaContext.addCookies(await context.cookies());
    await pwaContext.addInitScript(id=>localStorage.setItem('helm_active_id',id),first);
    const pwa = await pwaContext.newPage();
    await pwa.goto(origin);
    await pwa.waitForFunction(id=>state.detail?.summary.id===id,first);
    await pwa.evaluate(()=>navigator.serviceWorker.ready);
    await pwa.reload();
    await pwa.waitForFunction(id=>navigator.serviceWorker.controller&&state.detail?.summary.id===id,first);
    await pwa.locator('#composerInput').fill('pwa-reopen-phone');
    await pwa.getByRole('button',{name:'Send',exact:true}).tap();
    await pwa.locator('#chatScroll').getByText('Harness received: pwa-reopen-phone',{exact:true}).waitFor();
    await pwa.screenshot({path:path.join(artifacts,`${engineName}-pwa-reopen.png`)});
    await pwaContext.close();
    // /rc launches a real PTY, not a print-mode model prompt. Native CLI
    // questions are answered explicitly, and history is discovered normally.
    await send('/rc');
    await page.locator('#rcOutput').filter({hasText:'trust this project'}).waitFor();
    assert.equal(await page.locator('#rcOpenLink').isVisible(),false);
    const rcViews=await (await context.request.get(origin+'/api/claude-rc')).json();
    const rcId=rcViews.sessions[0].id;
    const unauthenticated=await browser.newContext();
    assert.equal((await unauthenticated.request.get(origin+'/api/claude-rc')).status(),401);
    await unauthenticated.close();
    assert.equal((await context.request.post(origin+'/api/claude-rc',{data:{workdir:alpha,profile:'missing-account'}})).status(),400);
    assert.equal((await context.request.post(origin+'/api/claude-rc/'+rcId+'/input',{data:{text:'x'.repeat(4097)}})).status(),400);
    assert.equal((await context.request.post(origin+'/api/claude-rc/'+rcId+'/input',{data:{key:'run-shell'}})).status(),400);
    const duplicate=await context.request.post(origin+'/api/claude-rc',{data:{workdir:alpha}});
    assert.equal((await duplicate.json()).id,rcId);
    const rcReceipt=readReceipts().find(r=>r.kind==='claude'&&r.args[0]==='remote-control');
    assert.equal(rcReceipt.tty,true);
    assert.equal(rcReceipt.args[0],'remote-control');
    assert(!rcReceipt.args.includes('-p'));
    await page.locator('#rcTerminalInput').fill('yes');
    await page.route('**/input',route=>route.fulfill({status:500,json:{error:'Native terminal reply rejected'}}));
    await page.getByRole('button',{name:'Send Enter',exact:true}).tap();
    await page.locator('#rcError').filter({hasText:'Native terminal reply rejected'}).waitFor();
    assert.equal(await page.locator('#rcTerminalInput').inputValue(),'yes');
    await page.unroute('**/input');
    await page.getByRole('button',{name:'Send Enter',exact:true}).tap();
    await page.locator('#rcOpenLink').waitFor({state:'visible'});
    assert.equal(await page.locator('#rcStartForm').isVisible(),false);
    assert.equal(await page.locator('#rcOpenLink').getAttribute('href'),'https://claude.ai/code/session_fixture?env=local');
    await page.screenshot({path:path.join(artifacts,`${engineName}-claude-remote-control.png`)});
    const workerPid=Number(fs.readFileSync(receipts+'.rc-worker','utf8'));
    await closePanel();
    await page.waitForFunction(()=>state.conversations.some(c=>c.harness==='claude'&&c.last_preview.includes('Native app reply fixture')));
    await send('/rc');
    await page.locator('#rcOpenLink').waitFor({state:'visible'});
    assert.equal(await page.locator('#rcStartForm').isVisible(),false);
    assert.equal(readReceipts().filter(r=>r.kind==='claude'&&r.args[0]==='remote-control').length,1);
    await page.getByRole('button',{name:'Stop Remote Control',exact:true}).tap();
    await page.locator('#rcStatus').filter({hasText:'Remote Control stopped.'}).waitFor();
    const alive=pid=>{
      try {return fs.readFileSync('/proc/'+pid+'/stat','utf8').split(') ')[1][0]!=='Z'} catch {return false}
    };
    for(let i=0;i<30&&(alive(rcReceipt.pid)||alive(workerPid));i++) await sleep(100);
    assert.equal(alive(rcReceipt.pid),false);
    assert.equal(alive(workerPid),false);
    const failProject=path.join(testHome,'Projects/rc-fail');fs.mkdirSync(failProject);
    await page.locator('#rcWorkdir').fill(failProject);
    // Editing a stopped session must survive the previous one-second poll.
    await sleep(1500);
    assert.equal(await page.locator('#rcWorkdir').inputValue(),failProject);
    await page.getByRole('button',{name:'Start Remote Control',exact:true}).tap();
    await page.locator('#rcOutput').filter({hasText:'requires a full-scope login token'}).waitFor();
    await page.locator('#rcStatus').filter({hasText:'could not keep Remote Control running'}).waitFor();
    assert.equal(await page.locator('#rcOpenLink').isVisible(),false);
    await closePanel();
    const profileResponse=await context.request.post(origin+'/api/profiles',{data:{harness:'claude'}});
    const profileData=await profileResponse.json();
    assert.equal(profileResponse.status(),200);
    await page.evaluate(()=>fetchState());
    await page.locator('#harnessPillBtn').tap();
    await page.getByRole('button',{name:'Claude app · Remote Control',exact:true}).tap();
    await page.locator('#rcWorkdir').fill(beta);
    await page.locator('#rcProfile').selectOption(profileData.profile.id);
    await page.getByRole('button',{name:'Start Remote Control',exact:true}).tap();
    await page.locator('#rcOutput').filter({hasText:'trust this project'}).waitFor();
    await page.locator('#rcTerminalInput').fill('yes');
    await page.getByRole('button',{name:'Send Enter',exact:true}).tap();
    await page.locator('#rcOpenLink').waitFor({state:'visible'});
    const active=readReceipts().filter(r=>r.kind==='claude').at(-1);
    assert.equal(active.inheritedPrimaryToken,false);
    assert.equal(active.profileConfig,path.join(profileData.profile.home,'.claude'));
    await closePanel();
    // With a second signed-in account, the composer gets a tap-to-switch account pill.
    await page.evaluate(()=>pickProfile('claude',null));
    await page.locator('#accountPillBtn').filter({hasText:'#1'}).waitFor();
    await page.locator('#accountPillBtn').tap();
    await page.locator('#accountPillBtn').filter({hasText:'#2'}).waitFor();
    assert.equal(await page.evaluate(()=>state.selectedProfile),profileData.profile.id);
    await page.locator('#accountPillBtn').tap();
    await page.locator('#accountPillBtn').filter({hasText:'#1'}).waitFor();
    assert.equal(await page.evaluate(()=>state.selectedProfile),null);
    // Closing the gateway also closes its terminal; it must not orphan RC.
    const exited=new Promise(resolve=>child.once('exit',resolve));child.kill(engineName==='chromium'?'SIGTERM':'SIGKILL');await exited;
    const lastWorker=Number(fs.readFileSync(receipts+'.rc-worker','utf8'));
    for(let i=0;i<30&&(alive(active.pid)||alive(lastWorker));i++) await sleep(100);
    assert.equal(alive(active.pid),false);
    assert.equal(alive(lastWorker),false);
    console.log(`${engineName}: project/model choices, spawn, resume, polling taps, navigation races, errors, stop, reconnect, archive, allow/deny settings and native Remote Control (PTY, prompts, discovery, accounts, cleanup) passed`);
  });
}
