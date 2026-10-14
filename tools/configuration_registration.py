#!/usr/bin/env python3
"""Corrected F13 registration checks on owned loopback Redis hosts."""
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
MODES = ['success', 'macro_numeric_name', 'macro_numeric_flags',
         'macro_bool_duplicate', 'macro_enum_duplicate'] + [
             f'direct_{kind}_{reason}' for kind in ['numeric', 'bool', 'enum']
             for reason in ['duplicate', 'name', 'flags']]


def sources():
    selected = ['Cargo.toml', 'Cargo.lock', 'src/configuration.rs',
                'src/configuration/registration.rs', 'src/macros.rs',
                'src/configuration/string_lifecycle.rs',
                'examples/configuration_registration.rs',
                'tools/configuration_registration.py']
    return {p: host.sha256(ROOT / p) for p in selected}


def cell(binary, version, profile, mode, out):
    out.mkdir(parents=True, exist_ok=False)
    suffix = 'dylib' if sys.platform == 'darwin' else 'so'
    module = ROOT / 'target' / profile / ('examples/libconfiguration_registration.' + suffix)
    with socket.socket() as reservation:
        reservation.bind(('127.0.0.1', 0))
        port = reservation.getsockname()[1]
    log = out / 'redis.log'
    conf = out / 'redis.conf'
    conf.write_text(f'bind 127.0.0.1\nport {port}\nsave ""\nappendonly no\nenable-module-command yes\ndir {out}\nlogfile {log}\n')
    env = dict(os.environ, F13_MODE=mode)
    result = {'profile': profile, 'host': version, 'mode': mode,
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
    def get(name): return q('CONFIG', 'GET', 'cfgregistration.' + name)[1]
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
                expected = ('numeric-macro' if 'macro_numeric' in mode else
                            'bool-macro' if mode == 'macro_bool_duplicate' else
                            'enum-macro' if mode == 'macro_enum_duplicate' else
                            mode.split('_')[1] + '-direct')
                assert segment.count('F13_DROP ' + expected) == 1, segment
                assert 'configuration registration failed:' in segment, segment
                expected_errno = errno.EALREADY if mode.endswith('duplicate') else errno.EINVAL
                assert f'(os error {expected_errno})' in segment, segment
                assert 'F13_UNEXPECTED_ACCEPTANCE' not in segment, segment
                if mode.startswith('direct_'):
                    assert f'F13_REJECTED {mode} drops=1' in segment, segment
                    assert 'F13_RELEASE live=0 issued=1 released=1' in segment, segment
                # Macro bool/enum rejects after accepted strings, before defaults/
                # user init can issue a getter reply. Zero snapshots is legal.
                assert q('CONFIG', 'GET', 'cfgregistration.*') == []
                module_names = [dict(zip(row[::2], row[1::2]))[b'name'] for row in q('MODULE', 'LIST')]
                assert b'cfgregistration' not in module_names, module_names
                assert q('PING') == b'PONG'
                result['cases'].append({'attempt': attempt, 'rejected_callback_drops': 1,
                                        'observed_reply_cleanup': '1/1' if mode.startswith('direct_') else 'zero snapshots before init',
                                        'result': 'passed'})
        else:
            assert q('MODULE', 'LOAD', module) == b'OK'
            stats = q('cfgregistration.stats')
            assert stats == [3, 0, 1, 0, 1, 1], stats
            assert [get(n) for n in ['number', 'toggle', 'choice', 'text']] == [b'7', b'yes', b'One', b'default']
            before = q('cfgregistration.stats')[0]
            assert q('CONFIG', 'SET', 'cfgregistration.number', '9') == b'OK'
            assert q('cfgregistration.stats')[0] == before + 1
            assert q('CONFIG', 'SET', 'cfgregistration.number', '9') == b'OK'
            assert q('cfgregistration.stats')[0] == before + 1
            error('CONFIG', 'SET', 'cfgregistration.number', '13')
            error('CONFIG', 'SET', 'cfgregistration.number', '101')
            assert get('number') == b'9'
            assert q('CONFIG', 'SET', 'cfgregistration.toggle', 'no') == b'OK'
            assert get('toggle') == b'no'
            assert q('CONFIG', 'SET', 'cfgregistration.choice', 'Two') == b'OK'
            assert get('choice') == b'Two' and q('cfgregistration.stats')[5] == 2
            assert q('CONFIG', 'REWRITE') == b'OK'
            assert get('number') == b'9' and get('choice') == b'Two'
            position = len(log.read_text(errors='replace'))
            assert q('MODULE', 'UNLOAD', 'cfgregistration') == b'OK'
            assert 'F13_RELEASE live=0' in log.read_text(errors='replace')[position:]
            assert q('MODULE', 'LOAD', module, 'number', '11', 'toggle', 'no', 'choice', 'Two') == b'OK'
            assert [get(n) for n in ['number', 'toggle', 'choice']] == [b'11', b'no', b'Two']
            position = len(log.read_text(errors='replace'))
            assert q('MODULE', 'UNLOAD', 'cfgregistration') == b'OK'
            assert 'F13_RELEASE live=0' in log.read_text(errors='replace')[position:]
            result['cases'] = ['typed defaults, custom setter rejection, native range rejection',
                               'one callback on change, no callback on same value',
                               'enum text mapping and typed integer getter',
                               'REWRITE and ordinary native unload/reload/module arguments']
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
                    report['cells'].append(cell(binary, version, profile, mode, out / (profile + '-' + version + '-' + mode)))
                    host.write_json(out / 'report.json', report)
        report['sources_after'] = sources(); assert before == report['sources_after']; report['status'] = 'passed'
    except BaseException as e:
        report['status'] = 'failed'; report['error'] = repr(e); raise
    finally: host.write_json(out / 'report.json', report)
    print(f"Passed {len(report['cells'])} owned host/profile/mode cells")


if __name__ == '__main__': main()
