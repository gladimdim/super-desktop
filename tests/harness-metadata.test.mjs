import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, rmSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import pi from "../assets/harness/pi.mjs";
import { SuperDesktop } from "../assets/harness/opencode.mjs";
import openclaw from "../assets/harness/openclaw.mjs";

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
    const path=join(root,"mapped.json");writeFileSync(path,JSON.stringify({pid:process.pid}));
    writeFileSync(join(root,"sd_term_test.link.json"),JSON.stringify({path,exe:f.exe}));
    const handlers={}; openclaw.register({on:(name,fn)=>{handlers[name]=fn;}});
    await handlers.before_prompt_build({prompt:"Private unrelated task"},{sessionKey:"agent:main:elsewhere"});
    assert.equal(f.events().length,0);
    const ctx={sessionKey:"agent:main:sd_term_test",modelId:"test-model"};
    await handlers.before_prompt_build({prompt:"Desktop task"},ctx);
    assert.equal(f.events().at(-1).status,"working");
    assert.equal(f.events().at(-1).session,ctx.sessionKey);
    await handlers.agent_end({success:false},ctx);
    assert.equal(f.events().at(-1).status,"error");
    await handlers.agent_end({success:true},ctx);
    assert.equal(f.events().at(-1).status,"idle");
  } finally { f.close(); }
});
