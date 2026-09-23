#!/usr/bin/env python3
"""Optional installed CLI checks: isolated HOME, no prompts or model requests. Run cargo build first."""
import json, os, pathlib, shutil, subprocess, tempfile, time, urllib.request, socket
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
  if agent=='pi':
    args=[binary,'--offline','--no-extensions','--extension',str(repo/'assets/harness/pi.mjs'),'--no-session','--name','Integration probe','--mode','rpc']
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
      if process.poll() is not None: break
      if agent=='opencode' and not created:
        try:
          req=urllib.request.Request(f'http://127.0.0.1:{port}/session',data=json.dumps({'title':'Integration probe'}).encode(),headers={'Content-Type':'application/json'})
          with urllib.request.urlopen(req,timeout=1) as response: data=json.load(response)
          created=True
        except (OSError,ValueError): pass
      try:
        value=json.loads(state.read_text())
        if value.get('native_session') and value.get('title')=='Integration probe':
          print(agent,'PASS: native adapter loaded; session identity, title, state =',value['status'],flush=True)
          return
      except (OSError,ValueError): pass
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
for agent in ['pi','opencode']: probe(agent)
