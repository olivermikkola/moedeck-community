const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const net = require('node:net');
const { spawn, execFileSync } = require('node:child_process');

const binary = process.env.MOEDECK_BIN || path.resolve(__dirname, '../../target/debug/moedeck');
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

// Scripted native processes verify gateway transport, not model behavior.
const cli = `#!/usr/bin/env node
const fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
const args=process.argv.slice(2),kind=path.basename(process.argv[1]),emit=v=>console.log(JSON.stringify(v));
if(args.includes('--version')) {console.log('fixture 2.2.5');process.exit(0)}
if(args[0]==='models') {if(kind==='agy')console.log('gemini-3.8-flash-low\\tGemini 3.8 Flash (Low)');process.exit(0)}
if(kind==='cursor-agent') {
  const flag='--resume';
  const sid=args.includes(flag)?args[args.indexOf(flag)+1]:(kind==='opencode'?'ses_':'')+crypto.randomUUID();
  fs.appendFileSync(process.env.MOEDECK_RECEIPTS,JSON.stringify({kind,args,sid,pid:process.pid,config:process.env.OPENCODE_CONFIG_CONTENT||null})+'\\n');
  emit({type:'system',subtype:'init',session_id:sid,model:'Composer'});
  emit({type:'assistant',message:{role:'assistant',content:[{type:'text',text:'SESSION_OK'}]}});
  emit({type:'result',subtype:'success',is_error:false,result:'SESSION_OK',session_id:sid});
  process.exit(0);
}
const flag=kind==='claude'||kind==='grok'?'--resume':kind==='agy'?'--conversation':'--session';
const sid=args.includes(flag)?args[args.indexOf(flag)+1]:(kind==='opencode'?'ses_':'')+crypto.randomUUID();
fs.appendFileSync(process.env.MOEDECK_RECEIPTS,JSON.stringify({kind,args,sid,pid:process.pid,config:process.env.OPENCODE_CONFIG_CONTENT||null})+'\\n');
if(kind==='opencode') {
  if(args.at(-1).includes('wait-for-stop')||args.at(-1).includes('goal-wait-proof')) {emit({type:'step_start',sessionID:sid});setTimeout(()=>process.exit(0),10000);return;}
  emit({type:'error',error:{message:args.at(-1).includes('billing-proof')?'Insufficient balance. Top up your credits.':'PROVIDER_FAILURE_PROOF'}});process.exit(1);
}
if(kind==='grok') {
  emit({type:'text',data:'SESSION_OK'});
  emit({type:'tool_call',toolCallId:'t1',toolName:'read_file',title:'Read',status:'in_progress',rawInput:{path:'README.md'}});
  emit({type:'tool_call_update',toolCallId:'t1',status:'completed'});
  emit({type:'end',stopReason:'end_turn',sessionId:sid});
  return;
}
if(kind==='claude') {
  emit({type:'system',subtype:'init',session_id:sid});
  emit({type:'result',subtype:'success',result:'SESSION_OK'});
} else {
  emit({event:'init',conversation_id:sid});
  emit({event:'step_update',step_update:{text_delta:'SESSION_'}});
  emit({event:'step_update',step_update:{text_delta:'OK'}});
  emit({event:'result',result:{status:'SUCCESS',response:'SESSION_OK'}});
}
`;

test('harness sessions, OpenCode history, failures and Safe policy', { timeout: 60000 }, async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'moedeck-harness-'));
  const home = path.join(root, 'home'), config = path.join(root, 'config');
  const bin = path.join(home, '.local/bin'), project = path.join(home, 'Projects/proof');
  const data = path.join(home, '.local/share/opencode');
  [bin, project, data, config].forEach(p => fs.mkdirSync(p, { recursive: true }));
  // The gateway replaces PATH. The fixtures' shebang needs node on that path.
  fs.symlinkSync(process.execPath, path.join(bin, 'node'));
  for (const h of ['claude', 'agy', 'opencode', 'cursor-agent', 'grok']) fs.writeFileSync(path.join(bin, h), cli, { mode: 0o755 });
  const receipts = path.join(root, 'receipts.jsonl');
  const db = path.join(data, 'opencode.db');
  const oldTime = Date.now() - 60000;
  const statements = [
    'CREATE TABLE session_v2 (id text,title text,directory text,model text,parent_id text,time_updated integer);',
    'CREATE TABLE session_message (session_id text,type text,seq integer,data text);'
  ];
  const sqlString = s => "'" + s.replaceAll("'", "''") + "'";
  for (const sid of ['ses_long', 'ses_error', 'ses_repeat', 'ses_stale']) {
    statements.push(`INSERT INTO session_v2 VALUES (${sqlString(sid)},'proof',${sqlString(project)},'{}',NULL,${oldTime});`);
  }
  const insert = (sid, seq, role, text) => {
    const value = role === 'user' ? {text,time:{created:oldTime+seq}} : {content:[{type:'text',text}],finish:'stop',time:{created:oldTime+seq}};
    statements.push(`INSERT INTO session_message VALUES ('${sid}','${role}',${seq},${sqlString(JSON.stringify(value))});`);
  };
  for (let i = 1; i <= 404; i++) insert('ses_long', i, i % 2 ? 'user' : 'assistant', `MESSAGE_${i}`);
  insert('ses_error', 1, 'assistant', 'OLD_NATIVE_ANSWER');
  insert('ses_repeat', 1, 'user', 'Repeat this prompt');
  insert('ses_repeat', 2, 'assistant', 'Previous reply');
  insert('ses_stale', 1, 'user', 'Native prompt');
  insert('ses_stale', 2, 'assistant', 'Native finished reply');
  execFileSync('sqlite3', [db], { input: statements.join('\n') });
  const socket = net.createServer();
  await new Promise(resolve => socket.listen(0, '127.0.0.1', resolve));
  const port = socket.address().port;
  await new Promise(resolve => socket.close(resolve));
  const origin = `http://127.0.0.1:${port}`;
  // Pre-rename HELM_* settings on purpose: existing service files still use them.
  const startGateway = () => spawn(binary, [], {env:{...process.env,HOME:home,CODEX_HOME:path.join(home,'.codex'),
    HELM_CONFIG_DIR:config,HELM_PORT:String(port),HELM_PASSCODE:'harness-proof',MOEDECK_RECEIPTS:receipts,
    XAI_API_KEY:'xai-secret-not-argv'},stdio:'ignore'});
  let child=startGateway();
  t.after(async () => {
    child.kill('SIGTERM');
    if (child.exitCode === null && child.signalCode === null) await new Promise(resolve => child.once('exit', resolve));
    console.log(`harness subprocess evidence: ${root}`);
  });
  for (let i = 0; ; i++) {
    try { if ((await fetch(origin+'/api/health')).ok) break; } catch {}
    if (i > 100) throw Error('Gateway did not start');
    await sleep(50);
  }
  let cookie = '';
  const request = async (route, body) => {
    const response = await fetch(origin+route, {method:body===undefined?'GET':'POST',headers:{'Content-Type':'application/json',Origin:origin,Cookie:cookie},body:body===undefined?undefined:JSON.stringify(body)});
    if (route === '/api/auth/login') cookie = response.headers.get('set-cookie').split(';')[0];
    const value = await response.json();
    assert(response.ok, JSON.stringify(value));
    return value;
  };
  await request('/api/auth/login', {passcode:'harness-proof'});
  const route = id => '/api/conversations/'+encodeURIComponent(id);
  const finish = async id => {
    for (let n=0;n<100;n++) {
      const d=await request(route(id));
      if(d.summary.status!=='busy' && d.messages.some(m=>m.role==='assistant')) return d;
      await sleep(30);
    }
    throw Error('Turn did not finish');
  };
  for (const harness of ['claude','agy']) {
    const model=harness==='agy'?'gemini-3.8-flash-low':'default';
    const {id}=await request('/api/conversations',{harness,model,effort:'default',workdir:project});
    for (let turn=0;turn<2;turn++) {
      await request(route(id)+'/messages',{text:'Reply SESSION_OK',harness,model,effort:'default',workdir:project});
      const d=await finish(id);
      assert.equal(d.messages.at(-1).text,'SESSION_OK');
    }
    const runs=fs.readFileSync(receipts,'utf8').trim().split('\n').map(JSON.parse).filter(r=>r.kind===harness);
    assert.equal(runs.length,2);
    assert.equal(runs[0].sid,runs[1].sid);
    assert(runs[1].args.includes(harness==='claude'?'--resume':'--conversation'));
    assert.equal(runs[1].args.at(-1),'Reply SESSION_OK');
  }
  const {id: cursorId}=await request('/api/conversations',{harness:'cursor',model:'default',effort:'default',workdir:project});
  for (let turn=0;turn<2;turn++) {
    await request(route(cursorId)+'/messages',{text:'Reply SESSION_OK',harness:'cursor',model:'default',effort:'default',workdir:project});
    const d=await finish(cursorId);
    assert.equal(d.messages.at(-1).text,'SESSION_OK');
  }
  const cursorRuns=fs.readFileSync(receipts,'utf8').trim().split('\n').map(JSON.parse).filter(r=>r.kind==='cursor-agent');
  assert.equal(cursorRuns.length,2);
  assert.equal(cursorRuns[0].sid,cursorRuns[1].sid);
  assert(cursorRuns[1].args.includes('--resume'));
  assert(cursorRuns[0].args.includes('--force'));
  assert(cursorRuns[0].args.includes('--trust'));
  assert(cursorRuns[0].args.includes('stream-json'));
  assert(!cursorRuns[0].args.includes('--api-key'));
  assert.equal(cursorRuns[1].args.at(-1),'Reply SESSION_OK');
  const long=await request(route('opencode:ses_long'));
  assert.equal(long.messages.length,400);
  assert.equal(long.messages.at(-1).text,'MESSAGE_404');
  assert.equal(long.total_messages,404);
  assert.equal(long.truncated,4);
  const tail=await request(route('opencode:ses_long')+'?tail=20');
  assert.equal(tail.messages.length,20);
  assert.equal(tail.total_messages,404);
  assert.equal(tail.truncated,384);
  const overlayMessage=(role,text,streaming=false,harness='opencode')=>({id:role+text,role,harness,timestamp:new Date(oldTime).toISOString(),text,streaming,tools:[],images:[],blocks:[]});
  const stored={id:'opencode:ses_stale',pinned:false,harness:'opencode',workdir:project,created_at:oldTime,updated_at:oldTime,
    appended_messages:[overlayMessage('user','Native prompt'),overlayMessage('assistant','Thinking…',true)]};
  const storedPath=path.join(config,'threads/opencode_ses_stale.json');
  fs.writeFileSync(storedPath,JSON.stringify(stored));
  const recovered=await request(route(stored.id));
  assert.deepEqual(recovered.messages.map(m=>m.text),['Native prompt','Native finished reply']);
  stored.appended_messages.push(overlayMessage('assistant','Handoff reply',false,'codex'));
  fs.writeFileSync(storedPath,JSON.stringify(stored));
  assert.equal((await request(route(stored.id))).messages.at(-1).text,'Handoff reply');
  const denied = await fetch(origin+route('opencode:ses_error')+'/messages', {
    method:'POST',headers:{'Content-Type':'application/json',Origin:origin,Cookie:cookie},
    body:JSON.stringify({text:'Do not silently take over a native chat',harness:'opencode',workdir:project})
  });
  assert.equal(denied.ok,false);
  assert.match((await denied.json()).error,/Read-only OpenCode history/);
  const adoptedIds = new Map();
  for (const [sid,prompt] of [['ses_error','New prompt'],['ses_repeat','Repeat this prompt']]) {
    // Native sessions are read-only. Exercise resume on explicitly staged,
    // Deck-owned threads, keeping the prior repeated-prompt/history coverage.
    const prior = await request(route('opencode:'+sid));
    const {id} = await request('/api/conversations',{harness:'opencode',workdir:project});
    const file = path.join(config,'threads',id.replaceAll(':','_')+'.json');
    const stored = JSON.parse(fs.readFileSync(file,'utf8'));
    Object.assign(stored,{native_session:sid,appended_messages:prior.messages});
    fs.writeFileSync(file,JSON.stringify(stored));
    adoptedIds.set(sid,id);
    await request(route(id)+'/messages',{text:prompt,harness:'opencode',model:'default',effort:'default',workdir:project});
    const d=await finish(id);
    assert(d.messages.at(-1).text.includes('PROVIDER_FAILURE_PROOF'));
    assert.equal(d.messages.filter(m=>m.role==='user'&&m.text===prompt).length,sid==='ses_repeat'?2:1);
  }
  const stoppedId=adoptedIds.get('ses_error');
  await request(route(stoppedId)+'/messages',{text:'wait-for-stop',harness:'opencode',model:'default',workdir:project});
  for(let i=0;i<100;i++) {
    if(fs.readFileSync(receipts,'utf8').includes('wait-for-stop'))break;
    await sleep(20);
  }
  assert.equal((await request(route(stoppedId)+'/stop',{})).stopped,true);
  // Stop marks the run idle before its subprocess cleanup persists the result.
  for(let i=0;i<100;i++) {
    if((await request(route(stoppedId))).messages.at(-1).text.includes('Stopped by user.'))break;
    await sleep(20);
  }
  assert((await request(route(stoppedId))).messages.at(-1).text.includes('Stopped by user.'));
  await request('/api/approvals/safety-mode',{mode:'safe'});
  const {id}=await request('/api/conversations',{harness:'opencode',model:'default',workdir:project});
  await request(route(id)+'/messages',{text:'Safe fixture',harness:'opencode',model:'default',workdir:project});
  await finish(id);
  const run=fs.readFileSync(receipts,'utf8').trim().split('\n').map(JSON.parse).at(-1);
  assert(run.args.includes('--standalone'));
  assert(!run.args.includes('--auto'));
  const policies=JSON.parse(run.config).policies;
  assert.deepEqual(policies[0],{action:'tool.use',resource:'*',effect:'deny'});
  assert.equal((await request('/api/approvals')).pending.length,0);
  const cursorCount = () => fs.readFileSync(receipts,'utf8').trim().split('\n').filter(Boolean).map(JSON.parse).filter(r=>r.kind==='cursor-agent').length;
  const beforeSafe = cursorCount();
  await request(route(cursorId)+'/messages',{text:'Safe cursor',harness:'cursor',model:'default',workdir:project});
  for (let i=0;i<100 && cursorCount()===beforeSafe;i++) await sleep(30);
  const safeCursor=fs.readFileSync(receipts,'utf8').trim().split('\n').filter(Boolean).map(JSON.parse).filter(r=>r.kind==='cursor-agent').at(-1);
  assert.equal(safeCursor.args.at(-1),'Safe cursor');
  assert(safeCursor.args.includes('--mode') && safeCursor.args.includes('ask'));
  assert(!safeCursor.args.includes('--force'));

  const grokSid='11111111-1111-4111-8111-111111111111';
  const grokSub='22222222-2222-4222-8222-222222222222';
  const grokRoot=path.join(home,'.grok','sessions',encodeURIComponent(project));
  fs.mkdirSync(path.join(grokRoot,grokSid),{recursive:true});
  fs.mkdirSync(path.join(grokRoot,grokSub),{recursive:true});
  fs.writeFileSync(path.join(grokRoot,grokSid,'summary.json'),JSON.stringify({
    info:{id:grokSid,cwd:project},generated_title:'Grok proof',session_summary:'Listed from disk',
    last_turn_summary:'Disk answer',current_model_id:'grok-4.7',updated_at:new Date(oldTime).toISOString(),session_kind:'parent'
  }));
  fs.writeFileSync(path.join(grokRoot,grokSid,'updates.jsonl'),[
    JSON.stringify({method:'session/update',params:{update:{sessionUpdate:'user_message_chunk',content:{type:'text',text:'Disk prompt'}}},timestamp:oldTime}),
    JSON.stringify({method:'session/update',params:{update:{sessionUpdate:'agent_message_chunk',content:{type:'text',text:'Disk answer'}}},timestamp:oldTime+1}),
    JSON.stringify({method:'session/update',params:{update:{sessionUpdate:'turn_completed'}},timestamp:oldTime+2})
  ].join('\n'));
  fs.writeFileSync(path.join(grokRoot,grokSub,'summary.json'),JSON.stringify({
    info:{id:grokSub,cwd:project},generated_title:'child',session_kind:'subagent',updated_at:new Date(oldTime).toISOString()
  }));
  const boot=await request('/api/bootstrap');
  assert(boot.harnesses.some(h=>h.id==='grok'&&h.available&&h.binary==='grok'));
  assert(boot.conversations.some(c=>c.id==='grok:'+grokSid&&c.title==='Grok proof'));
  assert(!boot.conversations.some(c=>c.id==='grok:'+grokSub));
  const disk=await request(route('grok:'+grokSid));
  assert.deepEqual(disk.messages.map(m=>m.text),['Disk prompt','Disk answer']);

  await request('/api/approvals/safety-mode',{mode:'standard'});
  const created=await request('/api/conversations',{harness:'grok',model:'grok-4.7',effort:'high',workdir:project});
  for (let turn=0;turn<2;turn++) {
    await request(route(created.id)+'/messages',{text:'Reply SESSION_OK',harness:'grok',model:'grok-4.7',effort:'high',workdir:project});
    const d=await finish(created.id);
    assert.equal(d.messages.at(-1).text,'SESSION_OK');
    assert.equal((await request('/api/approvals')).pending.length,0);
  }
  const grokRuns=()=>fs.readFileSync(receipts,'utf8').trim().split('\n').map(JSON.parse).filter(r=>r.kind==='grok');
  let runs=grokRuns();
  assert.equal(runs.length,2);
  assert.equal(runs[0].sid,runs[1].sid);
  assert(runs[1].args.includes('--resume'));
  assert(runs[0].args.includes('--permission-mode')&&runs[0].args[runs[0].args.indexOf('--permission-mode')+1]==='auto');
  assert(!runs[0].args.includes('--always-approve'));
  assert(runs[0].args.includes('--model')&&runs[0].args[runs[0].args.indexOf('--model')+1]==='grok-4.7');
  assert(runs[0].args.includes('--effort')&&runs[0].args[runs[0].args.indexOf('--effort')+1]==='high');
  assert(runs.every(r=>!r.args.includes('xai-secret-not-argv')&&!r.args.includes('--api-key')&&!r.args.includes('XAI_API_KEY')));

  await request('/api/approvals/safety-mode',{mode:'safe'});
  await request(route(created.id)+'/messages',{text:'Safe read',harness:'grok',model:'default',effort:'default',workdir:project});
  await finish(created.id);
  runs=grokRuns();
  const safe=runs.at(-1);
  assert(safe.args[safe.args.indexOf('--permission-mode')+1]==='dontAsk');
  assert(safe.args[safe.args.indexOf('--sandbox')+1]==='read-only');
  assert(safe.args[safe.args.indexOf('--tools')+1]==='read_file,grep,list_dir');
  assert(!safe.args.includes('--always-approve'));
  assert.equal((await request('/api/approvals')).pending.length,0);

  await request('/api/approvals/safety-mode',{mode:'yolo'});
  await request(route(created.id)+'/messages',{text:'Yolo turn',harness:'grok',model:'default',effort:'default',workdir:project});
  await finish(created.id);
  const yolo=grokRuns().at(-1);
  assert(yolo.args.includes('--always-approve'));
  assert(!yolo.args.includes('--permission-mode'));
  assert(!yolo.args.includes('--sandbox'));
  assert(!yolo.args.some(arg=>arg.includes('xai-secret-not-argv')));

  // Exercise the real scheduler/runner failure path, not a copy of its logic.
  const makeGoal = async (prompt, auto_triage) => request('/api/goals', {
    title:'Bounded failure proof', workdir:project, schedule:'1h',
    ...(auto_triage === undefined ? {} : {auto_triage}),
    subgoals:[{title:'Provider failure',prompt,harness:'opencode',max_attempts:3}]
  });
  const goalRoute = g => '/api/goals/'+g.id;
  const billing = await makeGoal('billing-proof', true);
  await request(goalRoute(billing)+'/tick', {});
  const billed = await request(goalRoute(billing));
  assert.equal(billed.status,'failed');
  assert.equal(billed.subgoals.length,1);
  assert.match(billed.subgoals[0].error,/Insufficient balance/);
  assert(billed.subgoals[0].conversation_id);

  const bounded = await makeGoal('ordinary provider failure', true);
  await request(goalRoute(bounded)+'/tick', {});
  const firstFailure = await request(goalRoute(bounded));
  assert.equal(firstFailure.subgoals.length,2);
  assert.equal(firstFailure.subgoals[0].status,'blocked');
  await request(goalRoute(bounded)+'/tick', {});
  const stopped = await request(goalRoute(bounded));
  assert.equal(stopped.status,'failed');
  assert.equal(stopped.subgoals.length,2,'failed triage must never spawn another triage');
  assert.equal(stopped.subgoals[1].status,'failed');
  assert.notEqual(stopped.subgoals[0].conversation_id,stopped.subgoals[1].conversation_id);
  for (const subgoal of stopped.subgoals) {
    const detail = await request(route(subgoal.conversation_id));
    assert(detail.messages.some(m=>m.text.includes('PROVIDER_FAILURE_PROOF')),'each failure keeps its transcript');
  }
  await request(goalRoute(bounded)+'/pause', {});
  await request(goalRoute(bounded)+'/tick', {});
  assert.equal((await request(goalRoute(bounded))).status,'paused');
  assert.equal((await request(goalRoute(bounded))).subgoals.length,2);

  const optIn = await makeGoal('ordinary provider failure');
  assert.equal(optIn.auto_triage,false,'automatic remediation must be explicitly enabled');
  await request(goalRoute(optIn)+'/tick', {});
  assert.equal((await request(goalRoute(optIn))).subgoals.length,1);
  const inFlight = await makeGoal('goal-wait-proof', true);
  const tick = request(goalRoute(inFlight)+'/tick', {});
  const inFlightId = `helm:goal:${inFlight.id}:${inFlight.subgoals[0].id}`;
  for(let n=0;;n++) {
    const b = await request('/api/bootstrap');
    if(b.conversations.some(c=>c.id===inFlightId && c.status==='busy')) break;
    assert(n<100,'goal turn never started');
    await sleep(30);
  }
  await request(goalRoute(inFlight)+'/pause', {});
  await request(route(inFlightId)+'/stop', {});
  await tick;
  const paused = await request(goalRoute(inFlight));
  assert.equal(paused.status,'paused','in-flight completion must preserve pause');
  assert.equal(paused.subgoals.length,1,'pausing must prevent new triage work');
  const saved = JSON.parse(fs.readFileSync(path.join(config,'goals.json'),'utf8'));
  assert.equal(saved[inFlight.id].status,'paused','pause must survive daemon restart');
  console.log('goal proof: billing stops; failed triage stops at two tasks; distinct histories survive; pause holds during completion and persists; remediation is opt-in');

  // Cancel and delete must terminate the currently running process, not merely
  // change a database flag while a later completion overwrites cancellation.
  for (const action of ['cancel','delete']) {
    const created = await request('/api/sprints',{title:'Stop proof '+action,harness:'opencode',workdir:project,
      steps:[{name:'Held turn',prompt:'wait-for-stop '+action},{name:'Must not run',prompt:'SPRINT_NEXT_STEP_MUST_NOT_RUN'}]});
    const sprintRoute='/api/sprints/'+created.id;
    let active, run;
    for(let n=0;;n++) {
      active=await request(sprintRoute);
      const conv=active.steps[0].conversation_id;
      run=fs.readFileSync(receipts,'utf8').trim().split('\n').map(JSON.parse).find(r=>r.args.at(-1)?.includes('wait-for-stop '+action));
      if(conv && run && (await request(route(conv))).summary.status==='busy') {
        // A CLI receipt precedes its stdout being consumed by the gateway.
        // Resume proof requires that native initialization has been ingested.
        const overlay=JSON.parse(fs.readFileSync(path.join(config,'threads',conv.replaceAll(':','_')+'.json'),'utf8'));
        if(overlay.native_session)break;
      }
      assert(n<100,'sprint process did not start');await sleep(20);
    }
    let response;
    if(action==='cancel') response=await request(sprintRoute+'/cancel',{});
    else {
      response=await fetch(origin+sprintRoute,{method:'DELETE',headers:{Origin:origin,Cookie:cookie,'Content-Type':'application/json'},body:'{}'});
      assert.equal(response.ok,true,await response.text());
    }
    for(let n=0;fs.existsSync('/proc/'+run.pid);n++){assert(n<100,'sprint '+action+' left its process running');await sleep(20)}
    // Wait for the runner's final stopped outcome to be persisted.
    const conv=active.steps[0].conversation_id;
    for(let n=0;;n++) {
      const d=await request(route(conv));
      if(d.messages.at(-1).text.includes('Stopped by user.'))break;
      assert(n<100,'stopped turn outcome not persisted');await sleep(20);
    }
    if(action==='cancel') {
      const cancelled=await request(sprintRoute);
      assert.equal(cancelled.status,'cancelled');
      assert.deepEqual(cancelled.steps.map(step=>step.status),['skipped','skipped']);
      const saved=JSON.parse(fs.readFileSync(path.join(config,'sprints.json'),'utf8'));
      assert.equal(saved[created.id].status,'cancelled');
    } else {
      const missing=await fetch(origin+sprintRoute,{headers:{Cookie:cookie}});
      assert.equal(missing.status,404);
    }
    assert(!fs.readFileSync(receipts,'utf8').includes('SPRINT_NEXT_STEP_MUST_NOT_RUN'));
  }
  // Re-arm a disposable incomplete record as though a gateway exited during
  // the held step. A restart must reuse the conversation and transcript.
  const savedPath=path.join(config,'sprints.json');
  const savedSprintState=JSON.parse(fs.readFileSync(savedPath,'utf8'));
  const resumed=Object.values(savedSprintState).find(s=>s.title==='Stop proof cancel');
  const resumedConv=resumed.steps[0].conversation_id;
  const overlayFile=path.join(config,'threads',resumedConv.replaceAll(':','_')+'.json');
  const beforeRestart=JSON.parse(fs.readFileSync(overlayFile,'utf8'));
  const runsBefore=fs.readFileSync(receipts,'utf8').trim().split('\n').map(JSON.parse).length;
  child.kill('SIGTERM');await new Promise(resolve=>child.once('exit',resolve));
  resumed.status='running';resumed.steps[0].status='running';resumed.steps[1].status='pending';
  fs.writeFileSync(savedPath,JSON.stringify(savedSprintState));
  child=startGateway();
  let resumedRun;
  for(let n=0;;n++) {
    try {
      const now=await request('/api/sprints/'+resumed.id);
      const runs=fs.readFileSync(receipts,'utf8').trim().split('\n').map(JSON.parse);
      resumedRun=runs.slice(runsBefore).find(r=>r.kind==='opencode'&&r.args.at(-1)?.includes('wait-for-stop cancel'));
      if(now.steps[0].conversation_id===resumedConv&&resumedRun)break;
    }catch{}
    assert(n<100,'gateway did not resume the held sprint');await sleep(30);
  }
  assert(beforeRestart.native_session,'the fixture run must have a native session to resume');
  assert(resumedRun.args.includes('--session'));
  assert.equal(resumedRun.sid,beforeRestart.native_session);
  const afterRestart=JSON.parse(fs.readFileSync(overlayFile,'utf8'));
  assert.equal(afterRestart.created_at,beforeRestart.created_at);
  assert(beforeRestart.appended_messages.every(m=>afterRestart.appended_messages.some(later=>later.id===m.id)));
  await request('/api/sprints/'+resumed.id+'/cancel',{});
  for(let n=0;fs.existsSync('/proc/'+resumedRun.pid);n++){assert(n<100,'resumed sprint did not stop');await sleep(20)}
  console.log('sprint proof: cancel/delete kill actual processes; cancellation persists; later steps never run; restart preserves conversation, transcript and native resume ID');

});
