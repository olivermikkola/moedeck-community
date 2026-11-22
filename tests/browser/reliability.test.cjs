const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const net = require('node:net');
const {spawn,execFileSync} = require('node:child_process');
const {chromium,webkit} = require('playwright');
const binary = process.env.MOEDECK_BIN || path.resolve(__dirname,'../../target/debug/moedeck');
const artifacts = path.join(__dirname,'artifacts');
fs.mkdirSync(artifacts,{recursive:true});
const sleep = ms => new Promise(resolve=>setTimeout(resolve,ms));
async function stop(child) {
  if(child && child.exitCode===null && child.signalCode===null) {
    child.kill('SIGTERM'); await new Promise(resolve=>child.once('exit',resolve));
  }
}
const cli = `#!/usr/bin/env node
const fs=require('node:fs'),readline=require('node:readline');
if(process.argv.includes('--version')) {console.log('fixture 1');process.exit(0)}
const emit=v=>console.log(JSON.stringify(v));
readline.createInterface({input:process.stdin}).on('line',line=>{
  const m=JSON.parse(line);
  if(m.method==='initialize')emit({id:m.id,result:{userAgent:'fixture'}});
  if(m.method==='thread/start'||m.method==='thread/resume')emit({id:m.id,result:{thread:{id:'owned-native'}}});
  if(m.method==='turn/start') {
    fs.writeFileSync(process.env.MOEDECK_RUN_PID,String(process.pid));
    emit({id:m.id,result:{turn:{id:'turn-owned',status:'inProgress'}}});
    emit({method:'item/started',params:{item:{type:'commandExecution',id:'cmd-owned',command:'sleep 30',status:'inProgress'}}});
  }
});
`;
for(const engine of (process.env.MOEDECK_BROWSERS||'chromium,webkit').split(',')) {
  test(`${engine}: imported Codex history, truthful activity and composer Stop`,{timeout:60000},async t=>{
    const root=fs.mkdtempSync(path.join(os.tmpdir(),'moedeck-reliability-'));
    const home=path.join(root,'home'),config=path.join(root,'config'),bin=path.join(home,'.local/bin');
    const project=path.join(home,'Projects/proof'),codex=path.join(home,'.codex'),sessions=path.join(codex,'sessions/2026/11/15');
    for(const p of [config,bin,project,sessions])fs.mkdirSync(p,{recursive:true});
    fs.symlinkSync(process.execPath,path.join(bin,'node'));
    fs.writeFileSync(path.join(bin,'codex'),cli,{mode:0o755});
    const imageFile=path.join(project,'attached.png');
    fs.writeFileSync(imageFile,Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+j8MsAAAAASUVORK5CYII=','base64'));
    const at=new Date().toISOString();
    const event=(type,rest={})=>({timestamp:at,type:'event_msg',payload:{type,...rest}});
    const response=(id,role,text)=>({timestamp:at,type:'response_item',payload:{type:'message',id,role,content:[{type:'output_text',text}]}});
    const user=(id,text)=>event('item_completed',{item:{type:'UserMessage',id,content:[{text},...(id==='ui-user'?[{type:'local_image',path:imageFile}]:[])]}});
    const answer=(id,text)=>event('item_completed',{item:{type:'AgentMessage',id,content:[{text}]}});
    const command=(id,code)=>event('item_completed',{item:{type:'CommandExecution',id,command:['bash','-lc',`python3 check-${id}.py`],exit_code:code,status:code?'failed':'completed',aggregated_output:code?'EXPECTED_FAILURE':'CHECK_OK'}});
    const history=[event('task_started'),{type:'turn_context',payload:{model:'fixture-model'}},
      response('injected','user','INTERNAL_INSTRUCTIONS_MUST_NOT_RENDER'),response('transport-user','user','Inspect my project'),user('ui-user','Inspect my project'),
      answer('msg-one','I am checking the project.'),response('msg-one','assistant','I am checking the project.'),
      command('one',0),command('two',1),answer('msg-final','The checks are complete.'),response('msg-final','assistant','The checks are complete.'),event('task_complete'),
      event('task_started'),user('ui-user-2','Repeat that'),answer('msg-repeat','The checks are complete.'),response('msg-repeat','assistant','The checks are complete.'),event('task_complete')];
    const files={};
    for(const [id,records] of Object.entries({history,active:[event('task_started'),user('active-user','Keep working'),answer('active-answer','Still working.')],stale:[event('task_started'),user('stale-user','Old interrupted task')]})) {
      files[id]=path.join(sessions,`rollout-${id}.jsonl`);
      fs.writeFileSync(files[id],records.map(JSON.stringify).join('\n')+'\n');
    }
    // Older Codex schema: optional name/preview/originator/ms columns absent.
    const sql=[`CREATE TABLE threads (id text,title text,first_user_message text,cwd text,model text,reasoning_effort text,updated_at integer,source text,archived integer);`];
    const quote=s=>"'"+s.replaceAll("'","''")+"'";
    for(const [i,id] of ['active','history','stale',...Array.from({length:15},(_,i)=>'older-'+i)].entries()) {
      sql.push(`INSERT INTO threads VALUES (${quote(id)},${quote('Codex '+id)},'User prompt',${quote(id==='active'?home:project)},'fixture-model','medium',${Math.floor(Date.now()/1000)-i},'vscode',0);`);
    }
    execFileSync('sqlite3',[path.join(codex,'state_4.sqlite')],{input:sql.join('\n')});
    let native,openCodeServer,gateway,browser;
    t.after(async()=>{await browser?.close();await stop(gateway);await stop(native);await stop(openCodeServer);console.log(`${engine} reliability evidence: ${root}`)});
    // A real open writer is required for the native activity observation.
    native=spawn(process.execPath,['-e',`process.title='codex';const fs=require('node:fs');for(const p of process.argv.slice(1))fs.openSync(p,'a');console.log('ready');setInterval(()=>{},1000);`,files.active,files.history],{stdio:['ignore','pipe','inherit']});
    await new Promise((resolve,reject)=>{native.stdout.once('data',resolve);native.once('error',reject)});
    const openCodeData=path.join(home,'.local/share/opencode');fs.mkdirSync(openCodeData,{recursive:true});
    const openCodeDb=path.join(openCodeData,'opencode.db');
    execFileSync('sqlite3',[openCodeDb],{input:`
      CREATE TABLE session_v2 (id text,title text,directory text,model text,parent_id text,time_updated integer,time_archived integer,time_busy integer,time_idle integer);
      INSERT INTO session_v2 VALUES ('native-open','OpenCode native',${quote(project)},'{}',NULL,${Date.now()},NULL,${Date.now()},0);
      CREATE TABLE session_message (session_id text,type text,seq integer,data text);
      INSERT INTO session_message VALUES ('native-open','user',1,'{"text":"Native OpenCode prompt"}');
      INSERT INTO session_message VALUES ('native-open','assistant',2,'{"content":[{"type":"tool","name":"bash","state":{"status":"running","input":{"command":"sleep 30"}}}]}');
    `});
    openCodeServer=spawn(process.execPath,['-e',`process.title='opencode';require('node:fs').openSync(process.argv[1],'a');console.log('ready');setInterval(()=>{},1000);`,openCodeDb],{stdio:['ignore','pipe','inherit']});
    await new Promise((resolve,reject)=>{openCodeServer.stdout.once('data',resolve);openCodeServer.once('error',reject)});
    const socket=net.createServer();await new Promise(r=>socket.listen(0,'127.0.0.1',r));const port=socket.address().port;await new Promise(r=>socket.close(r));
    const origin=`http://127.0.0.1:${port}`,pidFile=path.join(root,'owned.pid');
    gateway=spawn(binary,[],{env:{...process.env,HOME:home,CODEX_HOME:codex,MOEDECK_CONFIG_DIR:config,MOEDECK_PORT:String(port),MOEDECK_PASSCODE:'reliability-proof',MOEDECK_RUN_PID:pidFile},stdio:'ignore'});
    for(let n=0;;n++) {try{if((await fetch(origin+'/api/health')).ok)break}catch{} assert(n<100,'gateway startup');await sleep(50)}
    const launch={headless:true};
    if(engine==='webkit'&&process.env.MOEDECK_WEBKIT_LIBS) {
      const base=path.join(path.dirname(webkit.executablePath()),'minibrowser-wpe');
      launch.executablePath=path.join(base,'bin/MiniBrowser');
      launch.env={...process.env,LD_LIBRARY_PATH:`${process.env.MOEDECK_WEBKIT_LIBS}:${base}/lib:${base}/sys/lib`,WEBKIT_EXEC_PATH:`${base}/bin`,WEBKIT_INJECTED_BUNDLE_PATH:`${base}/lib`,WEBKIT_INSPECTOR_RESOURCES_PATH:`${base}/share`};
    }
    browser=await ({chromium,webkit}[engine]).launch(launch);
    const context=await browser.newContext({viewport:{width:393,height:852},isMobile:true,hasTouch:true,serviceWorkers:'block'});
    const page=await context.newPage();page.setDefaultTimeout(10000);
    const errors=[];page.on('pageerror',e=>errors.push(e.message));
    await page.goto(origin+'/?c=codex%3Aactive');await page.locator('#loginPass').fill('reliability-proof');await page.locator('#loginForm button').tap();
    await page.waitForFunction(()=>state.bootstrappedOnce);
    await page.waitForFunction(()=>state.detail?.summary.id==='codex:active');
    await page.evaluate(()=>fetchState());
    assert.equal(await page.evaluate(()=>state.selectedWorkdir),home,'polling must preserve the linked chat workspace instead of borrowing another chat project');
    assert.equal(await page.evaluate(()=>state.detail.summary.workdir),home);
    assert.match(await page.locator('#convSubtitle').textContent(),/home$/);
    await page.evaluate(()=>startNewChat());
    await page.evaluate(()=>fetchState());
    assert.equal(await page.evaluate(()=>state.selectedWorkdir),home,'polling must also preserve the new-chat draft project');

    assert.equal(await page.evaluate(()=>state.conversations.filter(c=>c.id.startsWith('codex:')).length),18,'all chats must be discoverable beyond the old 12-chat limit');
    await page.evaluate(()=>loadConversation('codex:history',true));
    await page.waitForFunction(()=>state.detail?.summary.id==='codex:history');
    assert.equal(await page.locator('#chatScroll .t-text').filter({hasText:'I am checking the project.'}).count(),1);
    assert.equal(await page.locator('#chatScroll .t-text').filter({hasText:'The checks are complete.'}).count(),2,'intentional repeated answers in distinct turns survive');
    assert.equal(await page.getByText('INTERNAL_INSTRUCTIONS_MUST_NOT_RENDER',{exact:true}).count(),0);
    await page.locator('#chatScroll .msg-img-card').waitFor();
    assert.equal(await page.locator('#chatScroll .msg-img-card').evaluate(img=>img.complete&&img.naturalWidth>0),true,'native image attachments must load');
    assert.equal(await page.locator('#chatScroll .status-line').count(),0,'finished history must not say Running');
    assert.equal(await page.locator('#chatScroll .msg-assistant').count(),2,'updates and tools belong to one turn');
    const steps=page.locator('#chatScroll details.steps').first();
    assert.equal(await steps.getAttribute('open'),null);await steps.locator(':scope > summary').tap();
    assert.match(await steps.locator('summary.step-name').first().textContent(),/^Ran python3/);
    assert.equal(await steps.locator('.step-raw').first().isVisible(),false,'raw commands require their own disclosure');
    await steps.getByRole('button',{name:'Load full step details',exact:true}).tap();
    await page.waitForFunction(()=>state.detail.messages.some(m=>m.blocks?.some(b=>b.output==='CHECK_OK')));
    await steps.locator('summary.step-name').first().tap();
    assert.match(await steps.locator('.step-raw').first().textContent(),/CHECK_OK/);
    assert.equal(await steps.locator('.step-raw').first().isVisible(),true);
    assert.match(await steps.innerText(),/That step failed/);
    await page.evaluate(()=>loadConversation('codex:history'));
    assert.equal(await steps.locator('.step-raw').first().isVisible(),true,'polling must preserve open step details');
    await page.screenshot({path:path.join(artifacts,`${engine}-reliable-history.png`)});
    await page.evaluate(()=>loadConversation('codex:active',true));
    await page.waitForFunction(()=>state.detail?.summary.id==='codex:active'&&state.detail.summary.status==='busy');
    assert.equal(await page.locator('#sendBtn').getAttribute('aria-label'),'Running in Codex');
    assert.equal(await page.locator('#sendBtn').isDisabled(),true);
    assert.equal(await page.locator('#runningStopBtn').isVisible(),false);
    assert.match(await page.locator('#runningTaskText').textContent(),/Control this turn in Codex/);
    const headers={Origin:origin};
    const refused=await context.request.post(origin+'/api/conversations/codex%3Aactive/stop',{headers,data:{}});
    assert.equal(refused.status(),409);
    const blocked=await context.request.post(origin+'/api/conversations/codex%3Aactive/messages',{headers,data:{text:'Do not interrupt native work',harness:'codex',workdir:project}});
    assert.equal(blocked.ok(),false);
    assert.equal(fs.existsSync(pidFile),false,'observing a native chat must not launch a competing run');
    await page.screenshot({path:path.join(artifacts,`${engine}-native-running.png`)});
    fs.appendFileSync(files.active,JSON.stringify(event('task_complete'))+'\n');
    await page.evaluate(()=>loadConversation('codex:active'));
    await page.waitForFunction(()=>state.detail.summary.status==='idle');
    await page.evaluate(()=>loadConversation('codex:stale',true));
    assert.equal(await page.evaluate(()=>state.detail.summary.status),'idle','a crashed native writer is not still Running');
    await page.evaluate(()=>loadConversation('opencode:native-open',true));
    assert.equal(await page.evaluate(()=>state.detail.summary.status),'unknown');
    assert.equal(await page.locator('#sendBtn').getAttribute('aria-label'),'Read-only history');
    assert.equal(await page.evaluate(()=>state.detail.messages.flatMap(m=>m.tools||[]).some(t=>t.status==='running')),false,'old native flags must not look like live tools');
    assert.equal(await page.locator('#sendBtn').isDisabled(),true);
    assert.match(await page.locator('#runningTaskText').textContent(),/Read-only OpenCode history/);
    const createdGoal=await context.request.post(origin+'/api/goals',{headers,data:{title:'Scheduled proof',workdir:project,schedule:'1h',subgoals:[{title:'Future work',prompt:'Not executed by this test',harness:'opencode'}]}});
    assert.equal(createdGoal.ok(),true);const scheduled=await createdGoal.json();
    // Bootstrap is polled every 0.7–2 s: it must carry a goal summary, not the
    // subgoal bodies (a triage storm once grew that payload to 8 MB per poll).
    const bootRaw=await (await context.request.get(origin+'/api/bootstrap')).text();
    assert.equal(bootRaw.includes('Not executed by this test'),false,'bootstrap must not ship subgoal prompts');
    const bootGoal=JSON.parse(bootRaw).goals.find(g=>g.id===scheduled.id);
    assert.equal(bootGoal.subgoal_count,1);
    assert.equal(bootGoal.subgoals,undefined);
    await page.evaluate(()=>fetchState());
    await page.getByRole('button',{name:'Menu',exact:true}).tap();
    await page.locator('#backgroundWorkBtn').tap();
    await page.getByRole('button',{name:'Pause scheduling',exact:true}).tap();
    await page.waitForFunction(id=>state.goals.find(g=>g.id===id)?.status==='paused',scheduled.id);
    const savedGoals=JSON.parse(fs.readFileSync(path.join(config,'goals.json'),'utf8'));
    assert.equal(savedGoals[scheduled.id].status,'paused');
    await page.screenshot({path:path.join(artifacts,`${engine}-background-goal-paused.png`)});
    await page.emulateMedia({colorScheme:'dark'});
    await page.screenshot({path:path.join(artifacts,`${engine}-background-goal-paused-dark.png`)});
    await page.emulateMedia({colorScheme:'light'});
    await page.getByRole('button',{name:'Close panel',exact:true}).tap();
    const report=await (await context.request.get(origin+'/api/agents')).json();
    assert.equal(report.agents.some(a=>a.harness==='opencode'),false,'a shared server cannot prove a native chat is running');
    assert.equal(report.agents.some(a=>a.id==='codex:stale'),false,'another live Codex process cannot revive an abandoned chat');
    const completed=report.agents.find(a=>a.id==='codex:history');
    assert.equal(completed.state,'waiting','a completed turn is not Working just because its file is recent');
    await page.screenshot({path:path.join(artifacts,`${engine}-opencode-readonly.png`)});
    // The composer controls an actual gateway-owned subprocess, with no model calls.
    const created=await context.request.post(origin+'/api/conversations',{headers,data:{harness:'codex',workdir:project}});
    const {id}=await created.json();await page.evaluate(id=>loadConversation(id,true),id);
    await page.locator('#composerInput').fill('Run until I stop you');await page.getByRole('button',{name:'Send',exact:true}).tap();
    await page.getByRole('button',{name:'Stop',exact:true}).filter({has:page.locator('svg')}).waitFor();
    await page.waitForFunction(()=>state.detail?.summary.can_stop===true);
    const pid=Number(fs.readFileSync(pidFile,'utf8'));
    await page.screenshot({path:path.join(artifacts,`${engine}-composer-stop.png`)});
    await page.locator('#sendBtn').tap();
    await page.waitForFunction(()=>state.detail?.summary.status==='idle'&&!sendingTurn);
    for(let n=0;fs.existsSync('/proc/'+pid);n++){assert(n<100,'Stop did not terminate the owned process');await sleep(30)}
    assert.equal(await page.locator('#sendBtn').getAttribute('aria-label'),'Send');
    assert.deepEqual(errors,[]);
    console.log(`${engine}: duplicate-free history; 18 old-schema chats; native images; completed commands; persistent compact steps; native liveness; read-only OpenCode; persisted goal pause; real composer Stop passed`);
  });
}
