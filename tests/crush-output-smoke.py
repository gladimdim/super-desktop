#!/usr/bin/env python3
"""Installed Crush 0.96.x paging regression: isolated saved messages, no model requests."""
import subprocess,tempfile,pathlib,os,json,time,sqlite3,signal,shutil
root=pathlib.Path(tempfile.mkdtemp(prefix='sd-crush-history-'))
exe=os.environ.get('CRUSH_TEST_BINARY')
if not exe and shutil.which('mise'):
 resolved=subprocess.run(['mise','which','crush'],capture_output=True,text=True)
 if resolved.returncode==0:exe=resolved.stdout.strip()
if not exe:raise SystemExit('Set CRUSH_TEST_BINARY to an installed Crush binary (not an installer wrapper).')
sock='sd-crush-history-'+str(os.getpid())
env={k:v for k,v in os.environ.items() if not any(x in k.upper() for x in ['TOKEN','API_KEY','SECRET'])}
for key,suffix in [('HOME','home'),('XDG_CONFIG_HOME','config'),('XDG_DATA_HOME','data'),('XDG_STATE_HOME','state'),('XDG_CACHE_HOME','cache'),('XDG_RUNTIME_DIR','runtime'),('CRUSH_GLOBAL_CONFIG','config/crush'),('CRUSH_GLOBAL_DATA','data/crush')]:
 p=root/suffix;p.mkdir(parents=True,exist_ok=True);p.chmod(0o700);env[key]=str(p)
env.update(CRUSH_DISABLE_PROVIDER_AUTO_UPDATE='1',CRUSH_DISABLE_METRICS='1')
config={'providers':{'fixture':{'id':'fixture','name':'Fixture','type':'openai-compat','base_url':'http://127.0.0.1:1/v1','api_key':'local-fixture','discover_models':False,'models':[{'id':'fixture','name':'Fixture','context_window':32000,'default_max_tokens':4096}]}},'models':{k:{'model':'fixture','provider':'fixture'} for k in ['large','small']},'options':{'disable_provider_auto_update':True}}
(root/'crush.json').write_text(json.dumps(config))
(root/'AGENTS.md').write_text('Local output fixture; no tools or model calls.\n')
args=[exe,'--data-dir',str(root/'db'),'--host','unix://'+str(root/'runtime/server.sock')]
def tmux(*a,check=True):return subprocess.run(['tmux','-L',sock,*a],env=env,text=True,capture_output=True,check=check)
def capture():return tmux('capture-pane','-p','-t','probe').stdout
try:
 tmux('new-session','-d','-s','probe','-x','76','-y','20','-c',str(root),*args)
 time.sleep(3)
 assert 'initialize this project' not in capture(), 'Unexpected setup prompt'
 tmux('kill-session','-t','probe')
 db=root/'db/crush.db'

 if not db.exists():raise RuntimeError('no isolated database')
 con=sqlite3.connect(db);now=int(time.time())
 con.execute('INSERT INTO sessions(id,title,updated_at,created_at) VALUES(?,?,?,?)',('output-fixture','Local output fixture',now,now))
 parts=[{'type':'text','data':{'text':'\n\n'.join(f'OUTPUT_LINE_{i:03d}' for i in range(80))}},{'type':'finish','data':{'reason':'end_turn','time':now}}]
 con.execute('INSERT INTO messages(id,session_id,role,parts,model,created_at,updated_at,finished_at) VALUES(?,?,?,?,?,?,?,?)',('output-message','output-fixture','assistant',json.dumps(parts),'fixture',now,now,now));con.commit();con.close()
 tmux('new-session','-d','-s','probe','-x','76','-y','20','-c',str(root),*args,'--session','output-fixture')
 time.sleep(3);before=capture()
 tmux('send-keys','-t','probe','-l','\t\x1b[5~');time.sleep(.5)
 after=capture()
 assert 'OUTPUT_LINE_079' in before and 'OUTPUT_LINE_072' in after, (before,after)
 tmux('send-keys','-t','probe','-l','\x1b[6~');time.sleep(.5)
 assert 'OUTPUT_LINE_079' in capture(), 'PageDown did not restore newest output'
 print('PASS: Crush Tab + PageUp reveals older saved output without model requests',flush=True)
finally:
 tmux('kill-server',check=False)
 # Only the server started with this unique fixture socket belongs to this probe.
 for proc in pathlib.Path('/proc').iterdir():
  if not proc.name.isdigit():continue
  try:
   cmd=(proc/'cmdline').read_bytes().split(b'\0')
   if cmd and cmd[0].decode()==exe and any(str(root).encode() in arg for arg in cmd):os.kill(int(proc.name),signal.SIGTERM)
  except (OSError,ProcessLookupError):pass
 shutil.rmtree(root,ignore_errors=True)
