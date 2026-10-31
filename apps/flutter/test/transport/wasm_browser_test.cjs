// Actual Chromium + actual Rust/WASM + a local XMPP fixture. No production service.
const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const os=require('node:os');
const root=path.resolve(__dirname,'../../web');
const browserBinary=process.env.WADDLE_BROWSER_BINARY ?? ['chromium','chromium-browser','google-chrome','google-chrome-stable'].map(name=>Bun.which(name)).find(Boolean);
const available=browserBinary!=null&&fs.existsSync(path.join(root,'rust/waddle_xmpp_client_wasm_bg.wasm'));
const required=process.env.WADDLE_REQUIRE_BROWSER_CONTRACT==='1';
const delay=ms=>new Promise(resolve=>setTimeout(resolve,ms));

async function chromiumPage(url) {
  const directory=fs.mkdtempSync(path.join(os.tmpdir(),'waddle-wasm-browser-'));
  const child=Bun.spawn([browserBinary,'--headless','--no-sandbox','--disable-gpu','--disable-dev-shm-usage',
    '--no-first-run','--remote-debugging-port=0',`--user-data-dir=${directory}`,'about:blank'],{stdout:'ignore',stderr:'ignore'});
  const deadline=Date.now()+15000;
  let socket;
  try {
    const portFile=path.join(directory,'DevToolsActivePort');
    while(!fs.existsSync(portFile)){if(Date.now()>deadline)throw new Error('Chromium did not start');await delay(30);}
    const [port,browserPath]=fs.readFileSync(portFile,'utf8').trim().split('\n');
    socket=new WebSocket(`ws://127.0.0.1:${port}${browserPath}`);
    await new Promise((resolve,reject)=>{socket.onopen=resolve;socket.onerror=reject;});
    let id=0;const pending=new Map();
    socket.onmessage=event=>{const value=JSON.parse(event.data);if(!value.id)return;const request=pending.get(value.id);pending.delete(value.id);
      if(value.error)request.reject(new Error(value.error.message));else request.resolve(value.result);};
    const send=(method,params={},sessionId)=>new Promise((resolve,reject)=>{const requestId=++id;pending.set(requestId,{resolve,reject});socket.send(JSON.stringify({id:requestId,method,params,...(sessionId?{sessionId}:{})}));});
    const {targetId}=await send('Target.createTarget',{url:'about:blank'});
    const {sessionId}=await send('Target.attachToTarget',{targetId,flatten:true});
    await send('Runtime.enable',{},sessionId);await send('Page.navigate',{url},sessionId);
    while(Date.now()<deadline){
      const result=await send('Runtime.evaluate',{expression:'globalThis.__contractResult ?? null',returnByValue:true},sessionId);
      if(result.result?.value!=null)return result.result.value;
      await delay(30);
    }
    throw new Error('The browser protocol contract timed out');
  } finally {socket?.close();child.kill();await child.exited;fs.rmSync(directory,{recursive:true,force:true});}
}

function html(endpoint,authenticationFailure=false) {
 return `<!doctype html><html><body>Testing actual WASM…<script src="/rust-loader.js"></script><script type="module">
 const seen=[];
 const wait=(type)=>new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(Error('Missing '+type)),10000);const poll=setInterval(()=>{const event=seen.find(e=>e.type===type);if(event){clearTimeout(timer);clearInterval(poll);resolve(event)}},10);});
 try {
   const transport=await waddleRust.create(event=>seen.push(event));
   await transport.rpc('connect',{server_url:'${endpoint}',jid:'alice@example.test',access_token:'test-token',resource:'flutter-fixture',resumable:false});
   if (${authenticationFailure}) {
     const failure=await wait('authenticationFailed');
     if(failure.data.condition!=='not-authorized')throw Error('Incorrect typed SASL condition');
     await transport.close();
     globalThis.__contractResult={passed:true,condition:failure.data.condition};
   }else{
   await wait('connected');
   await transport.rpc('publish_vcard4',{vcard:{full_name:'Flutter Alice',nickname:'alice'}});
   await transport.rpc('publish_mds_displayed',{chat_jid:'bob@example.test',stanza_id:'read-fixture',stanza_id_by:'example.test'});
   const options={stanza_id:'browser-outgoing',request_displayed_marker:true,muc_pm:false,markup_spans:[],references:[],shared_files:[]};
   const sent=await transport.rpc('send_chat_message',{peer_jid:'bob@example.test',body:'Browser fixture message',options});
   if(sent.type!=='sent'||sent.stanza_id!=='browser-outgoing')throw Error('Incorrect sent outcome '+JSON.stringify(sent));
   await wait('deliveryAcked');
   const message=await wait('message');
   if(message.data.body!=='Hello Flutter browser')throw Error('Incorrect incoming message');
   const wrong=await transport.rpc('send_chat_message',{peer_jid:'not a jid',body:'x',options});
   if(wrong.type!=='invalid_recipient')throw Error('Incorrect invalid outcome');
   await transport.close();
   globalThis.__contractResult={passed:true,sent,events:seen.map(e=>e.type)};
   }
 }catch(error){globalThis.__contractResult={passed:false,error:String(error)};}
 </script></body></html>`;
}

test('actual Chromium/WASM authenticates, binds without resume, sends and receives with native DTOs',{
 skip:!available&&!required?'Build WASM and install Chromium for this wire contract.':false,timeout:30000,
},async()=>{
 assert.ok(available,'The browser wire contract requires a built WASM artifact and Chrome/Chromium.');
 let outbound,smResume,openCount=0,handled=0;const published=[];
 const server=Bun.serve({hostname:'127.0.0.1',port:0,
   fetch(request,server){
    const url=new URL(request.url);
    if(url.pathname==='/xmpp'||url.pathname==='/xmpp-auth') {if(server.upgrade(request,{data:{openCount:0,reject:url.pathname==='/xmpp-auth'},headers:{'Sec-WebSocket-Protocol':'xmpp'}}))return;return new Response('Upgrade failed',{status:400});}
    if(url.pathname==='/contract-auth')return new Response(html(`ws://127.0.0.1:${server.port}/xmpp-auth`,true),{headers:{'Content-Type':'text/html'}});
    if(url.pathname==='/contract')return new Response(html(`ws://127.0.0.1:${server.port}/xmpp`),{headers:{'Content-Type':'text/html'}});
    const pathname=path.join(root,url.pathname.slice(1));
    if(!pathname.startsWith(root+path.sep)||!fs.existsSync(pathname))return new Response('Not found',{status:404});
    return new Response(Bun.file(pathname),{headers:{'Content-Type':pathname.endsWith('.wasm')?'application/wasm':'application/javascript'}});
   },
   websocket:{
    message(socket,raw){const message=typeof raw==='string'?raw:new TextDecoder().decode(raw);
      if(message.startsWith('<open')){
        socket.data.openCount++;openCount=socket.data.openCount;socket.send("<open xmlns='urn:ietf:params:xml:ns:xmpp-framing' from='example.test' id='fixture' version='1.0'/>");
        socket.send(openCount===1?"<stream:features xmlns:stream='http://etherx.jabber.org/streams'><mechanisms xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><mechanism>OAUTHBEARER</mechanism></mechanisms></stream:features>":"<stream:features xmlns:stream='http://etherx.jabber.org/streams'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/><sm xmlns='urn:xmpp:sm:3'/></stream:features>");
      }else if(message.startsWith('<auth'))socket.send(socket.data.reject ? "<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>" : "<success xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>");
      else if(message.startsWith('<iq')&&message.includes('xmpp-bind')){
        const id=message.match(/\bid=['"]([^'"]+)/)[1];
        socket.send(`<iq xmlns='jabber:client' type='result' id='${id}'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><jid>alice@example.test/flutter-fixture</jid></bind></iq>`);
      }else if(message.startsWith('<enable')){
        smResume=/\bresume=['"]true['"]/.test(message);socket.send("<enabled xmlns='urn:xmpp:sm:3'/>");
        socket.send("<message xmlns='jabber:client' from='bob@example.test/phone' to='alice@example.test/flutter-fixture' type='chat' id='browser-incoming'><body>Hello Flutter browser</body></message>");
      }else if(message.startsWith('<message')){handled++;outbound=message;socket.send(`<a xmlns='urn:xmpp:sm:3' h='${handled}'/>`);}
      else if(message.startsWith('<r'))socket.send(`<a xmlns='urn:xmpp:sm:3' h='${handled}'/>`);
      else if(message.startsWith('<iq')){handled++;published.push(message);const id=message.match(/\bid=['"]([^'"]+)/)[1];socket.send(`<iq xmlns='jabber:client' type='result' id='${id}'/>`);}
      else if(message.startsWith('<close'))socket.send("<close xmlns='urn:ietf:params:xml:ns:xmpp-framing'/>");
    },
   },
 });
 try {
   const result=await chromiumPage(`http://127.0.0.1:${server.port}/contract`);
   assert.equal(result.passed,true,result.error);assert.equal(smResume,false);assert.ok(outbound.includes('Browser fixture message'));
   assert.ok(published.some(iq=>iq.includes('Flutter Alice')));assert.ok(published.some(iq=>iq.includes('read-fixture')));
   assert.ok(result.events.includes('deliveryAcked'));assert.ok(result.events.includes('message'));
   const denied=await chromiumPage(`http://127.0.0.1:${server.port}/contract-auth`);
   assert.equal(denied.passed,true,denied.error);assert.equal(denied.condition,'not-authorized');
 }finally{server.stop(true);}
});

// A raw WebSocket fixture delays the close-control reply; Bun's upgrade server
// acknowledges that control frame automatically and cannot model a slow close.
function delayedWebSocketFixture({authenticationFailure=false,protocolError=false}={}) {
 const {createHash}=require('node:crypto');
 const held=new Set(),active=new Set(),timers=new Set();let overlap=false,connections=0;
 const frame=(opcode,payload)=>{
  const data=Buffer.from(payload),header=data.length<126?Buffer.from([0x80|opcode,data.length]):Buffer.from([0x80|opcode,126,data.length>>8,data.length&255]);
  return Buffer.concat([header,data]);
 };
 const server=Bun.listen({hostname:'127.0.0.1',port:0,socket:{
  open(socket){socket.data={buffer:Buffer.alloc(0),upgraded:false,opens:0};},
  data(socket,chunk){
   const state=socket.data;state.buffer=Buffer.concat([state.buffer,Buffer.from(chunk)]);
   if(!state.upgraded){
    const end=state.buffer.indexOf('\r\n\r\n');if(end<0)return;
    const request=state.buffer.subarray(0,end).toString(),key=request.match(/sec-websocket-key:\s*([^\r\n]+)/i)?.[1];
    assert.ok(key);if(active.size)overlap=true;active.add(socket);connections++;
    const accept=createHash('sha1').update(key+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64'); // NOSONAR: RFC 6455 requires SHA-1 for Sec-WebSocket-Accept; this localhost handshake does not hash passwords or sensitive data.
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\nSec-WebSocket-Protocol: xmpp\r\n\r\n`);
    state.upgraded=true;state.buffer=state.buffer.subarray(end+4);
   }
   while(state.buffer.length>=2){
    const opcode=state.buffer[0]&15,masked=(state.buffer[1]&128)!==0;let size=state.buffer[1]&127,offset=2;
    if(size===126){if(state.buffer.length<4)return;size=state.buffer.readUInt16BE(2);offset=4;}
    if(size===127)throw Error('The fixture only accepts small XMPP frames');
    const full=offset+(masked?4:0)+size;if(state.buffer.length<full)return;
    const mask=masked?state.buffer.subarray(offset,offset+4):null;offset+=masked?4:0;
    const payload=Buffer.from(state.buffer.subarray(offset,offset+size));if(mask)for(let i=0;i<payload.length;i++)payload[i]^=mask[i%4];
    state.buffer=state.buffer.subarray(full);
    if(opcode===8){
     held.add(socket);
     const timer=setTimeout(()=>{timers.delete(timer);held.delete(socket);active.delete(socket);socket.end(frame(8,payload));},250);timers.add(timer);continue;
    }
    if(opcode!==1)continue;
    const message=payload.toString(),send=value=>socket.write(frame(1,value));
    if(message.startsWith('<open')){
     state.opens++;send("<open xmlns='urn:ietf:params:xml:ns:xmpp-framing' from='example.test' id='delayed' version='1.0'/>");
     send(state.opens===1?"<stream:features xmlns:stream='http://etherx.jabber.org/streams'><mechanisms xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><mechanism>OAUTHBEARER</mechanism></mechanisms></stream:features>":"<stream:features xmlns:stream='http://etherx.jabber.org/streams'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/><sm xmlns='urn:xmpp:sm:3'/></stream:features>");
    }else if(message.startsWith('<auth'))send(authenticationFailure ? "<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>" : "<success xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>");
    else if(message.startsWith('<iq')&&message.includes('xmpp-bind')){
     const id=message.match(/\bid=['"]([^'"]+)/)[1];send(`<iq xmlns='jabber:client' type='result' id='${id}'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><jid>alice@example.test/flutter-fixture</jid></bind></iq>`);
    }else if(message.startsWith('<enable')){send("<enabled xmlns='urn:xmpp:sm:3'/>");if(protocolError)send("<message><malformed>");}
    else if(message.startsWith('<close'))send("<close xmlns='urn:ietf:params:xml:ns:xmpp-framing'/>");
   }
  },
  close(socket){held.delete(socket);active.delete(socket);},
  error(){},
 }});
 return {port:server.port,get overlap(){return overlap;},get connections(){return connections;},stop(){for(const timer of timers)clearTimeout(timer);server.stop(true);}};
}

for (const mode of ['graceful disconnect','SASL failure','protocol failure']) test(`actual WASM replacement waits for browser CLOSED after ${mode}`,{
 skip:!available&&!required?'Build WASM and install Chromium for the close contract.':false,timeout:30000,
},async()=>{
 assert.ok(available,'The close contract requires the actual WASM and browser.');
 const authenticationFailure=mode==='SASL failure',protocolError=mode==='protocol failure';
 const fixture=delayedWebSocketFixture({authenticationFailure,protocolError});
 const server=Bun.serve({hostname:'127.0.0.1',port:0,fetch(request){
  const url=new URL(request.url);
  if(url.pathname==='/close-contract')return new Response(`<!doctype html><script src='/rust-loader.js'></script><script type='module'>
   const seen=[],config={server_url:'ws://127.0.0.1:${fixture.port}',jid:'alice@example.test',access_token:'fixture',resource:'flutter-fixture',resumable:false};
   const waitReady=count=>new Promise((resolve,reject)=>{const end=Date.now()+10000;const timer=setInterval(()=>{if(seen.filter(e=>e.type==='${authenticationFailure?'authenticationFailed':protocolError?'disconnected':'connected'}').length>=count){clearInterval(timer);resolve();}else if(Date.now()>end){clearInterval(timer);reject(Error('No ready callback'));}},10);});
   try{const transport=await waddleRust.create(event=>seen.push(event));await transport.rpc('connect',config);await waitReady(1);await transport.rpc('connect',config);await waitReady(2);await transport.close();globalThis.__contractResult={passed:true};}
   catch(error){globalThis.__contractResult={passed:false,error:String(error)};}
  </script>`,{headers:{'Content-Type':'text/html'}});
  const pathname=path.join(root,url.pathname.slice(1));if(!pathname.startsWith(root+path.sep)||!fs.existsSync(pathname))return new Response('Not found',{status:404});
  return new Response(Bun.file(pathname),{headers:{'Content-Type':pathname.endsWith('.wasm')?'application/wasm':'application/javascript'}});
 }});
 try{
  const result=await chromiumPage(`http://127.0.0.1:${server.port}/close-contract`);
  assert.equal(result.passed,true,result.error);assert.equal(fixture.connections,2);assert.equal(fixture.overlap,false,'The successor opened while the prior socket close reply was held.');
 }finally{server.stop(true);fixture.stop();}
});


test('actual WASM immediate close cancels admitted startup before a socket can escape',{
 skip:!available&&!required?'Build WASM and install Chromium for the startup contract.':false,timeout:30000,
},async()=>{
 assert.ok(available,'The startup contract requires the actual WASM and browser.');
 const fixture=delayedWebSocketFixture();
 const server=Bun.serve({hostname:'127.0.0.1',port:0,fetch(request){
  const url=new URL(request.url);
  if(url.pathname==='/startup-contract')return new Response(`<!doctype html><script type='module'>
   try{const wasm=await import('/rust/waddle_xmpp_client_wasm.js');await wasm.default();
    const config=new wasm.WaddleConfig('ws://127.0.0.1:${fixture.port}','alice@example.test','fixture','flutter-fixture');config.set_resumable(false);
    const client=new wasm.WaddleClient(config),startup=client.connect(),closing=client.disconnect_and_wait();
    const results=await Promise.allSettled([startup,closing]);
    if(results[0].status!=='rejected'||results[1].status!=='fulfilled')throw Error('Startup or close did not settle safely');
    client.free();globalThis.__contractResult={passed:true};
   }catch(error){globalThis.__contractResult={passed:false,error:String(error)};}
  </script>`,{headers:{'Content-Type':'text/html'}});
  const pathname=path.join(root,url.pathname.slice(1));if(!pathname.startsWith(root+path.sep)||!fs.existsSync(pathname))return new Response('Not found',{status:404});
  return new Response(Bun.file(pathname),{headers:{'Content-Type':pathname.endsWith('.wasm')?'application/wasm':'application/javascript'}});
 }});
 try{const result=await chromiumPage(`http://127.0.0.1:${server.port}/startup-contract`);assert.equal(result.passed,true,result.error);assert.equal(fixture.connections,0);}
 finally{server.stop(true);fixture.stop();}
});
