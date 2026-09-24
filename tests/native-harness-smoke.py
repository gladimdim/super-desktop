#!/usr/bin/env python3
"""Optional installed CLI checks: isolated HOME, no prompts or model requests. Run cargo build first."""
import json, os, pathlib, shutil, subprocess, tempfile, time, urllib.request, socket, shlex
repo=pathlib.Path(__file__).resolve().parents[1]
exe=str(repo/'target/debug/super-desktop')
def probe(agent):
  temp=tempfile.TemporaryDirectory(prefix='sd-native-'+agent+'-'); home=pathlib.Path(temp.name)
  root=home/'.local/state/super-desktop/harness'; root.mkdir(parents=True)
  state=root/'probe.json'; state.write_text(json.dumps({'version':1,'agent':agent,'status':'unknown'}))
  env=os.environ.copy(); env.update(HOME=str(home),XDG_CONFIG_HOME=str(home/'config'),XDG_DATA_HOME=str(home/'data'),XDG_STATE_HOME=str(home/'state'),XDG_CACHE_HOME=str(home/'cache'),SD_HARNESS_FILE=str(state),SD_HARNESS_EXE=exe,SD_HARNESS_AGENT=agent,PI_CODING_AGENT_DIR=str(home/'pi'),PI_OFFLINE='1',OPENCODE_DISABLE_MODELS_FETCH='true')
  env.pop('TMUX',None); env.pop('TMUX_PANE',None)
  binary = shutil.which(agent)
  if shutil.which('mise'):
    resolved = subprocess.run(['mise','which',agent],text=True,capture_output=True)
    if resolved.returncode == 0: binary = resolved.stdout.strip()
  if not binary:
    print(agent, 'SKIP: not installed', flush=True); temp.cleanup(); return
  if agent=='claude':
    reporter=shlex.quote(exe)+' harness-event claude'
    hooks={event:[{'hooks':[{'type':'command','command':reporter,'timeout':3}]}]
           for event in ['SessionStart','PostModelSwitch','StopFailure','PostCompact']}
    args=[binary,'--init-only','--settings',json.dumps({'hooks':hooks})]
  elif agent=='pi':
    args=[binary,'--offline','--no-extensions','--extension',str(repo/'assets/harness/pi.mjs'),
          '--extension',str(repo/'tests/fixtures/pi-probe-provider.mjs'),'--provider','sd-probe','--model','fixture',
          '--no-session','--name','Integration probe','--mode','rpc']
  else:
    sock=socket.socket(); sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]; sock.close()
    env['OPENCODE_CONFIG_CONTENT']=json.dumps({'plugin':[(repo/'assets/harness/opencode.mjs').as_uri()]})
    args=[binary,'serve','--hostname','127.0.0.1','--port',str(port)]
  log=open(home/'log','w+')
  script='export SD_HARNESS_PID=$$; printf "{}" | "$SD_HARNESS_EXE" harness-event init; exec "$@"'
  process=subprocess.Popen(['sh','-c',script,'probe',*args],env=env,cwd=home,stdin=subprocess.PIPE,stdout=log,stderr=log,start_new_session=True)
  try:
    deadline=time.monotonic()+25
    created=False
    while time.monotonic()<deadline:
      if agent=='opencode' and not created:
        try:
          req=urllib.request.Request(f'http://127.0.0.1:{port}/session',data=json.dumps({'title':'Integration probe'}).encode(),headers={'Content-Type':'application/json'})
          with urllib.request.urlopen(req,timeout=1) as response: data=json.load(response)
          req=urllib.request.Request(f'http://127.0.0.1:{port}/tui/select-session',data=json.dumps({'sessionID':data['id']}).encode(),headers={'Content-Type':'application/json'})
          with urllib.request.urlopen(req,timeout=1) as response: response.read()
          created=True
        except (OSError,ValueError): pass
      try:
        value=json.loads(state.read_text())
        if value.get('native_session') and (agent=='claude' or value.get('title')=='Integration probe'):
          if agent in ('pi', 'opencode'):
            original=value['native_session']
            def wait_for(predicate):
              until=time.monotonic()+5
              while time.monotonic()<until:
                current=json.loads(state.read_text())
                if predicate(current): return current
                time.sleep(.1)
              raise AssertionError(agent+' did not update metadata: '+state.read_text())
            if agent=='pi':
              process.stdin.write(b'{"type":"prompt","message":"probe-success"}\n'); process.stdin.flush()
              complete=wait_for(lambda current: current.get('status')=='completed' and current.get('completion_id'))
              first_completion=complete['completion_id']
              assert len(first_completion)==64
              process.stdin.write(b'{"type":"prompt","message":"probe-error"}\n'); process.stdin.flush()
              wait_for(lambda current: current.get('status')=='error' and not current.get('completion_id'))
              process.stdin.write(b'{"type":"prompt","message":"probe-success-again"}\n'); process.stdin.flush()
              wait_for(lambda current: current.get('status')=='completed' and current.get('completion_id')!=first_completion)
              print('pi PASS: installed runtime final settlement, provider failure, recovery and unique completion IDs',flush=True)
              process.stdin.write(b'{"type":"set_session_name","name":"Renamed probe"}\n'); process.stdin.flush()
              wait_for(lambda current: current.get('title')=='Renamed probe')
              process.stdin.write(b'{"type":"new_session"}\n'); process.stdin.flush()
              wait_for(lambda current: current.get('native_session')!=original and not current.get('title'))
            else:
              def request(route, body, method='POST'):
                req=urllib.request.Request(f'http://127.0.0.1:{port}'+route,data=json.dumps(body).encode(),headers={'Content-Type':'application/json'},method=method)
                with urllib.request.urlopen(req,timeout=3) as response: return json.load(response)
              request('/session/'+original,{'title':'Renamed probe'},'PATCH')
              wait_for(lambda current: current.get('title')=='Renamed probe')
              other=request('/session',{'title':'Unrelated background session'})
              time.sleep(.3)
              assert json.loads(state.read_text())['native_session']==original
              request('/tui/select-session',{'sessionID':other['id']})
              wait_for(lambda current: current.get('native_session')==other['id'] and current.get('title')=='Unrelated background session')
            print(agent,'PASS: rename and session switch isolate native metadata',flush=True)
          print(agent,'PASS: native adapter loaded; session identity, title, state =',value['status'],flush=True)
          return
      except (OSError,ValueError): pass
      if process.poll() is not None: break
      time.sleep(.2)
    log.seek(0); print(agent,'FAILED: state',json.loads(state.read_text()),'log:',log.read()[-2000:],flush=True)
    raise RuntimeError(agent+' native adapter failed')
  finally:
    import signal
    try:os.killpg(process.pid,signal.SIGTERM)
    except ProcessLookupError:pass
    try:process.wait(timeout=4)
    except subprocess.TimeoutExpired:os.killpg(process.pid,signal.SIGKILL);process.wait()
    log.close();temp.cleanup()
if __name__ == '__main__':
  for agent in ['claude','pi','opencode']: probe(agent)
