#!/usr/bin/env python3
"""F27 public API controls: metadata only, never execute Redis APIs."""
import argparse,json,subprocess,hashlib
from pathlib import Path

def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def main():
 p=argparse.ArgumentParser();p.add_argument('--library',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args();a.output.mkdir(parents=True,exist_ok=False)
 pre='use redis_module::{key::RedisKeyWritable, RedisString, RedisError, raw};\n'
 cases=[
 ('typed-signatures', '''fn f(k:&RedisKeyWritable,s:RedisString,f:&RedisString,v:&RedisString)->Result<(),RedisError>{
 let _:Result<usize,RedisError>=k.hash_set("f",s);
 let _:Result<usize,RedisError>=k.hash_del("f");
 let _:Result<usize,RedisError>=k.hash_set_by_string(f,v);
 let _:Result<usize,RedisError>=k.hash_del_by_string(f); Ok(()) }''',None),
 ('borrowed-reuse','fn f(k:&RedisKeyWritable,f:RedisString,v:RedisString)->Result<(),RedisError>{k.hash_set_by_string(&f,&v)?; k.hash_del_by_string(&f)?; let _=(f,v); Ok(())}',None),
 ('consumed-setter','fn f(k:&RedisKeyWritable,v:RedisString){let _=k.hash_set("f",v);let _=v.as_slice();}','E0382'),
 ('no-status','fn f(k:&RedisKeyWritable){let _:raw::Status=k.hash_del("f");}','E0308'),
 ('borrow-required','fn f(k:&RedisKeyWritable,f:RedisString,v:RedisString){let _=k.hash_set_by_string(f,v);}','E0308'),
 ('ignored-result','#![deny(unused_must_use)]\n'+pre+'fn f(k:&RedisKeyWritable){k.hash_del("f");}','unused_must_use'),
 ('raw-signatures','''fn f(){
 let _:unsafe fn(*mut raw::RedisModuleKey,&str,*mut raw::RedisModuleString)->Result<usize,RedisError>=raw::hash_set;
 let _:unsafe fn(*mut raw::RedisModuleKey,&str)->Result<usize,RedisError>=raw::hash_del;
 let _:unsafe fn(*mut raw::RedisModuleKey,*mut raw::RedisModuleString,*mut raw::RedisModuleString)->Result<usize,RedisError>=raw::hash_set_by_string;
 let _:unsafe fn(*mut raw::RedisModuleKey,*mut raw::RedisModuleString)->Result<usize,RedisError>=raw::hash_del_by_string; }''',None)]
 for name,call in [('set','raw::hash_set(k,"f",v)'),('del','raw::hash_del(k,"f")'),('set-string','raw::hash_set_by_string(k,f,v)'),('del-string','raw::hash_del_by_string(k,f)')]:
  cases.append(('unsafe-'+name,'fn f(k:*mut raw::RedisModuleKey,f:*mut raw::RedisModuleString,v:*mut raw::RedisModuleString){let _='+call+';}','E0133'))
 report={'library':str(a.library),'library_sha256':sha(a.library),'native_executed':False,'cases':[]}
 for i,(name,code,expected) in enumerate(cases):
  source=a.output/f'case_{i}.rs';source.write_text(code if code.startswith('#!') else pre+code)
  command=['rustup','run','1.94.0','rustc','--edition=2021','--crate-type=lib','--emit=metadata','--error-format=json','--extern',f'redis_module={a.library}','-L','dependency='+str(a.library.parent),'--out-dir',str(a.output),str(source)]
  r=subprocess.run(command,capture_output=True,text=True);(a.output/f'case_{i}.jsonl').write_text(r.stderr)
  codes=[x['code']['code'] for x in (json.loads(l) for l in r.stderr.splitlines() if l.startswith('{')) if x.get('code')]
  passed=r.returncode==0 if expected is None else r.returncode!=0 and expected in codes
  report['cases'].append({'name':name,'source_sha256':sha(source),'exit':r.returncode,'expected':expected,'codes':codes,'passed':passed})
  (a.output/'report.json').write_text(json.dumps(report,indent=2)+'\n');print(name,passed,flush=True)
  assert passed,(name,r.stderr)
if __name__=='__main__':main()
