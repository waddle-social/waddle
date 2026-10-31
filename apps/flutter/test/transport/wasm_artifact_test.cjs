const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const {pathToFileURL}=require('node:url');
const {createAdapter}=require('../../web/rust-loader.js');
const root=path.resolve(__dirname,'../../web/rust');
const available=fs.existsSync(path.join(root,'waddle_xmpp_client_wasm_bg.wasm'));

test('actual WASM DTOs cross the adapter with native-compatible outcomes and encrypted options',{
  skip:!available&&process.env.WADDLE_REQUIRE_WASM_ARTIFACT!=='1'?'Build apps/flutter/web/rust before running the real artifact contract.':false,
},async()=>{
  assert.ok(available,'The actual WASM artifact is required for this contract.');
  const wasm=await import(pathToFileURL(path.join(root,'waddle_xmpp_client_wasm.js')).href);
  await wasm.default({module_or_path:fs.readFileSync(path.join(root,'waddle_xmpp_client_wasm_bg.wasm'))});
  // Suppress socket startup only. Constructors, callbacks, request validation and outcome serialization are real WASM.
  class DisconnectedClient extends wasm.WaddleClient {
    connect(){return Promise.resolve();}
    set_on_disconnected(callback){this.stopped=callback;super.set_on_disconnected(callback);}
    disconnect(){this.stopped?.();return Promise.resolve();}
    disconnect_and_wait(){return this.disconnect();}
  }
  const adapter=await createAdapter(()=>{},async()=>({...wasm,default:async()=>{},WaddleClient:DisconnectedClient}));
  await adapter.rpc('connect',{server_url:'wss://xmpp.example.test',jid:'alice@example.test',access_token:'fixture',resource:'flutter-fixture'});
  const options={stanza_id:'wasm-fixture',request_displayed_marker:true,muc_pm:false,
    shared_files:[{url:'https://files.example.test/cipher',name:'image.png',media_type:'image/png',size:3,
      disposition:'inline',hashes:[{algo:'sha-256',value_b64:'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA='}],encrypted:{cipher:'urn:xmpp:ciphers:aes-256-gcm-nopadding:0',
        key_b64:'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=',iv_b64:'AAAAAAAAAAAAAAAA',hashes:[],sources:['https://files.example.test/cipher']}}],
    markup_spans:[],references:[]};
  assert.deepEqual(await adapter.rpc('send_chat_message',{peer_jid:'bob@example.test',body:'',options}),{type:'not_connected'});
  assert.deepEqual(await adapter.rpc('send_groupchat_message',{room_jid:'room@muc.example.test',body:'hello',options}),{type:'not_connected'});
  assert.deepEqual(await adapter.rpc('send_correction',{peer_jid:'bob@example.test',target_id:'m1',new_body:'edited',audience:'direct',options}),{type:'not_connected'});
  assert.deepEqual(await adapter.rpc('send_chat_message',{peer_jid:'bob@example.test',body:'x',options:{...options,shared_files:[{...options.shared_files[0],encrypted:{...options.shared_files[0].encrypted,cipher:'urn:invalid'}}]}}),{type:'invalid_options'});
  const timeline=await adapter.rpc('timeline_snapshot',{conversation_jid:'bob@example.test'});
  assert.ok(Array.isArray(timeline.items));
  await adapter.close();
});
