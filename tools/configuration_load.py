#!/usr/bin/env python3
"""Corrected F19 load-result/order checks on owned loopback native hosts."""
import argparse
import contextlib
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import time
import package_baseline as host
import configuration_registration as registration

ROOT = Path(__file__).resolve().parents[1]
FAILURE = 'Module configuration loading failed; see Redis server log'


def sources():
    paths = ['Cargo.toml', 'Cargo.lock', 'src/configuration.rs', 'src/macros.rs',
             'src/configuration/string_lifecycle.rs', 'examples/configuration_load.rs',
             'examples/configuration_load_empty.rs', 'examples/support/configuration_ownership.rs',
             'examples/configuration_builders.rs', 'tools/configuration_load.py',
             'tools/package_baseline.py', 'tools/configuration_registration.py']
    return {p: host.sha256(ROOT / p) for p in paths}


@contextlib.contextmanager
def owned(binary, version, out, startup=''):
    out.mkdir(parents=True, exist_ok=False)
    with socket.socket() as reservation:
        reservation.bind(('127.0.0.1', 0)); port = reservation.getsockname()[1]
    log = out / 'redis.log'; conf = out / 'redis.conf'
    conf.write_text(f'bind 127.0.0.1\nport {port}\nsave ""\nappendonly no\n'
                    f'enable-module-command yes\ndir {out}\nlogfile {log}\n' + startup)
    def q(*args):
        return host.redis_command(port, *(a if isinstance(a, bytes) else str(a).encode() for a in args))
    with (out / 'stdout.log').open('w') as stream:
        process = subprocess.Popen([str(binary), str(conf)], start_new_session=True,
                                   stdout=stream, stderr=subprocess.STDOUT)
        try:
            until = time.monotonic() + 8
            while True:
                if process.poll() is not None: raise AssertionError('Owned host exited during startup')
                try: info = q('INFO', 'server'); break
                except (OSError, host.BaselineError):
                    if time.monotonic() >= until: raise
                    time.sleep(.03)
            identity = host.verify_server_info(info, process.pid, version)
            (out / 'server-info.txt').write_bytes(info)
            host.write_json(out / 'identity.json', {'pid': process.pid, 'info': identity,
                            'binary_sha256': host.sha256(binary), 'config_sha256': host.sha256(conf)})
            yield q, log, identity
        finally:
            host.stop_process(process)
            host.write_json(out / 'process-exit.json', {'actual_exit': process.returncode})


def rejected(q, *args):
    try: q(*args)
    except host.BaselineError as error:
        assert str(error).startswith('Redis error:'), error
        return str(error)
    raise AssertionError('Expected ordinary Redis command rejection')


def absent(q, module):
    assert q('CONFIG', 'GET', module + '.*') == []
    names = [dict(zip(row[::2], row[1::2]))[b'name'] for row in q('MODULE', 'LIST')]
    assert module.encode() not in names
    assert q('PING') == b'PONG'


def cell(binary, version, profile, out):
    suffix = 'dylib' if sys.platform == 'darwin' else 'so'
    modules = {n: ROOT / 'target' / profile / ('examples/libconfiguration_' + n + '.' + suffix)
               for n in ['load', 'load_empty', 'ownership']}
    row = {'host': version, 'profile': profile, 'status': 'running', 'cases': [],
           'modules': {n: {'path': str(p), 'sha256': host.sha256(p)} for n,p in modules.items()}}
    out.mkdir(parents=True, exist_ok=False)
    try:
        with owned(binary, version, out / 'main') as (q, log, identity):
            row['identity'] = identity
            macro, manual = modules['load'], modules['ownership']
            position = len(log.read_text(errors='replace'))
            assert q('MODULE', 'LOAD', macro) == b'OK'
            segment = log.read_text(errors='replace')[position:]
            events = re.findall(r'F19_(?:CHANGED|SET|MACRO_INIT)[^\n]*', segment)
            assert events == ['F19_CHANGED number', 'F19_SET text', 'F19_SET guard', 'F19_CHANGED mode', 'F19_MACRO_INIT'], events
            assert q('loadconfigs.state') == [7, b'ready', 1, 1]
            assert q('loadconfigs.phase') == FAILURE.encode()
            for name,value in [('number','9'),('mode','Two'),('text','héllo')]:
                assert q('CONFIG','SET','loadconfigs.'+name,value) == b'OK'
            assert q('loadconfigs.state') == [9, 'héllo'.encode(), 1, 2]
            rejected(q, 'CONFIG', 'SET', 'loadconfigs.text', b'\xff')
            assert q('loadconfigs.state')[1] == 'héllo'.encode()
            assert q('loadconfigs.refuse','yes') == b'OK'
            rejected(q, 'MODULE', 'UNLOAD', 'loadconfigs')
            assert q('loadconfigs.state')[0] == 9
            assert q('loadconfigs.refuse','no') == b'OK'
            assert q('MODULE','UNLOAD','loadconfigs') == b'OK'
            absent(q,'loadconfigs')
            row['cases'].append({'name':'macro defaults/order/custom setters/enum/strict text and real command-phase rejection; canceled/successful unload','status':'passed','events':events})
            assert q('MODULE','LOADEX',macro,'CONFIG','loadconfigs.text','queued','CONFIG','loadconfigs.number','21','CONFIG','loadconfigs.mode','Two') == b'OK'
            assert q('loadconfigs.state') == [21,b'queued',1,2]
            assert q('MODULE','UNLOAD','loadconfigs') == b'OK'
            for args in [('LOADEX', macro, 'CONFIG','loadconfigs.text','valid','CONFIG','loadconfigs.guard','no'),
                         ('LOAD',macro,'text','reject')]:
                position = len(log.read_text(errors='replace')); rejected(q,'MODULE',*args)
                segment=log.read_text(errors='replace')[position:]
                assert FAILURE in segment and 'F19_MACRO_INIT' not in segment and 'F19_MACRO_DEINIT' not in segment,segment
                assert 'F19_SET text' in segment
                if args[0]=='LOADEX': assert 'F19_GET text' in segment and 'F19_SET guard' in segment
                absent(q,'loadconfigs')
                assert q('MODULE','LOAD',macro)==b'OK'
                assert q('MODULE','UNLOAD','loadconfigs')==b'OK'
            row['cases'].append({'name':'queued/default native rejection skips user init/deinit, clears configs/module, later PING and legal reload','status':'passed'})
            for default in [False,True]:
                position=len(log.read_text(errors='replace'))
                args=('LOAD',manual,'load-guard-default') if default else ('LOADEX',manual,'CONFIG','cfgowners.text','valid','CONFIG','cfgowners.guard','no','ARGS','load-guard')
                rejected(q,'MODULE',*args);segment=log.read_text(errors='replace')[position:]
                matches=re.findall(r'F19_MANUAL_LOAD_ERROR issued=(\d+) live=(\d+)',segment)
                expected=[('0','0')] if default else [('1','1')]
                assert matches==expected,segment
                assert 'F19_MANUAL_LOAD_OK' not in segment and 'F11_CLOSE_BEGIN' not in segment and FAILURE in segment,segment
                if not default: assert 'F11_REPLY_RELEASE cfgowners live=0' in segment,segment
                absent(q,'cfgowners')
                assert q('MODULE','LOAD',manual)==b'OK'
                issued,released,live,changed=q('cfgowners.stats');assert issued-released==live==3
                assert q('MODULE','UNLOAD','cfgowners')==b'OK'
                row['cases'].append({'name':'manual default rejection zero snapshots' if default else 'manual queued string before later boolean rejection; one issued snapshot closed on pending guard','status':'passed','issued_live_before_return':matches})
            position=len(log.read_text(errors='replace'))
            assert q('MODULE','LOAD',modules['load_empty'])==b'OK'
            assert log.read_text(errors='replace')[position:].count('F19_EMPTY_INIT')==1
            assert q('MODULE','UNLOAD','cfgempty')==b'OK';absent(q,'cfgempty')
            row['cases'].append({'name':'present-empty macro block and absent-block manual first-call controls','status':'passed'})
        startup=f'loadmodule {modules["load"]}\nloadconfigs.number 23\nloadconfigs.text startup\nloadconfigs.mode Two\n'
        with owned(binary,version,out/'startup',startup) as (q,log,identity):
            assert q('loadconfigs.state')==[23,b'startup',1,2]
            assert 'F19_GET text' in log.read_text(errors='replace')
            assert q('MODULE','UNLOAD','loadconfigs')==b'OK';absent(q,'loadconfigs')
            row['startup_identity']=identity
            row['cases'].append({'name':'actual startup configuration queue; selected custom/default/enum values','status':'passed'})
        # Existing corrected F13 fixture rejects numeric registration before any
        # load/initializer or accounting-hook installation. Native phase is OnLoad.
        row['registration_control']=registration.cell(binary,version,profile,'macro_numeric_name',out/'registration')
        row['status']='passed'
    except BaseException as error:
        row['status']='failed';row['error']=repr(error);raise
    finally: host.write_json(out/'report.json',row)
    return row


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--server',action='append',required=True,help='VERSION=PATH')
    p.add_argument('--profile',action='append',choices=['debug','release'],required=True)
    p.add_argument('--output',type=Path,required=True);args=p.parse_args()
    out=args.output.resolve();out.mkdir(parents=True,exist_ok=False)
    report={'status':'running','sources_before':sources(),'cells':[]}
    try:
        for profile in args.profile:
            for entry in args.server:
                version,path=entry.split('=',1);binary=Path(path).resolve(strict=True)
                assert 'v='+version+' ' in subprocess.check_output([str(binary),'--version'],text=True)
                report['cells'].append(cell(binary,version,profile,out/(profile+'-'+version)))
                host.write_json(out/'report.json',report)
        report['sources_after']=sources();assert report['sources_before']==report['sources_after'];report['status']='passed'
    except BaseException as error:report['status']='failed';report['error']=repr(error);raise
    finally:host.write_json(out/'report.json',report)
if __name__=='__main__':main()
