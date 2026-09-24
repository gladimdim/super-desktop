#!/usr/bin/env python3
"""Real Claude hooks with an isolated home and local Anthropic fixture; no paid API."""
import http.server
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import threading
import time

REPO = Path(__file__).resolve().parents[1]

class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_POST(self):
        try: self.respond()
        except (BrokenPipeError, ConnectionResetError): pass
    def respond(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        if 'count_tokens' in self.path:
            self.send_response(200); self.send_header('Content-Type','application/json'); self.end_headers()
            self.wfile.write(b'{"input_tokens":10}'); return
        latest = next((m for m in reversed(body.get('messages', [])) if m.get('role') == 'user'), {})
        if 'fixture-cancel' in json.dumps(latest): time.sleep(3)
        if 'fixture-failure' in json.dumps(latest):
            self.send_response(400); self.send_header('Content-Type','application/json'); self.end_headers()
            self.wfile.write(b'{"type":"error","error":{"type":"invalid_request_error","message":"Fixture rejected request"}}'); return
        self.send_response(200); self.send_header('Content-Type','text/event-stream'); self.end_headers()
        events = [
            ('message_start', {'message':{'id':'msg_fixture','type':'message','role':'assistant','model':body.get('model'),
                'content':[],'stop_reason':None,'stop_sequence':None,'usage':{'input_tokens':10,'output_tokens':0}}}),
            ('content_block_start', {'index':0,'content_block':{'type':'text','text':''}}),
            ('content_block_delta', {'index':0,'delta':{'type':'text_delta','text':'Fixture response complete.'}}),
            ('content_block_stop', {'index':0}),
            ('message_delta', {'delta':{'stop_reason':'end_turn','stop_sequence':None},'usage':{'output_tokens':5}}),
            ('message_stop', {}),
        ]
        for kind, event in events:
            self.wfile.write(('event: '+kind+'\ndata: '+json.dumps({'type':kind,**event})+'\n\n').encode())
        self.wfile.flush()


def main():
    binary = shutil.which('claude')
    if shutil.which('mise'):
        result=subprocess.run(['mise','which','claude'],capture_output=True,text=True)
        if result.returncode==0: binary=result.stdout.strip()
    if not binary: raise RuntimeError('Installed Claude Code required')
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Model)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    with tempfile.TemporaryDirectory(prefix='sd-claude-completion-') as temp:
        home=Path(temp); root=home/'.local/state/super-desktop/harness'; root.mkdir(parents=True)
        state=root/'probe.json'; state.write_text(json.dumps({'version':1,'agent':'claude','status':'unknown'}))
        events=home/'events'; events.write_text('')
        reporter=home/'report.py'
        reporter.write_text('''import json,os,subprocess,sys
from pathlib import Path
payload=sys.stdin.read()
subprocess.run([os.environ['SD_HARNESS_EXE'],'harness-event','claude'],input=payload,text=True,check=True)
event=json.loads(payload)
with open(os.environ['SD_TEST_EVENTS'],'a') as out:
 out.write(json.dumps({'hook':event['hook_event_name'],'has_text':bool(event.get('last_assistant_message')), 'active':event.get('stop_hook_active'), 'state':json.loads(Path(os.environ['SD_HARNESS_FILE']).read_text())})+'\\n')
''')
        import shlex
        command=shlex.quote(shutil.which('python3'))+' '+shlex.quote(str(reporter))
        hooks={event:[{'hooks':[{'type':'command','command':command,'timeout':3}]}]
            for event in ['SessionStart','UserPromptSubmit','Stop','StopFailure','SessionEnd','PreToolUse','PermissionRequest']}
        env={'PATH':os.environ['PATH'],'HOME':temp,'TERM':'xterm-256color',
            'CLAUDE_CONFIG_DIR':str(home/'claude'),'CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC':'1',
            'ANTHROPIC_BASE_URL':f'http://127.0.0.1:{server.server_port}','ANTHROPIC_API_KEY':'fixture',
            'SD_HARNESS_EXE':str(REPO/'target/debug/super-desktop'),'SD_HARNESS_FILE':str(state),
            'SD_HARNESS_AGENT':'claude','SD_TEST_EVENTS':str(events)}
        with open(home/'log','w+') as log:
            process=subprocess.Popen(['sh','-c','export SD_HARNESS_PID=$$; printf "{}" | "$SD_HARNESS_EXE" harness-event init; exec "$@"',
                'probe',binary,'--print','--verbose','--input-format','stream-json','--output-format','stream-json',
                '--model','claude-sonnet-4-6','--tools','','--settings',json.dumps({'hooks':hooks})],
                cwd=temp,env=env,stdin=subprocess.PIPE,stdout=log,stderr=log,text=True,start_new_session=True)
            def write(value): process.stdin.write(json.dumps(value)+'\n'); process.stdin.flush()
            def prompt(text): write({'type':'user','message':{'role':'user','content':text}})
            def wait(predicate):
                deadline=time.monotonic()+25
                while time.monotonic()<deadline:
                    records=[json.loads(line) for line in events.read_text().splitlines()]
                    if predicate(records): return records
                    if process.poll() is not None: break
                    time.sleep(.1)
                raise AssertionError('Hook events: '+events.read_text())
            try:
                prompt('fixture-success')
                records=wait(lambda rows:any(r['state'].get('status')=='completed' for r in rows))
                first=next(r['state']['completion_id'] for r in records if r['state'].get('status')=='completed')
                prompt('fixture-success-again')
                wait(lambda rows:any(r['state'].get('status')=='completed' and r['state'].get('completion_id')!=first for r in rows))
                prompt('fixture-failure')
                wait(lambda rows:any(r['hook']=='StopFailure' and r['state']['status']=='error' and not r['state'].get('completion_id') for r in rows))
                prompt('fixture-cancel')
                wait(lambda rows:rows and rows[-1]['state'].get('prompt')=='fixture-cancel')
                write({'type':'control_request','request_id':'fixture-interrupt','request':{'subtype':'interrupt'}})
                time.sleep(3.5)
                assert not json.loads(state.read_text()).get('completion_id')
                print('PASS: installed Claude successful turns, unique IDs, StopFailure and interruption suppression')
            except Exception:
                log.flush(); log.seek(0); print(log.read()[-5000:]); raise
            finally:
                if process.poll() is None: os.killpg(process.pid,signal.SIGTERM)
                try:process.wait(timeout=5)
                except subprocess.TimeoutExpired:os.killpg(process.pid,signal.SIGKILL);process.wait()
    server.shutdown(); server.server_close()

if __name__=='__main__': main()
