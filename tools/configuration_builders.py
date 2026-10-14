#!/usr/bin/env python3
"""Paired F14 builder/free registration checks on owned loopback Redis hosts."""
import argparse
import errno
import sys
import json
import os
from pathlib import Path
import socket
import subprocess
import time
import package_baseline as host

ROOT = Path(__file__).resolve().parents[1]
MODES = ['success', 'integer_duplicate', 'integer_flags', 'boolean_name', 'boolean_flags', 'enum_flags', 'string_duplicate', 'string_flags']


def sources():
    selected = ['Cargo.toml', 'Cargo.lock', 'src/configuration.rs',
                'src/configuration/config.rs', 'src/macros.rs',
                'src/configuration/string_lifecycle.rs',
                'examples/configuration_builders.rs',
                'tools/configuration_builders.py']
    return {p: host.sha256(ROOT / p) for p in selected}


def cell(binary, version, profile, mode, backend, out):
    out.mkdir(parents=True, exist_ok=False)
    suffix = 'dylib' if sys.platform == 'darwin' else 'so'
    module = ROOT / 'target' / profile / ('examples/libconfiguration_builders.' + suffix)
    with socket.socket() as reservation:
        reservation.bind(('127.0.0.1', 0))
        port = reservation.getsockname()[1]
    log = out / 'redis.log'
    conf = out / 'redis.conf'
    conf.write_text(f'bind 127.0.0.1\nport {port}\nsave ""\nappendonly no\nenable-module-command yes\ndir {out}\nlogfile {log}\n')
    env = dict(os.environ, F14_MODE=mode, F14_BACKEND=backend)
    result = {'profile': profile, 'host': version, 'mode': mode, 'backend': backend,
              'module_sha256': host.sha256(module), 'server_sha256': host.sha256(binary),
              'cases': [], 'status': 'running'}
    def save(): host.write_json(out / 'report.json', result)
    def q(*args): return host.redis_command(port, *(a if isinstance(a, bytes) else str(a).encode() for a in args))
    def error(*args):
        try: q(*args)
        except host.BaselineError as e:
            assert str(e).startswith('Redis error:'), e
            return str(e)
        raise AssertionError('Expected ordinary Redis command rejection')
    def get(name): return q('CONFIG', 'GET', 'cfgbuilders.' + name)[1]
    process = subprocess.Popen([str(binary), str(conf)], env=env,
                               stdout=(out / 'stdout.log').open('w'),
                               stderr=subprocess.STDOUT, start_new_session=True)
    save()
    try:
        until = time.monotonic() + 8
        while True:
            if process.poll() is not None: raise AssertionError('Owned server exited during startup')
            try: info = q('INFO', 'server'); break
            except (OSError, host.BaselineError):
                if time.monotonic() >= until: raise
                time.sleep(.03)
        result['identity'] = host.verify_server_info(info, process.pid, version)
        (out / 'server-info.txt').write_bytes(info)
        if mode != 'success':
            # Rejection, repeated on a second load of the same isolated names.
            for attempt in range(2):
                position = len(log.read_text(errors='replace'))
                error('MODULE', 'LOAD', module)
                segment = log.read_text(errors='replace')[position:]
                expected = ('enumeration' if mode.startswith('enum') else mode.split('_')[0])
                assert segment.count('F14_DROP ' + expected) == 1, segment
                assert 'configuration registration failed:' in segment, segment
                expected_errno = errno.EALREADY if mode.endswith('duplicate') else errno.EINVAL
                assert f'(os error {expected_errno})' in segment, segment
                assert 'F14_UNEXPECTED_ACCEPTANCE' not in segment, segment
                assert f'F14_REJECTED {mode} drops=1' in segment, segment
                assert 'F14_RELEASE live=0 issued=1 released=1' in segment, segment
                assert q('CONFIG', 'GET', 'cfgbuilders.*') == []
                module_names = [dict(zip(row[::2], row[1::2]))[b'name'] for row in q('MODULE', 'LIST')]
                assert b'cfgbuilders' not in module_names, module_names
                assert q('PING') == b'PONG'
                result['cases'].append({'attempt': attempt, 'rejected_callback_drops': 1,
                                        'observed_reply_cleanup': '1/1',
                                        'result': 'passed'})
        else:
            assert q('MODULE', 'LOAD', module) == b'OK'
            stats = q('cfgbuilders.stats')
            assert stats == [4, 0, 1, 0, 1, 1], stats
            assert [get(n) for n in ['number', 'toggle', 'choice', 'text']] == [b'7', b'yes', b'One', b'default']
            before = q('cfgbuilders.stats')[0]
            assert q('CONFIG', 'SET', 'cfgbuilders.number', '9') == b'OK'
            assert q('cfgbuilders.stats')[0] == before + 1
            assert q('CONFIG', 'SET', 'cfgbuilders.number', '9') == b'OK'
            assert q('cfgbuilders.stats')[0] == before + 1
            error('CONFIG', 'SET', 'cfgbuilders.number', '13')
            error('CONFIG', 'SET', 'cfgbuilders.number', '101')
            assert get('number') == b'9'
            assert q('CONFIG', 'SET', 'cfgbuilders.toggle', 'no') == b'OK'
            assert get('toggle') == b'no'
            assert q('CONFIG', 'SET', 'cfgbuilders.choice', 'Two') == b'OK'
            assert get('choice') == b'Two' and q('cfgbuilders.stats')[5] == 2
            assert q('CONFIG', 'SET', 'cfgbuilders.text', 'héllo %') == b'OK'
            assert get('text') == 'héllo %'.encode()
            error('CONFIG', 'SET', 'cfgbuilders.text', b'\xff')
            assert get('text') == 'héllo %'.encode()
            assert get('locked') == b'fixed'
            error('CONFIG', 'SET', 'cfgbuilders.locked', 'other')
            assert q('cfgbuilders.late') == b'String configuration registration requires its active load scope'
            assert q('cfgbuilders.stats')[1] == 1
            result['observations'] = {'defaults': [7, 'yes', 'One', 'default'], 'number': get('number').decode(), 'toggle': get('toggle').decode(), 'choice': get('choice').decode(), 'text': get('text').decode(), 'locked': get('locked').decode(), 'stats': q('cfgbuilders.stats')}
            assert q('CONFIG', 'REWRITE') == b'OK'
            assert get('number') == b'9' and get('choice') == b'Two'
            position = len(log.read_text(errors='replace'))
            assert q('MODULE', 'UNLOAD', 'cfgbuilders') == b'OK'
            assert 'F14_RELEASE live=0' in log.read_text(errors='replace')[position:]
            assert q('MODULE', 'LOAD', module) == b'OK'
            assert [get(n) for n in ['number', 'toggle', 'choice']] == [b'7', b'yes', b'One']
            position = len(log.read_text(errors='replace'))
            assert q('MODULE', 'UNLOAD', 'cfgbuilders') == b'OK'
            assert 'F14_RELEASE live=0' in log.read_text(errors='replace')[position:]
            result['cases'] = ['typed defaults, custom setter rejection, native range rejection',
                               'one callback on change, no callback on same value',
                               'enum text mapping and typed integer getter',
                               'REWRITE, strict UTF8, immutable flag, post-commit admission, ordinary unload/reload']
        result['status'] = 'passed'
    except BaseException as e:
        result['status'] = 'failed'; result['error'] = repr(e); raise
    finally:
        host.stop_process(process); result['server_exit'] = process.returncode; save()
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server', action='append', required=True)
    parser.add_argument('--profile', action='append', choices=['debug', 'release'], required=True)
    parser.add_argument('--mode', action='append', choices=MODES)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args(); out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    before = sources(); report = {'status': 'running', 'sources_before': before,
                                 'sdk_head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                                 'sdk_status': subprocess.check_output(['git', 'status', '--short'], cwd=ROOT, text=True), 'cells': []}
    try:
        for profile in args.profile:
            for server in args.server:
                version, path = server.split('=', 1); binary = Path(path).resolve(strict=True)
                assert 'v=' + version + ' ' in subprocess.check_output([str(binary), '--version'], text=True)
                for mode in args.mode or MODES:
                    pairs = []
                    for backend in ['free', 'builder']:
                        row = cell(binary, version, profile, mode, backend, out / (profile + '-' + version + '-' + mode + '-' + backend))
                        pairs.append(row)
                        report['cells'].append(row)
                        host.write_json(out / 'report.json', report)
                    if mode == 'success':
                        assert pairs[0]['observations'] == pairs[1]['observations'], pairs
        report['sources_after'] = sources(); assert before == report['sources_after']; report['status'] = 'passed'
    except BaseException as e:
        report['status'] = 'failed'; report['error'] = repr(e); raise
    finally: host.write_json(out / 'report.json', report)
    print(f"Passed {len(report['cells'])} owned host/profile/mode cells")


if __name__ == '__main__': main()
