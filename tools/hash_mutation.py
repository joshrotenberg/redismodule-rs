#!/usr/bin/env python3
"""Corrected F27 single-field counts/bytes on legal owned hosts; no field TTL."""
import argparse
from pathlib import Path
import package_baseline as host
from call_reply import Wire
from configuration_load import owned
ROOT = Path(__file__).resolve().parents[1]


def cell(binary, version, profile, out):
    module = ROOT / 'target' / profile / 'examples/libhash_mutation.dylib'
    row = {'version': version, 'profile': profile, 'module_sha256': host.sha256(module), 'cases': []}
    out.mkdir(parents=True, exist_ok=False)
    try:
        with owned(binary, version, out / 'host') as (_, _, identity):
            row['identity'] = identity
            port = int(next(x.split()[1] for x in (out/'host/redis.conf').read_text().splitlines() if x.startswith('port ')))
            wire = Wire(port)
            try:
                q = wire.query
                def mutate(mode, key, field, *value): return q('hashmutation.run', mode, key, field, *value)
                def reject(mode, key, field, *value, expect):
                    try: mutate(mode, key, field, *value)
                    except Exception as error:
                        assert expect in str(error), error
                        return str(error)
                    raise AssertionError('expected ordinary error')
                assert q('MODULE', 'LOAD', module) == b'OK'
                # Small entries select packed encoding; 100-byte values force hashtable.
                for large in [False, True]:
                    for i, field in enumerate([b'', 'λ'.encode(), b'a\0b', b'\xff\xfe']):
                        key = f'hash:{large}:{i}'
                        values = [b'', 'héllo'.encode(), b'v\0\r\n', b'\xff\x00']
                        if large: values = [v + b'x'*100 for v in values]
                        assert mutate('del', key, field) == 0
                        assert q('EXISTS', key) == 0
                        for j, value in enumerate(values):
                            expected = int(j != 0)
                            assert mutate('set', key, field, value) == [expected, value, field, value]
                            assert mutate('set', key, field, value) == [1, value, field, value]
                            assert q('HGET', key, field) == value
                            assert q('HEXISTS', key, field) == 1 and q('HLEN', key) == 1
                        encoding = q('OBJECT', 'ENCODING', key)
                        assert encoding == (b'hashtable' if large else b'listpack'), encoding
                        assert mutate('del', key, b'absent') == 0
                        assert q('HLEN', key) == 1
                        assert mutate('del', key, field) == 1
                        assert q('EXISTS', key) == 0 and q('HGET', key, field) is None
                        assert mutate('del', key, field) == 0
                        assert q('PING') == b'PONG'
                        row['cases'].append({'field_hex':field.hex(), 'encoding':encoding.decode(), 'value_hex':[v.hex() for v in values], 'count_sequence':'insert0; replacement/same1; missing-delete0; last-delete1'})
                assert mutate('set','multiple',b'one',b'a')==[0,b'a',b'one',b'a']
                assert mutate('set','multiple',b'two',b'b')==[0,b'b',b'two',b'b']
                assert mutate('del','multiple',b'one')==1
                assert q('HLEN','multiple')==1 and q('HGET','multiple',b'two')==b'b'
                assert mutate('del','multiple',b'two')==1 and q('EXISTS','multiple')==0
                row['cases'].append({'name':'delete existing while retaining another field; delete last field'})
                key='text'
                for field in [b'', 'λ'.encode()]:
                    assert mutate('text-set',key,field,b'\0\xff') == [0,b'\0\xff',field,b'\0\xff']
                    assert mutate('text-set',key,field,b'other') == [1,b'other',field,b'other']
                    assert mutate('text-del',key,field)==1
                    assert mutate('text-del',key,field)==0
                reject('text-set',key,b'a\0b',b'value',expect='interior NUL'); reject('text-del',key,b'a\0b',expect='interior NUL')
                assert q('EXISTS',key)==0
                assert q('SET','wrong',b'original')==b'OK'
                for mode in ['set','text-set','del','text-del']:
                    reject(mode,'wrong',b'f',*([b'v'] if mode.endswith('set') else []),expect='WRONGTYPE')
                    assert q('GET','wrong')==b'original'
                assert q('PING')==b'PONG'
                row['cases'].append({'name':'consuming text setter, NUL rejection, wrong type, unchanged content'})
                assert q('MODULE','UNLOAD','hash_mutation')==b'OK'
                assert q('MODULE','LOAD',module)==b'OK'
                assert mutate('set','reload',b'\0',b'\xff')==[0,b'\xff',b'\0',b'\xff']
                assert q('PING')==b'PONG'
                assert q('MODULE','UNLOAD','hash_mutation')==b'OK'
                row['cases'].append({'name':'fresh legal reload and later PING'})
            finally: wire.close()
        row['status']='passed'
    except Exception as error:
        row['status']='failed'; row['error']=repr(error); raise
    finally: host.write_json(out/'result.json',row)
    return row


def main():
    parser=argparse.ArgumentParser(); parser.add_argument('--output',type=Path,required=True); args=parser.parse_args()
    out=args.output.resolve(); out.mkdir(parents=True,exist_ok=False)
    report={'cells':[], 'sources':{p:host.sha256(ROOT/p) for p in ['src/raw.rs','src/raw/hash.rs','src/key.rs','examples/hash_mutation.rs','tools/hash_mutation.py','Cargo.toml','Cargo.lock']}, 'limits':['No field expiry, replication, raw readonly write, allocator/refcount or general safety claim.','Selected macOS arm64/libc hosts, not support policy.','6.0.20 runtime gate recorded separately; no old path executed.']}
    try:
        for version,binary in [('7.2.8',ROOT/'target/f27/servers/redis-7.2.8/src/redis-server'),('8.10.2',Path('/opt/homebrew/Cellar/redis/8.10.2/bin/redis-server'))]:
            for profile in ['debug','release']:
                report['cells'].append(cell(binary,version,profile,out/f'{version}-{profile}'))
                print(version,profile,'passed',flush=True)
    finally: host.write_json(out/'report.json',report)
if __name__=='__main__': main()
