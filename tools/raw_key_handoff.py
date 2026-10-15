#!/usr/bin/env python3
"""F25 corrected same-phase transfers on isolated owned Redis hosts."""
import argparse
from pathlib import Path
import sys
import package_baseline as host
from call_reply import Wire
from configuration_load import owned

ROOT = Path(__file__).resolve().parents[1]
VALUE = b'owned\0\xff\r\n'


def cell(binary, version, profile, out):
    suffix = 'dylib' if sys.platform == 'darwin' else 'so'
    module = ROOT / 'target' / profile / f'examples/libraw_key_handoff.{suffix}'
    row = {'host': version, 'profile': profile, 'status': 'running', 'cases': [],
           'module': str(module), 'module_sha256': host.sha256(module)}
    out.mkdir(parents=True, exist_ok=False)
    try:
        with owned(binary, version, out / 'host') as (_, _, identity):
            row['identity'] = identity
            config = (out / 'host/redis.conf').read_text().splitlines()
            wire = Wire(int(next(line.split()[1] for line in config if line.startswith('port '))))
            try:
                q = wire.query
                assert q('MODULE', 'LOAD', module) == b'OK'
                for am in [0, 1]:
                    key = f'raw:{am}:string'
                    missing = f'raw:{am}:missing'
                    assert q('SET', key, VALUE) == b'OK'
                    assert q('EXISTS', missing) == 0
                    for mode, value, count in [('round', VALUE, 1), ('manual', 1, 1),
                                               ('early', b'early', 1), ('cycles', VALUE, 16),
                                               ('ordinary', VALUE, 1)]:
                        assert q('rawkey.check', key, mode, am) == [value, count, count, 0]
                        assert q('GET', key) == VALUE
                        assert q('PING') == b'PONG'
                        row['cases'].append({'name': f'{mode} AM={am}', 'opens': count,
                                             'manual_nonnull_closes': count, 'later_ping': True})
                    for mode, value, null_closes in [('round', None, 0), ('null-manual', 0, 1)]:
                        assert q('rawkey.check', missing, mode, am) == [value, 1, 0, null_closes]
                        assert q('EXISTS', missing) == 0
                        assert q('PING') == b'PONG'
                        row['cases'].append({'name': f'missing {mode} AM={am}', 'null_api_closes': null_closes})
                    # Select hashtable encodings, avoiding the untested template-hash family.
                    hash_key, set_key = f'raw:{am}:hash', f'raw:{am}:set'
                    pairs = [(b'f\0\xff', b'v' * 80), (b'other', b'\0\xfe')]
                    assert q('HSET', hash_key, *[x for pair in pairs for x in pair]) == 2
                    members = [b'a' * 80, b'b\0\xff' * 30]
                    assert q('SADD', set_key, *members) == 2
                    for scan_key, expected in [(hash_key, dict(pairs)), (set_key, dict.fromkeys(members))]:
                        encoding = q('OBJECT', 'ENCODING', scan_key)
                        assert encoding == b'hashtable', (version, scan_key, encoding)
                        rows, opens, closes, nulls = q('rawkey.check', scan_key, 'scan-key', am)
                        assert dict(rows) == expected
                        assert [opens, closes, nulls] == [1, 1, 0]
                        assert q('PING') == b'PONG'
                        row['cases'].append({'name': f'borrowed ScanKey {scan_key} AM={am}', 'encoding': encoding.decode()})
                    names, opens, closes, nulls = q('rawkey.check', key, 'scan-db', am)
                    assert {key.encode(), hash_key.encode(), set_key.encode()} <= set(names)
                    assert [opens, closes, nulls] == [0, 0, 0]
                    assert q('GET', key) == VALUE
                    assert q('HLEN', hash_key) == 2 and q('SCARD', set_key) == 2
                    assert q('PING') == b'PONG'
                    row['cases'].append({'name': f'borrowed stack Keys scan AM={am}', 'manual_closes': 0})
                assert q('MODULE', 'UNLOAD', 'raw_key_handoff') == b'OK'
                assert q('MODULE', 'LOAD', module) == b'OK'
                assert q('rawkey.check', 'raw:0:string', 'round', 1) == [VALUE, 1, 1, 0]
                assert q('PING') == b'PONG'
                row['cases'].append({'name': 'fresh legal reload'})
            finally:
                wire.close()
        row['status'] = 'passed'
    except Exception as error:
        row['status'] = 'failed'
        row['error'] = repr(error)
        raise
    finally:
        host.write_json(out / 'result.json', row)
    return row


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--output', type=Path, required=True)
    args = p.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    files = ['src/key.rs', 'src/context/key_cursor.rs', 'src/context/keys_cursor.rs',
             'examples/raw_key_handoff.rs', 'tools/raw_key_handoff.py', 'tools/call_reply.py',
             'tools/configuration_load.py', 'tools/package_baseline.py', 'Cargo.toml', 'Cargo.lock']
    report = {'sources': {f: host.sha256(ROOT / f) for f in files}, 'cells': [],
              'limits': ['Issued manual API calls, not internal AM collection/heap/refcounts.',
                         'Selected macOS arm64/libc 7.2.8/8.10.2 debug/release, not support policy.',
                         'No old-code execution or live handle across command return.',
                         'Scan callback views do not escape; actual selected hash/set encoding only.']}
    try:
        for version, binary in [('7.2.8', ROOT / 'target/f01/servers/redis-7.2.8/src/redis-server'),
                                ('8.10.2', Path('/opt/homebrew/Cellar/redis/8.10.2/bin/redis-server'))]:
            for profile in ['debug', 'release']:
                row = cell(binary, version, profile, out / f'{version}-{profile}')
                report['cells'].append(row)
                print(version, profile, len(row['cases']), 'case groups passed', flush=True)
    finally:
        host.write_json(out / 'report.json', report)


if __name__ == '__main__':
    main()
