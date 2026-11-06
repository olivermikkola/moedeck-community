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
const flag=kind==='claude'?'--resume':kind==='agy'?'--conversation':'--session';
const sid=args.includes(flag)?args[args.indexOf(flag)+1]:crypto.randomUUID();
fs.appendFileSync(process.env.MOEDECK_RECEIPTS,JSON.stringify({kind,args,sid,config:process.env.OPENCODE_CONFIG_CONTENT||null})+'\\n');
if(kind==='opencode') {
  if(args.at(-1)==='wait-for-stop') {emit({type:'step_start',sessionID:sid});setTimeout(()=>process.exit(0),10000);return;}
  emit({type:'error',error:{message:'PROVIDER_FAILURE_PROOF'}});process.exit(1);
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

test('harness sessions, OpenCode history, failures and Safe policy', { timeout: 30000 }, async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'moedeck-harness-'));
  const home = path.join(root, 'home'), config = path.join(root, 'config');
  const bin = path.join(home, '.local/bin'), project = path.join(home, 'Projects/proof');
  const data = path.join(home, '.local/share/opencode');
  [bin, project, data, config].forEach(p => fs.mkdirSync(p, { recursive: true }));
  for (const h of ['claude', 'agy', 'opencode']) fs.writeFileSync(path.join(bin, h), cli, { mode: 0o755 });
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
  const child = spawn(binary, [], {env:{...process.env,HOME:home,CODEX_HOME:path.join(home,'.codex'),
    HELM_CONFIG_DIR:config,HELM_PORT:String(port),HELM_PASSCODE:'harness-proof',MOEDECK_RECEIPTS:receipts},stdio:'ignore'});
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
  for (const [sid,prompt] of [['ses_error','New prompt'],['ses_repeat','Repeat this prompt']]) {
    const id='opencode:'+sid;
    await request(route(id)+'/messages',{text:prompt,harness:'opencode',model:'default',effort:'default',workdir:project});
    const d=await finish(id);
    assert(d.messages.at(-1).text.includes('PROVIDER_FAILURE_PROOF'));
    assert.equal(d.messages.filter(m=>m.role==='user'&&m.text===prompt).length,sid==='ses_repeat'?2:1);
  }
  const stoppedId='opencode:ses_error';
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
});
