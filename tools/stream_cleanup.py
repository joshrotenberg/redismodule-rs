#!/usr/bin/env python3
"""F23 corrected stream checks on isolated owned native hosts; no old-code run."""
import argparse
import json
from pathlib import Path
import sys

import package_baseline as host
from call_reply import Wire
from configuration_load import owned, rejected

ROOT = Path(__file__).resolve().parents[1]
INPUTS = [(b'1-0', [b'f\0\xff', b'v\0\xfe']),
          (b'2-0', ['é'.encode(), b'\r\n']), (b'3-0', [b'', b''])]


def cell(binary, version, profile, out):
    suffix = 'dylib' if sys.platform == 'darwin' else 'so'
    modules = {name: ROOT / 'target' / profile / f'examples/lib{name}.{suffix}'
               for name in ['stream_cleanup', 'stream', 'string_ownership']}
    row = {'host': version, 'profile': profile, 'status': 'running', 'cases': [],
           'modules': {n: {'path': str(p), 'sha256': host.sha256(p)} for n,p in modules.items()}}
    out.mkdir(parents=True, exist_ok=False)
    try:
        with owned(binary, version, out / 'host') as (_, log, identity):
            row['identity'] = identity
            config = (out / 'host/redis.conf').read_text().splitlines()
            wire = Wire(int(next(line.split()[1] for line in config if line.startswith('port '))))
            try:
                q = wire.query
                for module in modules.values(): assert q('MODULE', 'LOAD', module) == b'OK'
                for key in ['stream:read', 'stream:write']:
                    for id_, fields in INPUTS: assert q('XADD', key, id_, *fields) == id_
                baseline = [[id_, fields] for id_,fields in INPUTS]
                for mode in ['zero', 'one', 'all', 'early', 'second', 'writable']:
                    key = 'stream:write' if mode == 'writable' else 'stream:read'
                    result, events = q('streamcheck.read', key, mode, 0, 0, 0)
                    selected = [] if mode in ['zero', 'early'] else baseline[:1] if mode in ['one','second'] else baseline
                    assert result == [selected, baseline], (mode, result)
                    wanted = [b'start'] + ([b'rejected'] if mode == 'second' else []) + [b'stop', b'start', b'stop', b'close']
                    assert events == wanted, (mode,events)
                    assert q('XRANGE', key, '-', '+') == baseline
                    assert q('PING') == b'PONG'
                    row['cases'].append({'name': mode + ': same-handle restart, nonmutation, Stop-before-Close', 'events':[e.decode() for e in events]})
                for reverse in [0,1]:
                    for exclusive in [0,1]:
                        result, events = q('streamcheck.read', 'stream:read', 'all', reverse, exclusive, 1)
                        selected = baseline[1:2] if exclusive else baseline
                        if reverse: selected = selected[::-1]
                        assert result == [selected, baseline]
                        assert events == [b'start',b'stop',b'start',b'stop',b'close']
                        row['cases'].append({'name':f'range reverse={reverse} exclusive={exclusive}; exact binary bytes'})
                assert q('SET','stream:wrong','text') == b'OK'
                for key in ['stream:missing','stream:wrong']:
                    result, events = q('streamcheck.read',key,'all',0,0,0)
                    assert result == b'Failed creating stream iterator', result
                    assert events == ([b'rejected'] if key.endswith('missing') else [b'rejected',b'close']), events
                    assert q('PING') == b'PONG'
                    row['cases'].append({'name':key+' Start rejected without Stop','events':[e.decode() for e in events]})
                assert q('XADD','stream:empty','1-0','f','v') == b'1-0'
                assert q('XDEL','stream:empty','1-0') == 1
                assert q('TYPE','stream:empty') == b'stream'
                result, events = q('streamcheck.read','stream:empty','all',0,0,0)
                assert result == [[],[]]
                assert events == [b'start',b'stop',b'start',b'stop',b'close']
                row['cases'].append({'name':'existing-empty traversal/restart','events':[e.decode() for e in events]})
                # Actual corrected example, not a reimplementation of its handler.
                assert 'WRONGTYPE' in rejected(q,'STREAM_POP','pop:missing')
                assert 'WRONGTYPE' in rejected(q,'STREAM_POP','stream:wrong')
                assert q('STREAM_POP','stream:empty') is None
                assert q('XLEN','stream:empty') == 0
                assert q('TYPE','stream:empty') == b'stream'
                assert q('XADD','pop:one','7-0','f','v') == b'7-0'
                assert q('STREAM_POP','pop:one') == b'7-0'
                assert q('XLEN','pop:one') == 0
                for id_, fields in INPUTS: assert q('XADD','pop:many',id_,*fields) == id_
                assert q('STREAM_POP','pop:many') == b'1-0'
                assert q('XRANGE','pop:many','-','+') == baseline[1:]
                assert q('STREAM_POP','pop:many') == b'2-0'
                assert q('XRANGE','pop:many','-','+') == baseline[2:]
                assert q('STREAM_POP','pop:many') == b'3-0'
                assert q('XLEN','pop:many') == 0
                assert q('PING') == b'PONG'
                row['cases'].append({'name':'actual STREAM_POP missing/wrong/empty/one/many, exact remaining IDs, later same-connection PING'})
                # Existing F09 fixture: ownership survives iterator/key/command return.
                expected = [value for _, fields in INPUTS for value in fields]
                for am in [0,1]:
                    assert q('owner.store','stream',am,'stream:read') == len(expected)
                    assert q('owner.read') == expected
                    assert q('owner.clear') == b'OK'
                    assert q('owner.read') == []
                    assert q('PING') == b'PONG'
                    row['cases'].append({'name':f'existing F09 stream owners after command return AM={am}'})
                # Only this fixture is unloaded: F09 has a registered data type.
                assert q('MODULE','UNLOAD','stream_cleanup') == b'OK'
                assert q('MODULE','LOAD',modules['stream_cleanup']) == b'OK'
                result, events=q('streamcheck.read','stream:read','one',0,0,0)
                assert result == [baseline[:1],baseline]
                assert events == [b'start',b'stop',b'start',b'stop',b'close']
                assert q('PING') == b'PONG'
                row['cases'].append({'name':'legal fresh fixture reload'})
            finally: wire.close()
        row['status'] = 'passed'
    except Exception as error:
        row['status'] = 'failed'; row['error'] = repr(error); raise
    finally: host.write_json(out/'result.json',row)
    return row


def main():
    p=argparse.ArgumentParser();p.add_argument('--output',type=Path,required=True);args=p.parse_args()
    out=args.output.resolve();out.mkdir(parents=True,exist_ok=False)
    report={'sources':{name:host.sha256(ROOT/name) for name in ['src/stream.rs','src/key.rs','examples/stream.rs','examples/stream_cleanup.rs','examples/string_ownership.rs','tools/stream_cleanup.py','tools/call_reply.py','tools/configuration_load.py','tools/package_baseline.py','Cargo.toml','Cargo.lock']},'cells':[], 'limits':['Counts issued API calls, not heap/refcount accounting.','Selected macOS arm64/libc hosts/profiles; no support or throughput claim.','Actual example ordering is source scope evidence plus real output; its APIs are not instrumented.']}
    try:
        for version,binary in [('7.2.8',ROOT/'target/f01/servers/redis-7.2.8/src/redis-server'),('8.10.2',Path('/opt/homebrew/Cellar/redis/8.10.2/bin/redis-server'))]:
            for profile in ['debug','release']:
                row=cell(binary,version,profile,out/f'{version}-{profile}');report['cells'].append(row)
                print(version,profile,len(row['cases']),'cases passed',flush=True)
    finally: host.write_json(out/'report.json',report)

if __name__=='__main__':main()
