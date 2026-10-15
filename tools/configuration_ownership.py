#!/usr/bin/env python3
"""Corrected F11 native getter/lifecycle checks on owned loopback servers.

Counts freshly owned getter replies forwarded through actual host APIs, not heap
or historical leak sampling. Each cell retains source/library/server provenance.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import time
import package_baseline as host

ROOT=Path(__file__).resolve().parents[1]

def capture_sources():
    files=[ROOT/'Cargo.toml',ROOT/'Cargo.lock',ROOT/'rust-toolchain.toml']
    for folder in ['src','crates','examples']:
        files.extend(p for p in (ROOT/folder).rglob('*') if p.is_file() and p.suffix in ['.rs','.c','.h'] and 'target' not in p.relative_to(ROOT).parts)
    files.append(Path(__file__))
    return {str(p.relative_to(ROOT)):host.sha256(p) for p in sorted(files)}

def one_cell(binary,version,profile,out):
    out.mkdir(parents=True,exist_ok=False)
    with socket.socket() as reservation:
        reservation.bind(('127.0.0.1',0));port=reservation.getsockname()[1]
    config=out/'redis.conf';log=out/'redis.log'
    config.write_text(f'bind 127.0.0.1\nport {port}\nsave ""\nappendonly no\nenable-module-command yes\ndir {out}\nlogfile {log}\n')
    modules={name:ROOT/'target'/profile/'examples'/('lib'+name+'.dylib') for name in ['configuration_ownership','configuration_ownership_b']}
    result={'status':'running','profile':profile,'version':version,'server':{'binary':str(binary),'sha256':host.sha256(binary),'version_output':subprocess.check_output([str(binary),'--version'],text=True).strip()},'modules':{n:{'path':str(p),'sha256':host.sha256(p)} for n,p in modules.items()},'cases':[]}
    def save():host.write_json(out/'report.json',result)
    def q(*args):return host.redis_command(port,*(a if isinstance(a,bytes) else str(a).encode() for a in args))
    def error(*args):
        try:q(*args)
        except host.BaselineError as e:
            assert str(e).startswith('Redis error:'),e
            return str(e)
        raise AssertionError('Expected ordinary command rejection')
    def get(name):
        answer=q('CONFIG','GET',name)
        assert len(answer)==2 and answer[0]==name.encode(),answer
        return answer[1]
    def check(name):
        issued,released,live,changed=q(name+'.stats')
        assert issued-released==live==3,(name,issued,released,live)
        return [issued,released,live,changed]
    process=subprocess.Popen([str(binary),str(config)],stdout=(out/'stdout.log').open('w'),stderr=subprocess.STDOUT,start_new_session=True)
    save()
    try:
        until=time.monotonic()+8
        while True:
            if process.poll() is not None:raise AssertionError('Owned server exited during startup')
            try:info=q('INFO','server');break
            except (OSError,host.BaselineError):
                if time.monotonic()>=until:raise
                time.sleep(.03)
        result['server']['observed']=host.verify_server_info(info,process.pid,version)
        (out/'server-info.txt').write_bytes(info)
        a='cfgowners';b='cfgowners_b';pa=str(modules['configuration_ownership']);pb=str(modules['configuration_ownership_b'])
        load_position=len(log.read_text(errors='replace'))
        assert q('MODULE','LOAD',pa)==b'OK'
        assert check(a)==[3,0,3,3] # fixture explicitly GETs all three during OnLoad
        for key in ['native','text','binary']:
            for value in [b'', 'héllo'.encode(),b'123456']:
                assert q('CONFIG','SET',a+'.'+key,value)==b'OK'
                for _ in range(25):assert get(a+'.'+key)==value
                before=check(a)
                assert q('CONFIG','SET',a+'.'+key,value)==b'OK'
                after=check(a)
                assert after[0]-before[0]==after[1]-before[1]>=1 and after[3]==before[3],(before,after)
                result.setdefault('same_value_getter_deltas',[]).append(after[0]-before[0])
                error('CONFIG','SET',a+'.'+key,'reject')
                assert get(a+'.'+key)==value
                check(a)
        assert 'F11_ALREADY_ACTIVE_REJECTED true' in log.read_text(errors="replace")[load_position:]
        result['cases'].append('bounded repeated GET/same-value SET, empty/Unicode/numeric-looking values, rejection/recovery, active-scope/post-commit admission')
        binary_value=b'left\0\xffright'
        assert q('CONFIG','SET',a+'.binary',binary_value)==b'OK'
        assert get(a+'.binary')==binary_value
        error('CONFIG','SET',a+'.text',b'\xff')
        assert q(a+'.update','direct-update')==b'OK'
        assert get(a+'.text')==b'direct-update'
        check(a)
        before={key:get(a+'.'+key) for key in ['native','text','binary']}
        assert q('CONFIG','REWRITE')==b'OK'
        after={key:get(a+'.'+key) for key in before};assert before==after
        rewritten=config.read_bytes();assert b'cfgowners.text' in rewritten and b'direct-update' in rewritten
        (out/'rewritten.conf').write_bytes(rewritten)
        result['cases'].append('binary custom backing, strict text rejection, direct backing refresh, actual CONFIG REWRITE')
        assert q(a+'.cancel','1')==b'OK'
        error('MODULE','UNLOAD',a)
        assert check(a)[2]==3
        assert 'F11_CLOSE_BEGIN cfgowners ' not in log.read_text(errors="replace")
        assert q(a+'.cancel','0')==b'OK'
        assert q('MODULE','LOAD',pb)==b'OK';check(b)
        unload_position=len(log.read_text(errors='replace'))
        assert q('MODULE','UNLOAD',a)==b'OK'
        assert 'F11_REPLY_RELEASE cfgowners live=0' in log.read_text(errors="replace")[unload_position:]
        assert q('CONFIG','GET',a+'.*')==[];check(b)
        assert q('MODULE','LOAD',pa)==b'OK';check(a)
        assert q('MODULE','UNLOAD',a)==b'OK';assert q('MODULE','UNLOAD',b)==b'OK'
        assert 'F11_REPLY_RELEASE cfgowners_b live=0' in log.read_text(errors="replace")
        result['cases'].append('canceled/successful unload, independent images, permitted reload, final cached-owner release')
        for mode in ['fail','duplicate']:
            position=len(log.read_text(errors="replace"))
            error('MODULE','LOAD',pa,mode)
            tail=log.read_text(errors="replace")[position:]
            assert 'F11_REPLY_RELEASE cfgowners live=0' in tail,tail
            if mode=='duplicate':assert 'F11_DUPLICATE_REJECTED true' in tail
            assert q('CONFIG','GET',a+'.*')==[]
            assert q('MODULE','LOAD',pa)==b'OK';check(a);assert q('MODULE','UNLOAD',a)==b'OK'
        result['cases'].append('failed OnLoad after read, duplicate registration rejection, pending guard cleanup/recovery')
        position=len(log.read_text(errors="replace"))
        error('MODULE','LOADEX',pa,'CONFIG',a+'.text','reject')
        tail=log.read_text(errors="replace")[position:]
        assert 'F11_CLOSE_BEGIN cfgowners ' not in tail,tail
        assert 'F19_MANUAL_LOAD_ERROR' in tail and 'F11_REPLY_RELEASE cfgowners live=0' in tail,tail
        assert q('CONFIG','GET',a+'.*')==[]
        assert q('MODULE','LOADEX',pa,'CONFIG',a+'.text','load-value')==b'OK'
        assert get(a+'.text')==b'load-value';check(a)
        assert q('MODULE','UNLOAD',a)==b'OK'
        result['cases'].append('ordinary rejected LOADEX returns OnLoad ERR with pending cleanup, configured LOADEX success and recovery')
        result['final_server_identity']=host.verify_server_info(q('INFO','server'),process.pid,version)
        result['status']='passed'
    except BaseException as e:
        result['status']='failed';result['error']=repr(e);raise
    finally:
        host.stop_process(process);result['server_exit']=process.returncode;save()
    # Start from the actual rewritten file in a new owned process; prove queued
    # startup values and unload cleanup independently of runtime MODULE LOAD.
    startup = subprocess.Popen([str(binary),str(out/'rewritten.conf')],stdout=(out/'startup-stdout.log').open('w'),stderr=subprocess.STDOUT,start_new_session=True)
    try:
        until=time.monotonic()+8
        while True:
            if startup.poll() is not None:raise AssertionError('Owned startup-config server exited')
            try:info=q('INFO','server');break
            except (OSError,host.BaselineError):
                if time.monotonic()>=until:raise
                time.sleep(.03)
        result['startup_server_identity']=host.verify_server_info(info,startup.pid,version)
        for key,value in before.items():assert get(a+'.'+key)==value
        check(a)
        position=len(log.read_text(errors="replace"))
        unload_position=len(log.read_text(errors='replace'))
        assert q('MODULE','UNLOAD',a)==b'OK'
        assert 'F11_REPLY_RELEASE cfgowners live=0' in log.read_text(errors="replace")[unload_position:]
        result['cases'].append('actual rewritten-file startup queue, valid native backing before comparison, unload cleanup')
    except BaseException as e:
        result['status']='failed';result['error']=repr(e);raise
    finally:
        host.stop_process(startup);result['startup_server_exit']=startup.returncode;save()
    return result

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server',action='append',required=True,help='VERSION=PATH')
    parser.add_argument('--profile',action='append',choices=['debug','release'],required=True)
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args();out=args.output.resolve();out.mkdir(parents=True,exist_ok=False)
    before=capture_sources();report={'status':'running','source_before':before,'sdk_head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),'sdk_status':subprocess.check_output(['git','status','--short'],cwd=ROOT,text=True),'rustc':subprocess.check_output(['rustc','-vV'],cwd=ROOT,text=True),'cells':[]}
    try:
        for profile in args.profile:
            for server in args.server:
                version,path=server.split('=',1);binary=Path(path).resolve(strict=True)
                assert 'v='+version+' ' in subprocess.check_output([str(binary),'--version'],text=True)
                report['cells'].append(one_cell(binary,version,profile,out/(profile+'-'+version)))
                host.write_json(out/'report.json',report)
        report['source_after']=capture_sources();assert before==report['source_after']
        report['status']='passed'
    except BaseException as e:report['status']='failed';report['error']=repr(e);raise
    finally:host.write_json(out/'report.json',report)
if __name__=='__main__':main()
