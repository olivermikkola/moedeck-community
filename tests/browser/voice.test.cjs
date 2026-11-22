const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),os=require('node:os'),net=require('node:net');
const {spawn}=require('node:child_process');
const {chromium,webkit}=require('playwright');
const binary=process.env.MOEDECK_BIN||path.resolve(__dirname,'../../target/debug/moedeck');
const artifacts=path.join(__dirname,'artifacts');fs.mkdirSync(artifacts,{recursive:true});
const sleep=ms=>new Promise(resolve=>setTimeout(resolve,ms));
for(const engine of (process.env.MOEDECK_BROWSERS||'chromium,webkit').split(',')) {
 test(`${engine}: dictation stays in its draft and has real lifecycle states`,{timeout:60000},async t=>{
  const root=fs.mkdtempSync(path.join(os.tmpdir(),'moedeck-voice-'));
  const home=path.join(root,'home'),config=path.join(root,'config'),bin=path.join(home,'.local/bin'),project=path.join(home,'Projects/proof');
  for(const p of [config,bin,project])fs.mkdirSync(p,{recursive:true});
  fs.symlinkSync(process.execPath,path.join(bin,'node'));
  const unexpected=path.join(root,'unexpected-model-spawn');
  fs.writeFileSync(path.join(bin,'codex'),`#!/usr/bin/env node\nif(process.argv.includes('--version')){console.log('fixture 1');process.exit(0)}require('node:fs').writeFileSync(process.env.UNEXPECTED_MODEL_SPAWN,'unexpected');process.exit(1);\n`,{mode:0o755});
  const socket=net.createServer();await new Promise(r=>socket.listen(0,'127.0.0.1',r));const port=socket.address().port;await new Promise(r=>socket.close(r));
  const origin=`http://127.0.0.1:${port}`;
  const gateway=spawn(binary,[],{env:{...process.env,HOME:home,CODEX_HOME:path.join(home,'.codex'),MOEDECK_CONFIG_DIR:config,MOEDECK_PORT:String(port),MOEDECK_PASSCODE:'voice-proof',UNEXPECTED_MODEL_SPAWN:unexpected},stdio:'ignore'});
  let browser;
  t.after(async()=>{await browser?.close();if(gateway.exitCode===null&&gateway.signalCode===null){gateway.kill('SIGTERM');await new Promise(r=>gateway.once('exit',r))}console.log(`${engine} dictation evidence: ${root}`)});
  for(let n=0;;n++){try{if((await fetch(origin+'/api/health')).ok)break}catch{}assert(n<100);await sleep(50)}
  const launch={headless:true};
  if(engine==='webkit'&&process.env.MOEDECK_WEBKIT_LIBS){const base=path.join(path.dirname(webkit.executablePath()),'minibrowser-wpe');launch.executablePath=path.join(base,'bin/MiniBrowser');launch.env={...process.env,LD_LIBRARY_PATH:`${process.env.MOEDECK_WEBKIT_LIBS}:${base}/lib:${base}/sys/lib`,WEBKIT_EXEC_PATH:`${base}/bin`,WEBKIT_INJECTED_BUNDLE_PATH:`${base}/lib`,WEBKIT_INSPECTOR_RESOURCES_PATH:`${base}/share`}}
  browser=await({chromium,webkit}[engine]).launch(launch);
  const context=await browser.newContext({viewport:{width:393,height:852},isMobile:true,hasTouch:true,serviceWorkers:'block'});
  await context.addInitScript(()=>{
   localStorage.setItem('helm_walkie','true'); // An old preference must not auto-send.
   window.voiceFixture={instances:[],behavior:'delayed'};
   class Recognition {
    constructor(){window.voiceFixture.instances.push(this);this.aborts=0;this.stops=0}
    start(){if(window.voiceFixture.behavior==='throw')throw Error('fixture start failure')}
    stop(){this.stops++}
    abort(){this.aborts++;queueMicrotask(()=>this.onend?.())}
   }
   window.SpeechRecognition=Recognition;window.webkitSpeechRecognition=undefined;
  });
  const page=await context.newPage(),errors=[],mutations=[];
  page.on('pageerror',e=>errors.push(e.message));
  page.on('request',r=>{if(r.method()!=='GET'&&r.url().includes('/api/')&&!r.url().endsWith('/auth/login'))mutations.push(r.url())});
  await page.goto(origin);await page.locator('#loginPass').fill('voice-proof');await page.locator('#loginForm button').tap();
  await page.waitForFunction(()=>state.bootstrappedOnce&&!document.querySelector('#composerInput').disabled);
  await page.evaluate(project=>{state.selectedWorkdir=project},project);
  const input=page.locator('#composerInput');await input.fill('Existing draft.');
  await page.getByRole('button',{name:'Dictate message',exact:true}).tap();
  assert.equal(await page.locator('#voiceWaveStatus').textContent(),'Starting microphone…');
  assert.equal(await page.locator('#voiceLevelMeter').count(),0,'there is no simulated sound-level meter');
  await page.evaluate(()=>voiceFixture.instances.at(-1).onstart());
  assert.equal(await page.locator('#voiceWaveStatus').textContent(),'Listening…');
  await page.screenshot({path:path.join(artifacts,`${engine}-dictation-listening.png`)});
  await page.getByRole('button',{name:'Stop dictation',exact:true}).tap();
  assert.equal(await page.locator('#voiceWaveStatus').textContent(),'Finishing dictation…');
  assert.equal(await page.locator('#micBtn').isDisabled(),true);
  await page.evaluate(()=>{const r=voiceFixture.instances.at(-1);r.onresult({results:[[{transcript:"Don't fix CI; stop and review diff later."}]]});r.onend()});
  assert.equal(await input.inputValue(),"Existing draft. Don't fix CI; stop and review diff later.");
  assert.deepEqual(mutations,[],'dictation must never call command, message or classification routes');
  assert.equal(fs.existsSync(unexpected),false);
  assert.equal(await page.locator('#micBtn').isDisabled(),false);
  await page.screenshot({path:path.join(artifacts,`${engine}-dictation-draft.png`)});
  // Cancel before a permission/start event, then deliver stale callbacks.
  await page.getByRole('button',{name:'Dictate message',exact:true}).tap();
  await page.getByRole('button',{name:'Cancel dictation',exact:true}).tap();
  const draft=await input.inputValue();
  await page.evaluate(()=>{const r=voiceFixture.instances.at(-1);r.onstart();r.onresult({results:[[{transcript:'STALE_CANCELLED_RESULT'}]]})});
  assert.equal(await input.inputValue(),draft);
  assert.equal(await page.locator('#voiceWaveBar').isVisible(),false);
  // Changing conversations/drafts aborts the old capture and ignores its result.
  await page.getByRole('button',{name:'Dictate message',exact:true}).tap();
  await page.evaluate(()=>startNewChat());
  await page.evaluate(()=>voiceFixture.instances.at(-1).onresult({results:[[{transcript:'WRONG_DRAFT_RESULT'}]]}));
  assert(!((await input.inputValue()).includes('WRONG_DRAFT_RESULT')));
  // Engine errors and synchronous start failures return the mic to a usable state.
  await page.getByRole('button',{name:'Dictate message',exact:true}).tap();
  await page.evaluate(()=>voiceFixture.instances.at(-1).onerror({error:'not-allowed'}));
  assert.match(await page.locator('#helmToast').textContent(),/permission was denied/);
  assert.equal(await page.locator('#voiceWaveBar').isVisible(),false);
  await page.evaluate(()=>voiceFixture.behavior='throw');
  await page.getByRole('button',{name:'Dictate message',exact:true}).tap();
  assert.match(await page.locator('#helmToast').textContent(),/Could not start dictation/);
  assert.equal(await page.locator('#micBtn').isDisabled(),false);
  // Drive the actual timeout callback without a minute of wall-clock waiting.
  await page.clock.install();await page.evaluate(()=>voiceFixture.behavior='delayed');
  await page.getByRole('button',{name:'Dictate message',exact:true}).tap();
  await page.clock.fastForward(60001);
  assert.equal(await page.locator('#voiceWaveBar').isVisible(),false);
  assert.match(await page.locator('#helmToast').textContent(),/timed out/);
  assert.equal(await page.evaluate(()=>voiceFixture.instances.at(-1).aborts>0),true);
  assert.deepEqual(mutations,[]);assert.equal(fs.existsSync(unexpected),false);assert.deepEqual(errors,[]);
  // Sending the reviewed draft ends capture, even if the request is rejected.
  await page.clock.resume();
  await page.evaluate(project=>{state.selectedWorkdir=project},project);
  await page.route('**/api/conversations',route=>route.request().method()==='POST'
   ? route.fulfill({status:403,contentType:'application/json',body:JSON.stringify({error:'fixture send rejection'})}) : route.continue());
  await input.fill('Reviewed draft');
  await page.getByRole('button',{name:'Dictate message',exact:true}).tap();
  await page.evaluate(()=>sendMessage());
  assert.equal(await page.evaluate(()=>voiceFixture.instances.at(-1).aborts>0),true);
  await page.evaluate(()=>voiceFixture.instances.at(-1).onresult({results:[[{transcript:'LATE_AFTER_SEND'}]]}));
  assert(!((await input.inputValue()).includes('LATE_AFTER_SEND')));
  assert.equal(fs.existsSync(unexpected),false);
  // The server also treats spoken command-like text as text, and failures as failures.
  for(const text of ['stop',"Don't fix CI",'bounce to codex later']){
   const response=await context.request.post(origin+'/api/voice/transcribe',{headers:{Origin:origin},data:{speech_text:text}});
   const result=await response.json();assert.equal(result.ok,true);assert.deepEqual(result.intent,{action:'chat_prompt',payload:text});assert.equal(result.transcript,text);
  }
  const failed=await(await context.request.post(origin+'/api/voice/transcribe',{headers:{Origin:origin},data:{audio_base64:'invalid-base64!',format:'wav'}})).json();
  assert.equal(failed.ok,false);assert(failed.error);assert.equal(failed.transcript,'');
  const unsupported=await browser.newContext({viewport:{width:393,height:852},serviceWorkers:'block'});
  await unsupported.addCookies(await context.cookies());
  await unsupported.addInitScript(()=>{window.SpeechRecognition=undefined;window.webkitSpeechRecognition=undefined});
  const unavailable=await unsupported.newPage();await unavailable.goto(origin);await unavailable.waitForFunction(()=>state.bootstrappedOnce);
  assert.equal(await unavailable.locator('#micBtn').isVisible(),false,'unavailable dictation is not advertised');await unsupported.close();
  console.log(`${engine}: editable dictation; no inferred actions or auto-send; genuine start/stop/cancel/error/timeout states; stale callbacks ignored; unavailable mic hidden; API failures explicit`);
 });
}
