const {test}=require('node:test');
const assert=require('node:assert/strict');
const {createAdapter,plain,outcome}=require('../../web/rust-loader.js');

function fixture() {
  const clients=[];
  class WaddleConfig {constructor(...args){this.args=args;}}
  class WaddleClient {
    constructor(config){this.config=config;this.callbacks={};this.calls=[];this.freed=0;this.closedProof=new Promise(resolve=>this.proveClosed=resolve);clients.push(this);}
    async connect(){assert.ok(this.callbacks.message);this.connected=true;}
    async disconnect(){this.disconnected=true;if(!this.holdDisconnect)this.callbacks.disconnected?.();}
    async disconnect_and_wait(){await this.disconnect();await this.closedProof;}
    free(){this.freed++;}
    async fetch_room_history_by_thread(...args){this.calls.push(['thread',args]);return {messages:[],complete:true};}
    async status_preference_fetch(){return {mode:'manual',status:'away'};}
    async flutter_rpc(...args){this.calls.push(['native-rpc',args]);}
    async admin_channels_kick(...args){this.calls.push(['kick',args]);}
    async admin_channels_delete(...args){this.calls.push(['delete',args]);}
    async publish_vcard4(...args){this.calls.push(['vcard',args]);}
    async publish_mds_displayed(...args){this.calls.push(['mds',args]);}
    async send_reaction(...args){this.calls.push(['reaction',args]);}
    async request_upload_slot(...args){this.calls.push(['upload',args]);return {get_url:'https://files.test/get'};}
    async send_chat_message(...args){this.calls.push(['chat',args]);return {kind:'sent',stanza_id:'sent-id'};}
    async send_groupchat_message(...args){this.calls.push(['group',args]);return new Promise(resolve=>this.completeSend=resolve);}
  }
  for(const [setter,type] of Object.entries({set_on_message:'message',set_on_presence:'presence',set_on_connected:'connected',
    set_on_session_lifecycle:'sessionLifecycle',set_on_call:'call',set_on_disconnected:'disconnected'})) {
    WaddleClient.prototype[setter]=function(callback){this.callbacks[type]=data=>{if(type==='disconnected')this.proveClosed();callback(data);};};
  }
  class WaddleFlutterTimeline {execute(request){return {messages:[],request};}free(){}}
  return {clients,module:{WaddleConfig,WaddleClient,WaddleFlutterTimeline,default:async()=>{}}};
}
const config={server_url:'wss://xmpp.test',jid:'alice@example.test',access_token:'test-token',resource:'flutter-1'};

test('registers callbacks before startup and fences old clients and late results',async()=>{
  const f=fixture(),events=[];const adapter=await createAdapter(event=>events.push(event),async()=>f.module);
  await adapter.rpc('connect',config);
  const first=f.clients[0];first.callbacks.sessionLifecycle('fresh');
  assert.deepEqual(events[0],{type:'sessionLifecycle',data:{mode:'fresh'}});
  const oldSend=adapter.rpc('send_groupchat_message',{room_jid:'room@muc.test',body:'hello',options:{}});
  await adapter.rpc('connect',config);first.callbacks.message({body:'stale account'});first.completeSend('old-id');
  await assert.rejects(oldSend,/session changed/);assert.equal(events.length,1);assert.equal(first.freed,1);
  f.clients[1].callbacks.message({body:'current'});assert.equal(events.at(-1).data.body,'current');
  await adapter.close();f.clients[1].callbacks.message({body:'closed'});assert.equal(events.length,2);assert.equal(f.clients[1].freed,1);
});

test('adapts native named parameters to existing WASM signatures and preserves IDs',async()=>{
  const f=fixture();const adapter=await createAdapter(()=>{},async()=>f.module);await adapter.rpc('connect',config);
  await adapter.rpc('send_reaction',{target_jid:'room@muc.test',target_stanza_id:'m1',emojis:['👍'],audience:'muc'});
  assert.deepEqual(f.clients[0].calls[0],['reaction',['room@muc.test','groupchat','m1',['👍'],null,null]]);
  await adapter.rpc('request_upload_slot',{service_jid:'upload.test',filename:'f.bin',size:123,content_type:'application/octet-stream'});
  assert.equal(f.clients[0].calls[1][1][2],123n);
  assert.equal(await adapter.rpc('send_reaction',{target_jid:'room@muc.test',target_stanza_id:'m2',emojis:[],audience:'muc'}),true);
  assert.deepEqual(await adapter.rpc('send_chat_message',{peer_jid:'bob@test',body:'hi',options:{}}),{type:'sent',stanza_id:'sent-id'});
  await adapter.close();
});

test('shared timeline runs before connection and raw XML is refused',async()=>{
  const f=fixture();const adapter=await createAdapter(()=>{},async()=>f.module);
  const result=await adapter.rpc('timeline_snapshot',{conversation_jid:'bob@test'});assert.deepEqual(result.messages,[]);
  assert.equal(f.clients.length,0);await assert.rejects(adapter.rpc('send_raw_iq',{xml:'<iq/>'}),/Raw XML/);
  await adapter.close();
});

test('normalizes nested JS Map, byte arrays and known camel fields without changing JID keys',()=>{
  const value=plain({serviceType:'turn',data:new Uint8Array([1,2]),owners:new Map([['Alice@Host/Phone',{tieBreak:true}]])});
  assert.deepEqual(value,{service_type:'turn',data:[1,2],owners:{'Alice@Host/Phone':{tie_break:true}}});
});

test('normalizes real Rust enum vocabulary without renaming raw call kind',()=>{
  assert.deepEqual(outcome('send_chat_message',{kind:'sent',stanza_id:'s1'}),{type:'sent',stanza_id:'s1'});
  assert.deepEqual(outcome('send_correction',{kind:'invalid-options'}),{type:'invalid_options'});
  assert.deepEqual(outcome('set_room_notification_mode',{kind:'node-config-mismatch'}),{type:'node_config_mismatch'});
  assert.deepEqual(outcome('set_dm_notification_mode',{kind:'ok',item:{notifyMode:'always',richPayloadOptIn:true}}),{type:'ok',item:{notify_mode:'always',rich_payload_opt_in:true}});
  assert.equal(outcome('send_call_session_terminate_with_outcome',{kind:'orphaned'}),'orphaned');
  assert.equal(outcome('send_reaction',undefined),true);
  assert.deepEqual(plain({kind:'session-accept',sid:'s1'}),{kind:'session-accept',sid:'s1'});
});


test('close retains ownership until confirmed disconnection and retries after missing proof',async()=>{
  const f=fixture(),events=[];const adapter=await createAdapter(event=>events.push(event),async()=>f.module,{closeTimeout:20});
  await adapter.rpc('connect',config);
  const client=f.clients[0];client.holdDisconnect=true;
  await assert.rejects(adapter.close(),/not finished closing/);
  assert.equal(client.freed,0);
  await assert.rejects(adapter.rpc('connect',config),/closed/);
  client.callbacks.message({body:'retired'});assert.equal(events.length,0);
  const retry=adapter.close();
  client.callbacks.disconnected();
  await retry;assert.equal(client.freed,1);assert.equal(events.length,0);
  await adapter.close();assert.equal(client.freed,1);
});

test('replacement waits for prior driver and close joins in-flight startup',async()=>{
  const f=fixture();const adapter=await createAdapter(()=>{},async()=>f.module);
  await adapter.rpc('connect',config);
  const first=f.clients[0];first.holdDisconnect=true;
  let replaced=false;
  const replacing=adapter.rpc('connect',config).then(()=>{replaced=true;});
  await new Promise(resolve=>setTimeout(resolve,0));
  assert.equal(replaced,false);assert.equal(f.clients.length,1);assert.equal(first.freed,0);
  await assert.rejects(adapter.rpc('send_chat_message',{peer_jid:'bob@test',body:'new account'}),/connection is changing/);
  first.callbacks.disconnected();await replacing;assert.equal(f.clients.length,2);
  const second=f.clients[1];let start;
  f.module.WaddleClient.prototype.connect=()=>new Promise(resolve=>{start=resolve;});
  const starting=adapter.rpc('connect',config);const failed=assert.rejects(starting,/session changed/);
  await new Promise(resolve=>setTimeout(resolve,0));
  const closing=adapter.close();assert.equal(f.clients[2].freed,0);
  start();await failed;await closing;
  assert.equal(second.freed,1);assert.equal(f.clients[2].freed,1);
});


test('adapts profile, administrative records and read-cursor field aliases',async()=>{
 const f=fixture();const adapter=await createAdapter(()=>{},async()=>f.module);await adapter.rpc('connect',config);
 assert.deepEqual(await adapter.rpc('fetch_status_preference'),{type:'manual',status:'away'});
 await adapter.rpc('fetch_room_thread_history',{room_jid:'room@muc.test',thread_id:'thread1',max_messages:50,before_id:'cursor'});
 assert.deepEqual(f.clients[0].calls.at(-1),['thread',['room@muc.test','thread1',50,'cursor']]);
 await adapter.rpc('publish_vcard4',{vcard:{full_name:'Alice Example',nickname:'alice'}});
 assert.deepEqual(f.clients[0].calls.at(-1),['vcard',[{fn:'Alice Example',nickname:'alice'}]]);
 await adapter.rpc('publish_mds_displayed',{chat_jid:'bob@example.test',stanza_id:'read1',stanza_id_by:'example.test'});
 assert.deepEqual(f.clients[0].calls.at(-1),['mds',['bob@example.test','read1','example.test']]);
 await adapter.rpc('admin_channels_kick',{channel_jid:'room@muc.test',occupant_jid:'room@muc.test/nick',reason:'reason'});
 assert.deepEqual(f.clients[0].calls.at(-1),['kick',[{channel_jid:'room@muc.test',occupant_jid:'room@muc.test/nick',reason:'reason'}]]);
 await adapter.rpc('set_room_affiliation',{room_jid:'room@muc.test',target_jid:'bob@example.test',affiliation:'member',reason:'Welcome'});
 assert.deepEqual(f.clients[0].calls.at(-1),['native-rpc',[{method:'set_room_affiliation',args:{room_jid:'room@muc.test',target_jid:'bob@example.test',affiliation:'member',reason:'Welcome'}}]]);
 await adapter.rpc('admin_channels_delete',{channel_jid:'room@muc.test'});
 assert.deepEqual(f.clients[0].calls.at(-1),['delete',[{channel_jid:'room@muc.test',confirm:'yes'}]]);
 assert.deepEqual(outcome('fetch_vcard4',{fn:'Alice',pronouns:'she/her'}),{full_name:'Alice',pronouns:'she/her'});
 assert.deepEqual(outcome('fetch_user_pep_profile',{tune:{length:180,title:'Song'}}),{tune:{length_seconds:180,title:'Song'}});
 await adapter.close();
});


test('typed disconnection cannot replace the physical-close proof',async()=>{
 const f=fixture();const adapter=await createAdapter(()=>{},async()=>f.module,{closeTimeout:20});await adapter.rpc('connect',config);
 const client=f.clients[0];let release;
 const physicalClose=new Promise(resolve=>{release=resolve;});
 client.disconnect_and_wait=async()=>{await client.disconnect();await physicalClose;};
 await assert.rejects(adapter.close(),/not finished closing/);assert.equal(client.freed,0);
 release();await adapter.close();assert.equal(client.freed,1);
 const g=fixture();const cancelled=await createAdapter(()=>{},async()=>g.module);await cancelled.rpc('connect',config);
 // Rust can cancel startup before opening a socket, yielding proof without a disconnected callback.
 g.clients[0].disconnect_and_wait=async()=>{};await cancelled.close();assert.equal(g.clients[0].freed,1);
});
