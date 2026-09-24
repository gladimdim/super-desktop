import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, rmSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import pi from "../assets/harness/pi.mjs";
import { SuperDesktop } from "../assets/harness/opencode.mjs";
import openclaw from "../assets/harness/openclaw.mjs";
import { report } from "../assets/harness/report.mjs";

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "sd-harness-"));
  const exe = join(root, "reporter");
  const events = join(root, "events");
  writeFileSync(exe, '#!/bin/sh\ncat >> "$SD_TEST_EVENTS"\nprintf "\\n" >> "$SD_TEST_EVENTS"\n', { mode: 0o700 });
  writeFileSync(events, "");
  const before = { ...process.env };
  Object.assign(process.env, { SD_HARNESS_EXE:exe, SD_HARNESS_FILE:join(root,"state.json"), SD_HARNESS_PID:String(process.pid), SD_TEST_EVENTS:events });
  return { root, exe, events: () => readFileSync(events,"utf8").split("\n").filter(Boolean).map(JSON.parse),
    close: () => { for (const k of Object.keys(process.env)) if (!(k in before)) delete process.env[k]; Object.assign(process.env,before); rmSync(root,{recursive:true,force:true}); } };
}

test("Pi waits for final settlement and reports waiting, error, and session names", async () => {
  const f = fixture();
  try {
    const handlers = {};
    pi({ on:(name, fn) => { handlers[name]=fn; } });
    const ctx = { sessionManager: { getSessionId:()=>"pi-one",getSessionName:()=>"Named task",getBranch:()=>[] }, model:{provider:"test",id:"model"},isIdle:()=>true };
    await handlers.session_start({},ctx);
    assert.equal(f.events().at(-1).title,"Named task");
    await handlers.before_agent_start({prompt:"Fix it"},ctx);
    assert.equal(f.events().at(-1).status,"working");
    assert.equal(handlers.agent_end,undefined,"an intermediate agent_end is not idle");
    await handlers.ui_prompt_start({},ctx);
    assert.equal(f.events().at(-1).status,"waiting");
    await handlers.agent_before_settle({outcome:"error"},ctx);
    await handlers.agent_settled({},ctx);
    assert.equal(f.events().at(-1).status,"error");
    await handlers.before_agent_start({prompt:"Retry"},ctx);
    await handlers.agent_before_settle({outcome:"completed"},ctx);
    await handlers.agent_settled({},ctx);
    assert.equal(f.events().at(-1).status,"idle");
  } finally { f.close(); }
});

test("OpenCode ignores child agents and unrelated sessions, handles retry and permission waits", async () => {
  const f=fixture();
  try {
    const client={session:{get:async({path:{id}})=>({data:{id,title:`Title ${id}`,...(id==="child"?{parentID:"root"}:{})}})}};
    const hooks=await SuperDesktop({client});
    const chat=(sessionID)=>hooks["chat.message"]({sessionID},{parts:[{type:"text",text:"Own prompt"},{type:"text",text:"Internal context",synthetic:true}]});
    await chat("root");
    assert.equal(f.events().at(-1).prompt,"Own prompt");
    const before=f.events().length;
    await chat("child");
    await hooks.event({event:{type:"session.status",properties:{sessionID:"other",status:{type:"idle"}}}});
    assert.equal(f.events().length,before);
    for (const [type,properties,status] of [
      ["permission.asked",{},"waiting"], ["permission.replied",{},"working"],
      ["session.status",{status:{type:"retry"}},"working"], ["session.error",{},"error"],
      ["session.status",{status:{type:"idle"}},"error"],
    ]) {
      await hooks.event({event:{type,properties:{sessionID:"root",...properties}}});
      assert.equal(f.events().at(-1).status,status);
    }
    await chat("new");
    assert.equal(f.events().at(-1).session,"new");
  } finally { f.close(); }
});

test("OpenClaw gateway only reports explicitly mapped desktop TUI sessions", async () => {
  const f=fixture();
  try {
    process.env.HOME=f.root;
    const root=join(f.root,".local/state/super-desktop/harness"); mkdirSync(root,{recursive:true});
    const path=join(root,"mapped.json");writeFileSync(path,JSON.stringify({agent:"openclaw",pid:process.pid}));
    writeFileSync(join(root,"sd_term_test.link.json"),JSON.stringify({path,exe:f.exe}));
    const handlers={}; openclaw.register({on:(name,fn)=>{handlers[name]=fn;}});
    await handlers.before_model_resolve({prompt:"Private unrelated task"},{sessionKey:"agent:main:elsewhere"});
    assert.equal(f.events().length,0);
    const ctx={sessionKey:"agent:main:sd_term_test",modelId:"test-model"};
    await handlers.before_model_resolve({prompt:"Desktop task"},ctx);
    assert.equal(f.events().at(-1).status,"working");
    assert.equal(f.events().at(-1).session,ctx.sessionKey);
    await handlers.agent_end({success:false},ctx);
    assert.equal(f.events().at(-1).status,"error");
    await handlers.agent_end({success:true},ctx);
    assert.equal(f.events().at(-1).status,"idle");
  } finally { f.close(); }
});

test("OpenCode background sessions cannot claim a card; latest explicit selection wins", async () => {
  const f = fixture();
  try {
    let finishSlow;
    const hooks = await SuperDesktop({client:{session:{get:async ({path:{id}}) =>
      id === "slow" ? new Promise(resolve => { finishSlow = resolve; }) : {data:{id,title:id}} }}});
    const event = (type, properties) => hooks.event({event:{type,properties}});
    await event("session.created", {info:{id:"background",title:"Unrelated"}});
    await event("session.updated", {info:{id:"background",title:"Renamed"}});
    assert.equal(f.events().length, 1, "only the initial idle observation");
    const slow = event("tui.session.select", {sessionID:"slow"});
    await event("tui.session.select", {sessionID:"chosen"});
    finishSlow({data:{id:"slow",title:"Stale"}});
    await slow;
    assert.equal(f.events().at(-1).session, "chosen");
    await event("session.deleted", {info:{id:"chosen"}});
    assert.equal(f.events().at(-1).status, "unknown");
    const count = f.events().length;
    await event("session.status", {sessionID:"chosen",status:{type:"busy"}});
    assert.equal(f.events().length, count);
  } finally { f.close(); }
});

test("OpenCode starts idle and reports a selected session's own server status", async () => {
  const f = fixture();
  try {
    let busy = {};
    const client = {session:{get:async ({path:{id}}) => ({data:{id,title:"New session - 2026-09-24T17:17:47.798Z"}}),
      status:async () => ({data:busy})}};
    const hooks = await SuperDesktop({client});
    const event = (type, properties) => hooks.event({event:{type,properties}});
    // A fresh process has no turn in flight: IDLE before any prompt.
    assert.deepEqual(f.events(), [{status:"idle", emitter:f.events()[0].emitter}]);
    await event("tui.session.select", {sessionID:"resumed"});
    assert.equal(f.events().at(-1).status, "idle");
    assert.equal(f.events().at(-1).session, "resumed");
    busy = {other:{type:"idle"}, running:{type:"busy"}};
    await event("tui.session.select", {sessionID:"running"});
    assert.equal(f.events().at(-1).status, "working");
    // Malformed or failing lookups never guess.
    for (const bad of [async () => ({}), async () => ({data:[]}), async () => { throw new Error("offline"); }]) {
      client.session.status = bad;
      const count = f.events().length;
      await event("tui.session.select", {sessionID:`bad${count}`});
      assert.notEqual(f.events().at(-1).status, "idle");
      assert.notEqual(f.events().at(-1).status, "working");
    }
    // A status event that lands while the lookup runs wins.
    let finish;
    client.session.status = () => new Promise(resolve => { finish = resolve; });
    const pending = event("tui.session.select", {sessionID:"race"});
    await new Promise(resolve => setImmediate(resolve));
    await event("session.status", {sessionID:"race",status:{type:"busy"}});
    finish({data:{}});
    await pending;
    assert.equal(f.events().at(-1).status, "working");
  } finally { f.close(); }
});

test("OpenClaw tracks native titles, resets, model and approvals without accepting stale runs", async () => {
  const f = fixture();
  try {
    process.env.HOME = f.root;
    const root = join(f.root,".local/state/super-desktop/harness"); mkdirSync(root,{recursive:true});
    const path = join(root,"mapped.json"); writeFileSync(path,JSON.stringify({agent:"openclaw",pid:process.pid}));
    writeFileSync(join(root,"sd_term_test.link.json"),JSON.stringify({path,exe:f.exe}));
    const handlers = {}; let subscription;
    let entry = {sessionId:"native-one",label:"Named session"};
    openclaw.register({on:(name,fn)=>{handlers[name]=fn;},
      runtime:{agent:{session:{getSessionEntry:()=>entry}}},
      agent:{events:{registerAgentEventSubscription:value=>{subscription=value;}}}});
    const ctx = {sessionKey:"agent:main:sd_term_test",sessionId:"native-one",runId:"run-one"};
    await handlers.before_model_resolve({prompt:"Own prompt"},ctx);
    assert.equal(f.events().at(-1).title,"Named session");
    await handlers.llm_input({runId:"run-one",provider:"test",model:"model"},ctx);
    assert.equal(f.events().at(-1).model,"test/model");
    assert.equal(f.events().at(-1).prompt,undefined);
    subscription.handle({...ctx,stream:"approval",data:{phase:"requested",status:"pending"}});
    assert.equal(f.events().at(-1).status,"waiting");
    await handlers.agent_end({runId:"run-one",success:true},ctx);
    assert.equal(f.events().at(-1).status,"waiting");
    subscription.handle({...ctx,stream:"approval",data:{phase:"resolved",status:"denied"}});
    assert.equal(f.events().at(-1).status,"error");
    entry = {sessionId:"native-two",label:"Renamed"};
    const next = {...ctx,sessionId:"native-two",runId:"run-two"};
    await handlers.session_start({},next);
    await handlers.before_model_resolve({prompt:"Next"},next);
    assert.equal(f.events().at(-1).session,`${ctx.sessionKey}/native-two`);
    const count=f.events().length;
    await handlers.agent_end({runId:"run-one",success:true},ctx);
    subscription.handle({...ctx,stream:"approval",data:{phase:"requested",status:"pending"}});
    assert.equal(f.events().length,count);
  } finally { f.close(); }
});

test("Lifecycle reporter bounds large prompts and ignores reporter failures", () => {
  const f=fixture();
  try {
    report("pi",{prompt:"x".repeat(100000),session:"own",status:"working"});
    assert.equal(f.events().at(-1).prompt.length,1000);
    assert.doesNotThrow(()=>report("pi",{status:"working"},{...process.env,SD_HARNESS_EXE:"/nonexistent"}));
  } finally { f.close(); }
});

test("Pi completion requires final settlement and a successful final response", async () => {
  const f=fixture();
  try {
    const handlers={}; pi({on:(name,fn)=>{handlers[name]=fn;}});
    const ctx={sessionManager:{getSessionId:()=>"own",getSessionName:()=>"",getBranch:()=>[]},isIdle:()=>true};
    await handlers.session_start({},ctx);
    assert.equal(f.events().at(-1).completionSupported,true);
    await handlers.before_agent_start({prompt:"Task"},ctx);
    await handlers.message_end({message:{role:"assistant",stopReason:"stop",content:[{type:"text",text:"Done"}]}},ctx);
    assert.equal(f.events().at(-1).status,"working");
    await handlers.agent_before_settle({outcome:"completed"},ctx);
    await handlers.agent_settled({},ctx);
    assert.equal(f.events().at(-1).status,"completed");
    const turn=f.events().at(-1).completionTurn;
    await handlers.agent_settled({},ctx);
    assert.equal(f.events().at(-1).completionTurn,turn);
    for (const outcome of ["error","aborted"]) {
      await handlers.before_agent_start({prompt:"Next"},ctx);
      await handlers.message_end({message:{role:"assistant",stopReason:"stop",content:[{type:"text",text:"Intermediate"}]}},ctx);
      await handlers.agent_before_settle({outcome},ctx);
      await handlers.agent_settled({},ctx);
      assert.notEqual(f.events().at(-1).status,"completed");
      assert.equal(f.events().at(-1).completionTurn,undefined);
    }
    await handlers.before_agent_start({prompt:"Tool only"},ctx);
    await handlers.message_end({message:{role:"assistant",stopReason:"toolUse",content:[{type:"toolCall",name:"test"}]}},ctx);
    await handlers.agent_before_settle({outcome:"completed"},ctx);
    await handlers.agent_settled({},ctx);
    assert.notEqual(f.events().at(-1).status,"completed");
  } finally { f.close(); }
});

test("OpenCode completion needs own successful final response and rejects stale idle lookups", async () => {
  const f = fixture();
  try {
    let records = [], resolve;
    const client = { session: {
      get: async ({ path: { id } }) => ({ data: { id } }),
      messages: async () => ({ data: records }),
    }};
    const hooks = await SuperDesktop({ client });
    const chat = (id = "root", turn = "user-one") => hooks["chat.message"]({ sessionID: id },
      { message: { id: turn }, parts: [{ type: "text", text: "Task" }] });
    const event = (type, properties = {}) => hooks.event({ event: { type, properties: { sessionID: "root", ...properties } } });
    const idle = () => event("session.status", { status: { type: "idle" } });
    const user = { info: { id: "user-one", sessionID: "root", role: "user" }, parts: [] };
    const assistant = { info: { id: "answer", sessionID: "root", role: "assistant", parentID: "user-one", finish: "stop", time: { completed: 123 } }, parts: [{ type: "text", text: "Done" }] };
    await chat();
    assert.equal(f.events().find(e => e.completionSupported)?.session, "root");
    for (const patch of [{ finish: "tool-calls" }, { finish: "length" }, { error: { name: "AbortedError" } }, { summary: true }, { parentID: "other-user" }, { time: {} }]) {
      records = [user, { ...assistant, info: { ...assistant.info, ...patch } }];
      await idle();
      assert.equal(f.events().at(-1).status, "idle");
    }
    records = [user, { ...assistant, parts: [] }];
    await idle(); assert.equal(f.events().at(-1).status, "idle");
    records = [user, assistant];
    await idle(); assert.equal(f.events().at(-1).status, "completed");
    assert.equal(f.events().at(-1).completionTurn, "user-one");
    await event("session.error"); await idle();
    assert.equal(f.events().at(-1).status, "error");
    await chat();
    client.session.messages = () => new Promise(r => { resolve = r; });
    const harmless = idle();
    await event("session.idle");
    resolve({ data: records }); await harmless;
    assert.equal(f.events().at(-1).status, "completed", "legacy idle notification must not invalidate native lookup");
    const pending = idle();
    await event("permission.asked");
    resolve({ data: records }); await pending;
    assert.equal(f.events().at(-1).status, "waiting");
    const stale = idle();
    await chat("other", "user-two");
    resolve({ data: records }); await stale;
    assert.equal(f.events().at(-1).session, "other");
    assert.equal(f.events().at(-1).status, "working");
  } finally { f.close(); }
});
