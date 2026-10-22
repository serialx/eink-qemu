import assert from 'node:assert/strict';
import test from 'node:test';
import { bridgeView } from '../desktop/src/bridge-dialog.ts';
import { initialBridgeState, reduceBridge } from '../desktop/src/bridge-state.ts';

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
  const save = bridgeView(reduceBridge({ ...initialBridgeState(), step:'saving', operation },
    {type:'failed',error:{code:'EIO',message:'Save failed'}}));
  assert.match(save.message, /setup exists/);
  assert.equal(save.retrySave,true);
});
test('a rejected new plan after completed removal cannot retry saving the old operation', () => {
  const inspection={support:'ready',selection:null,readiness:'needsSetup',activeOperation:operation};
  let state=reduceBridge(initialBridgeState(),{type:'inspected',inspection});
  state=reduceBridge(state,{type:'saved'});
  assert.equal(state.step,'choose');
  state=reduceBridge(state,{type:'select',adapterId:'enp0s1'});
  state=reduceBridge(state,{type:'failed',error:{code:'EUNSUPPORTED',message:'Select a persistent connection.'}});
  assert.equal(bridgeView(state).retrySave,false);
  assert.doesNotMatch(bridgeView(state).message,/Host setup exists|Retry saving/);
  assert.equal(reduceBridge(state,{type:'retrySave'}),state);
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

function installDom(t) {
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
  const ticks = new Set();
  globalThis.window={setInterval:fn=>{ticks.add(fn);return fn;},clearInterval:fn=>ticks.delete(fn)};
  t.after(()=>{
    for (const dialog of document.body.querySelectorAll('dialog')) if (dialog.open) dialog.close();
    globalThis.document=oldDocument;globalThis.window=oldWindow;
  });
  const trigger=new Element('button'); document.body.append(trigger); trigger.focus();
  return { document, trigger, tick:()=>{for(const fn of ticks) fn();} };
}

test('wizard submits Apply once, waits for saved verification, and restores focus', async (t) => {
  const { document, trigger } = installDom(t);
  let handler, applyCount=0, unwatchCount=0, resolveApply;
  const inspection={support:'ready',platform:'linux',adapters:[{id:'eth0',name:'Ethernet',eligible:true,linkUp:true,fingerprint:'one',kind:'ethernet',reason:null}],capabilities:{create:true,existingBridge:true,reason:null},selection:null,readiness:'needsSetup',activeOperation:null,externalOverride:null};
  const backend={
    watch:async cb=>{handler=cb;return()=>unwatchCount++;},
    inspect:async()=>inspection,
    plan:async request=>({id:operation.id,request,fingerprint:'one',changes:['Create bridge'],mayDisconnect:false,requiresAuthorization:true,expiresAt:Math.floor(Date.now()/1000)+300}),
    apply:async()=>{applyCount++; return await new Promise(resolve=>resolveApply=resolve);},
    verify:async()=>({...inspection,selection:{resourceId:operation.id,adapterId:'eth0'},readiness:'ready'}),
  };
  const {openBridgeDialog}=await import('../desktop/src/bridge-dialog.ts');
  const {dialogOpen}=await import('../desktop/src/dialog.ts');
  const finished=openBridgeDialog(backend,trigger);
  const flush=()=>new Promise(resolve=>setImmediate(resolve));
  await flush();
  const dialog=document.body.querySelectorAll('dialog')[0];
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

test('a running transport shows In use without re-verifying, saving, or offering repair', async (t) => {
  const {document,trigger}=installDom(t);
  const {openBridgeDialog}=await import('../desktop/src/bridge-dialog.ts');
  const inspection={support:'ready',platform:'linux',adapters:[],capabilities:{create:true,existingBridge:true,reason:null},
    selection:{resourceId:operation.id,adapterId:'eth0',displayName:'Ethernet'},readiness:'inUse',activeOperation:operation,externalOverride:null};
  let verifies=0;
  const finished=openBridgeDialog({watch:async()=>()=>{},inspect:async()=>inspection,verify:async()=>{verifies++;return inspection;}},trigger);
  await new Promise(resolve=>setImmediate(resolve));
  const dialog=document.body.querySelectorAll('dialog')[0];
  assert.equal(verifies,0);
  assert.match(dialog.textContent,/bridge is in use/);
  assert.match(dialog.textContent,/has not been reverified/);
  assert.doesNotMatch(dialog.textContent,/Bridge ready|needs repair|could not finish/);
  for(const label of ['Repair','Remove setup']) assert.equal(dialog.querySelectorAll('button').find(e=>e.textContent===label).disabled,true);
  dialog.querySelectorAll('button').find(e=>e.textContent==='Close').dispatchEvent(new Event('click'));
  await finished;
});

test('confirmed cleanup cannot offer cancellation and cleanup failure offers a completion retry', () => {
  const finalizing=bridgeView({...initialBridgeState(),step:'progress',operation:{...operation,state:'finalizing'}});
  assert.equal(finalizing.title,'Finishing bridge changes');
  assert.equal(finalizing.canCancel,false);
  assert.equal(finalizing.canClose,false);
  const failed=bridgeView({...initialBridgeState(),step:'failed',operation:{...operation,state:'failed'},error:{code:'ECLEANUP',message:'Cleanup needs attention'}});
  assert.equal(failed.retryRecovery,'Retry cleanup');
  assert.equal(failed.retrySave,false);
});


for (const duringPrompt of [false, true]) test(duringPrompt
  ? 'cancelling during authorization ignores a late Apply rejection'
  : 'cancellation stays queued across a busy native operation and disables Keep', async (t) => {
  const {document,trigger,tick}=installDom(t);
  const {openBridgeDialog}=await import('../desktop/src/bridge-dialog.ts');
  const inspection={support:'ready',platform:'windows',adapters:[{id:'ethernet',name:'Ethernet',eligible:true,linkUp:true,fingerprint:'one',kind:'ethernet',reason:null}],capabilities:{create:true,existingBridge:false,reason:null},selection:null,readiness:'needsSetup',activeOperation:null,externalOverride:null};
  let handler, resolveApply, rejectApply, cancelCalls=0, locked=!duringPrompt;
  const backend={
    watch:async cb=>{handler=cb;return()=>{};}, inspect:async()=>inspection,
    plan:async request=>({id:operation.id,request,fingerprint:'one',changes:['Create bridge'],mayDisconnect:true,requiresAuthorization:true,expiresAt:Math.floor(Date.now()/1000)+300}),
    apply:()=>new Promise((resolve,reject)=>{resolveApply=resolve;rejectApply=reject;}),
    cancel:async id=>{assert.equal(id,operation.id);cancelCalls++;if(locked)throw {code:'EBUSY',message:'Native step is running'};return {...operation,revision:4,state:'cancelled'};},
    keep:async()=>{assert.fail('Keep must not run after requesting cancellation');},
  };
  const finished=openBridgeDialog(backend,trigger);
  const flush=()=>new Promise(resolve=>setImmediate(resolve));
  await flush();
  const dialog=document.body.querySelectorAll('dialog')[0];
  const find=label=>dialog.querySelectorAll('button').find(e=>e.textContent===label);
  const click=label=>{const b=find(label);assert.ok(b,label);assert.equal(b.disabled,false,label);b.dispatchEvent(new Event('click'));};
  dialog.querySelectorAll('input')[0].dispatchEvent(new Event('change'));
  click('Review setup');await flush();click('Apply setup');
  if(!duringPrompt)handler({...operation,revision:1,state:'applying'});
  click('Cancel setup');await flush();
  assert.equal(cancelCalls,1);
  if(duringPrompt){
    rejectApply({code:'EAUTHDENIED',message:'The cancelled approval expired'});await flush();
    assert.doesNotMatch(dialog.textContent,/could not finish|approval expired/);
  }else{
    assert.match(dialog.textContent,/Cancellation requested/);
    assert.doesNotMatch(dialog.textContent,/could not finish/);
    locked=false;
    resolveApply({...operation,revision:2,state:'verifying',confirmationDeadline:Math.floor(Date.now()/1000)+120});await flush();
    assert.equal(find('Keep connection').disabled,true);
    tick();await flush();
    assert.equal(cancelCalls,2);
  }
  assert.match(dialog.textContent,/Choose a connection/);
  click('Close');await finished;
});
