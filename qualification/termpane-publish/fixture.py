# SPDX-FileCopyrightText: 2026 Alexey Zhokhov
# SPDX-License-Identifier: Apache-2.0

import http.server,json,os,pathlib,shlex,ssl,subprocess,tempfile,threading,tarfile,tomllib
intent=tomllib.loads(pathlib.Path('/test/intent.toml').read_text())
upload=tomllib.loads(pathlib.Path('/test/upload.toml').read_text())
assert intent['repository'] == 'tailrocks/termpane'
assert intent['packages'] == ['termpane']
assert intent['recovery']['package'] == 'termpane'
assert intent['recovery']['version'] == '0.1.1'
assert intent['recovery']['source_sha'] == 'f1c2dfde45bdbe91425af6bb17d80e606703baac'
features=intent['publish_features']['termpane']
assert len(upload['package']) == 1 and upload['package'][0]['name'] == 'termpane'
assert upload['package'][0].get('publish_features', []) == features
base=pathlib.Path('/tmp/release-test'); base.mkdir()
os.environ['TEST_REAL_PACKAGE'] = '1'
os.environ['TEST_OIDC_SUCCESS'] = '1'
subprocess.run(['openssl','req','-x509','-nodes','-newkey','rsa:2048','-keyout',str(base/'ca.key'),'-out',str(base/'ca.crt'),'-days','1','-subj','/CN=Fixture CA','-addext','basicConstraints=critical,CA:TRUE'],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
subprocess.run(['openssl','req','-nodes','-newkey','rsa:2048','-keyout',str(base/'key.pem'),'-out',str(base/'leaf.csr'),'-subj','/CN=api.github.com'],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
subprocess.run(['git','config','--system','--add','safe.directory','/source'],check=True)
(base/'ext').write_text('basicConstraints=critical,CA:FALSE\nsubjectAltName=DNS:api.github.com,DNS:github.com,DNS:fixture.actions.githubusercontent.com,DNS:crates.io\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n')
subprocess.run(['openssl','x509','-req','-in',str(base/'leaf.csr'),'-CA',str(base/'ca.crt'),'-CAkey',str(base/'ca.key'),'-CAcreateserial','-out',str(base/'leaf.crt'),'-days','1','-extfile',str(base/'ext')],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
subprocess.run(['cp',str(base/'ca.crt'),'/usr/local/share/ca-certificates/fixture.crt'],check=True)
subprocess.run(['update-ca-certificates'],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
events=[]
ancestor=""
class H(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  events.append(('GET',self.headers.get('host'),self.path))
  if self.path.startswith('/oidc'):
   if os.environ.get('TEST_OIDC_SUCCESS') == '1':
    assert self.headers.get('Authorization') == 'Bearer fixture-oidc'
    payload=b'{"value":"fixture-jwt"}'
    self.send_response(200);self.send_header('Content-Length',str(len(payload)));self.end_headers();self.wfile.write(payload);return
   self.send_response(403);self.send_header('Content-Length','0');self.end_headers();return
  pr={'user':{'id':1,'login':'fixture'},'number':1,'html_url':'https://github.com/fixture/fixture/pull/1','head':{'ref':'release-plz/fixture','sha':ancestor},'title':'fixture','body':None,'labels':[]}
  payload=json.dumps([{'author':None,'sha':ancestor}] if self.path.endswith('/pulls/1/commits') else [pr]).encode()
  self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(payload)));self.end_headers();self.wfile.write(payload)
 def do_POST(self):
  events.append(('POST',self.headers.get('host'),self.path))
  if os.environ.get('TEST_OIDC_SUCCESS') == '1' and self.headers.get('host') == 'crates.io' and self.path == '/api/v1/trusted_publishing/tokens':
   body=json.loads(self.rfile.read(int(self.headers.get('Content-Length','0'))));assert body == {'jwt':'fixture-jwt'}
   payload=b'{"token":"fixture-scoped"}';self.send_response(200);self.send_header('Content-Length',str(len(payload)));self.end_headers();self.wfile.write(payload);return
  self.send_response(403);self.send_header('Content-Length','0');self.end_headers()
 def do_DELETE(self):
  events.append(('DELETE',self.headers.get('host'),self.path))
  assert self.headers.get('host') == 'crates.io' and self.path == '/api/v1/trusted_publishing/tokens' and self.headers.get('Authorization') == 'Bearer fixture-scoped'
  self.send_response(204);self.send_header('Content-Length','0');self.end_headers()
 def log_message(self,*args):pass
srv=http.server.ThreadingHTTPServer(('127.0.0.1',443),H)
ctx=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);ctx.load_cert_chain(base/'leaf.crt',base/'key.pem');srv.socket=ctx.wrap_socket(srv.socket,server_side=True)
threading.Thread(target=srv.serve_forever,daemon=True).start()
real_cargo=subprocess.check_output(['rustup','which','cargo'],text=True).strip()
real_rustc=subprocess.check_output(['rustup','which','rustc'],text=True).strip()
real_rustdoc=subprocess.check_output(['rustup','which','rustdoc'],text=True).strip()
wrapper=base/'cargo';calls=base/'calls'
package_check = (shlex.quote(real_cargo) + ' package --offline "$@" >&2 || exit 73; ') if os.environ.get('TEST_REAL_PACKAGE') == '1' else ''
wrapper.write_text('#!/bin/sh\ncase "$1" in\n info) printf "error: could not find package\\n" >&2; exit 101 ;;\n publish) test "${CARGO_REGISTRY_TOKEN:-}" = fixture-scoped || exit 74; shift; printf "PUBLISH %s\\n" "$*" >> '+shlex.quote(str(calls))+'; git rev-parse HEAD >> '+shlex.quote(str(calls))+'; '+package_check+'printf "    Uploading fixture\\n    Published termpane v0.1.1 at fixture\\n" >&2; exit 0 ;;\n metadata) shift; exec '+shlex.quote(real_cargo)+' metadata --offline "$@" ;;\n *) exec '+shlex.quote(real_cargo)+' "$@" ;;\nesac\n');wrapper.chmod(0o755)
root=pathlib.Path('/source')
def git(*args):
 return subprocess.run(['git', '-c', 'safe.directory=/source', *args],cwd=root,check=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True).stdout.strip()
approved=git('rev-parse','HEAD')
assert approved == 'f1c2dfde45bdbe91425af6bb17d80e606703baac'
assert not git('status','--porcelain')
ancestor=git('rev-parse','HEAD^')
for case in ['recovery']:
 home=base/(case+'-home');home.mkdir();calls.unlink(missing_ok=True);events.clear()
 env={'PATH':str(base)+':/usr/bin:/bin','CARGO':str(wrapper),'RUSTC':real_rustc,'RUSTDOC':real_rustdoc,'HOME':str(home),'CARGO_HOME':'/cargo-cache','RUSTUP_HOME':'/usr/local/rustup','RUSTUP_AUTO_INSTALL':'0','RUNNER_TEMP':str(base),'GIT_TOKEN':'fixture-git','GITHUB_ACTIONS':'true','ACTIONS_ID_TOKEN_REQUEST_URL':'https://fixture.actions.githubusercontent.com/oidc','ACTIONS_ID_TOKEN_REQUEST_TOKEN':'fixture-oidc'}
 if case=='bootstrap':
  for key in ['GITHUB_ACTIONS','ACTIONS_ID_TOKEN_REQUEST_URL','ACTIONS_ID_TOKEN_REQUEST_TOKEN']:env.pop(key)
  env['CARGO_REGISTRY_TOKEN']='fixture-cargo'
 cmd=['/test/release-plz','release','--config',os.environ.get('TEST_CONFIG','/test/upload.toml'),'--manifest-path',str(root/'Cargo.toml')]
 result=subprocess.run(cmd,cwd=root,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=120)
 entries=calls.read_text() if calls.exists() else ''
 if os.environ.get('TEST_REAL_PACKAGE') == '1' and case != 'existing-tag' and result.returncode == 0:
  archive=root/'target/package/termpane-0.1.1.crate'
  with tarfile.open(archive,'r:gz') as bundle:
   source=bundle.extractfile('termpane-0.1.1/src/lib.rs').read()
   info=json.load(bundle.extractfile('termpane-0.1.1/.cargo_vcs_info.json'))
   grid=bundle.extractfile('termpane-0.1.1/src/grid.rs').read()
   assert grid == (root/'src/grid.rs').read_bytes() and b'enum EraseDisplayPolicy' in grid
  assert source == (root/'src/lib.rs').read_bytes() and info['git']['sha1'] == approved, 'packaged source mismatch'
 assert (case=='bootstrap') == (not any(host == 'fixture.actions.githubusercontent.com' for method,host,path in events)) or case=='existing-tag'
 assert not pathlib.Path('/cargo-cache/credentials.toml').exists()
 if os.environ.get('TEST_OIDC_SUCCESS') == '1' and case not in ('bootstrap','existing-tag'):
  assert ('POST','crates.io','/api/v1/trusted_publishing/tokens') in events and ('DELETE','crates.io','/api/v1/trusted_publishing/tokens') in events
 print(case,'exit',result.returncode,'upload',bool(entries),'events',events,'head_match',not entries or approved in entries,flush=True)
 lines=[line for line in entries.splitlines() if line.startswith('PUBLISH ')]
 assert len(lines) == 1
 args=shlex.split(lines[0])[1:]
 if features:
  assert args.count('--features') == 1 and set(args[args.index('--features')+1].split(',')) == set(features)
 else:
  assert '--features' not in args
 if result.returncode or (case!='existing-tag' and ('PUBLISH' not in entries or approved not in entries or 'termpane' not in entries or 'velnor_probe_b' in entries)) or (case=='existing-tag' and entries) or any(method=='POST' and host!='crates.io' for method,host,path in events):
  print('stderr:',result.stderr.decode(errors='replace')[-1800:],flush=True);raise SystemExit(1)
srv.shutdown()
