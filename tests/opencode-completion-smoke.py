#!/usr/bin/env python3
"""Installed OpenCode + real reporter, isolated home and local fixture model only."""
import http.server
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request

REPO = Path(__file__).resolve().parents[1]

class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_POST(self):
        try: self.respond()
        except (BrokenPipeError, ConnectionResetError): pass  # Expected on cancellation.

    def respond(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        latest = next((m for m in reversed(body.get('messages', [])) if m.get('role') == 'user'), {})
        if 'fixture-cancel' in json.dumps(latest): time.sleep(2)
        if 'fixture-failure' in json.dumps(latest):
            self.send_response(400)
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(b'{"error":{"message":"Fixture rejected request","type":"invalid_request_error"}}')
            return
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        for delta, finish in [({'role':'assistant','content':'Fixture completed successfully.'}, None), ({}, 'stop')]:
            packet = {'id':'chatcmpl-fixture','object':'chat.completion.chunk','created':int(time.time()),
                      'model':'fixture','choices':[{'index':0,'delta':delta,'finish_reason':finish}]}
            self.wfile.write(('data: '+json.dumps(packet)+'\n\n').encode())
        self.wfile.write(b'data: [DONE]\n\n')


def main():
    binary = shutil.which('opencode')
    if shutil.which('mise'):
        resolved = subprocess.run(['mise','which','opencode'],capture_output=True,text=True)
        if resolved.returncode == 0: binary = resolved.stdout.strip()
    if not binary: raise RuntimeError('Installed OpenCode required')
    server = http.server.ThreadingHTTPServer(('127.0.0.1',0), Model)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    with tempfile.TemporaryDirectory(prefix='sd-opencode-completion-') as temp:
        home = Path(temp)
        root = home/'.local/state/super-desktop/harness'
        root.mkdir(parents=True)
        metadata = root/'probe.json'
        metadata.write_text(json.dumps({'version':1,'agent':'opencode','status':'unknown'}))
        with socket.socket() as sock:
            sock.bind(('127.0.0.1',0)); port = sock.getsockname()[1]
        env = os.environ.copy()
        env.update(HOME=temp, XDG_CONFIG_HOME=str(home/'config'), XDG_DATA_HOME=str(home/'data'),
                   XDG_STATE_HOME=str(home/'state'), XDG_CACHE_HOME=str(home/'cache'),
                   SD_HARNESS_FILE=str(metadata), SD_HARNESS_EXE=str(REPO/'target/debug/super-desktop'),
                   SD_HARNESS_AGENT='opencode', OPENCODE_DISABLE_MODELS_FETCH='true')
        for key in ('TMUX','TMUX_PANE','OPENCODE_SERVER_PASSWORD','OPENCODE_SERVER_USERNAME','OPENCODE_CONFIG'):
            env.pop(key,None)
        env['OPENCODE_CONFIG_CONTENT'] = json.dumps({'plugin':[(REPO/'assets/harness/opencode.mjs').as_uri()],
            'enabled_providers':['fixture'], 'model':'fixture/fixture', 'small_model':'fixture/fixture',
            'provider':{'fixture':{'npm':'@ai-sdk/openai-compatible','name':'Local fixture',
                'options':{'baseURL':f'http://127.0.0.1:{server.server_port}/v1','apiKey':'fixture'},
                'models':{'fixture':{'name':'Fixture','limit':{'context':32000,'output':1024}}}}}})
        def request(route, body=None):
            req = urllib.request.Request(f'http://127.0.0.1:{port}'+route,
                data=None if body is None else json.dumps(body).encode(), headers={'Content-Type':'application/json'})
            with urllib.request.urlopen(req, timeout=20) as response:
                data=response.read()
                return json.loads(data) if data else None
        def wait(predicate):
            deadline=time.monotonic()+20
            while time.monotonic()<deadline:
                value=json.loads(metadata.read_text())
                if predicate(value): return value
                time.sleep(.1)
            raise AssertionError('Unexpected metadata: '+metadata.read_text())
        with open(home/'log','w+') as log:
            process=subprocess.Popen(['sh','-c','export SD_HARNESS_PID=$$; printf "{}" | "$SD_HARNESS_EXE" harness-event init; exec "$@"',
                'probe',binary,'serve','--hostname','127.0.0.1','--port',str(port)],
                cwd=temp,env=env,stdout=log,stderr=log,start_new_session=True)
            try:
                deadline=time.monotonic()+25
                while True:
                    try: session=request('/session',{'title':'Completion fixture'})['id']; break
                    except OSError:
                        if time.monotonic()>deadline: raise
                        time.sleep(.2)
                request('/tui/select-session',{'sessionID':session})
                wait(lambda v:v.get('completion_supported'))
                def prompt(text):
                    return request('/session/'+session+'/message',{'model':{'providerID':'fixture','modelID':'fixture'},
                        'parts':[{'type':'text','text':text}]})
                prompt('fixture-success')
                first=wait(lambda v:v.get('status')=='completed' and v.get('completion_id'))['completion_id']
                prompt('fixture-success-again')
                wait(lambda v:v.get('status')=='completed' and v.get('completion_id')!=first)
                prompt('fixture-failure')
                wait(lambda v:v.get('status')=='error' and not v.get('completion_id'))
                session=request('/session',{'title':'Cancellation fixture'})['id']
                request('/tui/select-session',{'sessionID':session})
                request('/session/'+session+'/prompt_async',{'model':{'providerID':'fixture','modelID':'fixture'},
                    'parts':[{'type':'text','text':'fixture-cancel'}]})
                wait(lambda v:v.get('native_session')==session and v.get('status')=='working')
                time.sleep(.3)
                request('/session/'+session+'/abort',{})
                wait(lambda v:v.get('status') in ('idle','error') and not v.get('completion_id'))
                time.sleep(2.5)
                assert not json.loads(metadata.read_text()).get('completion_id')
                print('PASS: installed OpenCode successful turns, distinct durable IDs, provider-error and cancellation suppression')
            except Exception:
                try: print("Native fixture messages:", json.dumps(request("/session/"+session+"/message?limit=16")))
                except Exception: pass
                log.flush(); log.seek(0)
                print(log.read()[-5000:])
                raise
            finally:
                os.killpg(process.pid,signal.SIGTERM)
                try: process.wait(timeout=5)
                except subprocess.TimeoutExpired: os.killpg(process.pid,signal.SIGKILL); process.wait()
    server.shutdown(); server.server_close()

if __name__ == '__main__': main()
