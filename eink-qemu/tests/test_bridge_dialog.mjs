import assert from 'node:assert/strict';
import test from 'node:test';
import { bridgeView } from '../desktop/src/bridge-dialog.ts';
import { initialBridgeState } from '../desktop/src/bridge-state.ts';

const operation = { id:'a'.repeat(32), revision:3, state:'ready', stage:'Bridge ready', confirmationDeadline:null, rebootRequired:false, error:null };
test('Ready explains host readiness and the guest network to join', () => {
  const view = bridgeView({ ...initialBridgeState(), step:'ready', operation });
  assert.equal(view.title, 'Bridge ready');
  assert.match(view.message, /PICSimLabWifi/);
  assert.doesNotMatch(view.message, /guest is connected|guest IP|192\.168\./i);
});
test('restoration failure and settings failure offer distinct recovery', () => {
  const restored = bridgeView({ ...initialBridgeState(), step:'failed', error:{code:'EROLLBACK',message:'Restore failed'} });
  assert.equal(restored.title,'Connection needs repair');
  const save = bridgeView({ ...initialBridgeState(), step:'failed', operation, error:{code:'EIO',message:'Save failed'} });
  assert.match(save.message, /setup exists/);
  assert.equal(save.retrySave,true);
});
test('Escape cannot hide an active mutation or restoration', () => {
  for (const state of ['applying','rollingBack','verifying']) {
    const view=bridgeView({ ...initialBridgeState(), step:'progress', operation:{...operation,state} });
    assert.equal(view.canClose,false);
  }
  assert.equal(bridgeView(initialBridgeState()).canClose,true);
});
test('external override blocks setup with an explicit explanation', () => {
  const view=bridgeView({ ...initialBridgeState(), inspection:{externalOverride:'X4NET'} });
  assert.match(view.message,/X4NET/);
});

test('wizard submits Apply once, waits for saved verification, and restores focus', async (t) => {
  const oldDocument=globalThis.document, oldWindow=globalThis.window;
  class Element extends EventTarget {
    constructor(tag) { super(); this.tagName=tag.toUpperCase(); this.children=[]; this.dataset={}; this.attributes={}; this.disabled=false; }
    text='';
    set textContent(value) { this.text=value; this.children=[]; }
    get textContent() { return this.text+this.children.map(e=>e.textContent).join(''); }
    append(...children) { for (const child of children) { child.parent=this; this.children.push(child); } }
    replaceChildren(...children) { for(const c of this.children) c.parent=null; this.children=[]; this.text=''; this.append(...children); }
    setAttribute(name,value) { this.attributes[name]=value; }
    get isConnected() { return this===document.body || !!this.parent?.isConnected; }
    contains(element) { return this===element || this.children.some(e=>e.contains(element)); }
    querySelectorAll(tag) { return this.children.flatMap(e=>[...(e.tagName.toLowerCase()===tag?[e]:[]),...e.querySelectorAll(tag)]); }
    focus() { document.activeElement=this; }
    showModal() { this.open=true; }
    close() { this.open=false; this.dispatchEvent(new Event('close')); }
    remove() { this.parent.children=this.parent.children.filter(e=>e!==this); this.parent=null; }
  }
  const document={createElement:tag=>new Element(tag),body:new Element('body'),activeElement:null};
  globalThis.document=document;
  globalThis.window={setInterval,clearInterval};
  t.after(()=>{globalThis.document=oldDocument;globalThis.window=oldWindow;});
  const trigger=new Element('button'); document.body.append(trigger); trigger.focus();
  let handler, applyCount=0, unwatchCount=0, resolveApply;
  const inspection={support:'ready',platform:'linux',adapters:[{id:'eth0',name:'Ethernet',eligible:true,linkUp:true,fingerprint:'one',kind:'ethernet',reason:null}],capabilities:{create:true,existingBridge:true,reason:null},selection:null,readiness:'needsSetup',activeOperation:null,externalOverride:null};
  const backend={
    watch:async cb=>{handler=cb;return()=>unwatchCount++;},
    inspect:async()=>inspection,
    plan:async request=>({id:operation.id,request,fingerprint:'one',changes:['Create bridge'],mayDisconnect:false,requiresAuthorization:true,expiresAt:Math.floor(Date.now()/1000)+300}),
    apply:async()=>{applyCount++; return await new Promise(resolve=>resolveApply=resolve);},
    verify:async()=>({...inspection,readiness:'ready'}),
  };
  const {openBridgeDialog}=await import('../desktop/src/bridge-dialog.ts');
  const {dialogOpen}=await import('../desktop/src/dialog.ts');
  const finished=openBridgeDialog(backend,trigger);
  const flush=()=>new Promise(resolve=>setImmediate(resolve));
  await flush();
  const dialog=document.body.querySelectorAll('dialog')[0];
  t.after(()=>{if(dialog.open)dialog.close();});
  const click=label=>{const b=dialog.querySelectorAll('button').find(e=>e.textContent===label);assert.ok(b,label);assert.equal(b.disabled,false,label);b.dispatchEvent(new Event('click'));return b;};
  assert.equal(dialogOpen(),true);
  const radio=dialog.querySelectorAll('input')[0]; radio.dispatchEvent(new Event('change'));
  click('Review setup'); await flush();
  const apply=click('Apply setup'); apply.dispatchEvent(new Event('click'));
  assert.equal(applyCount,1);
  const cancel=new Event('cancel',{cancelable:true});dialog.dispatchEvent(cancel);assert.equal(cancel.defaultPrevented,true);
  handler({...operation,state:'applying',revision:1});
  resolveApply(operation); await flush();
  assert.match(dialog.textContent,/PICSimLabWifi/);
  click('Done'); await finished;
  assert.equal(dialogOpen(),false);assert.equal(unwatchCount,1);assert.equal(document.activeElement,trigger);
});
