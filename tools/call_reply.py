#!/usr/bin/env python3
"""Corrected F17 byte/framing checks on owned loopback Redis hosts."""
import argparse
import hashlib
import json
import socket
import subprocess
import sys
import time
from pathlib import Path

import package_baseline as host

ROOT = Path(__file__).resolve().parents[1]
INPUTS = [b'', b'ASCII', 'héllo'.encode(), b'a\0b', b'\r', b'\n', b'\r\n',
          b'\xff\0\xfe', b'x' * 4096 + b'\xff']


class Wire:
    """Use the existing RESP2 decoder, retaining its actual framing headers."""
    def __init__(self, port):
        self.socket = socket.create_connection(('127.0.0.1', port), timeout=5)
        self.reader = self.socket.makefile('rb')
        self.headers = []

    def readline(self):
        line = self.reader.readline()
        self.headers.append(line)
        return line

    def read(self, size):
        return self.reader.read(size)

    def query(self, *args):
        args = [a if isinstance(a, bytes) else str(a).encode() for a in args]
        self.headers = []
        self.socket.sendall(b'*' + str(len(args)).encode() + b'\r\n' + b''.join(
            b'$' + str(len(a)).encode() + b'\r\n' + a + b'\r\n' for a in args))
        return host.read_resp2(self)

    def close(self):
        self.reader.close()
        self.socket.close()


def sources():
    paths = ['Cargo.toml', 'Cargo.lock', 'src/redisvalue.rs', 'src/context/mod.rs',
             'src/context/call_reply.rs', 'src/configuration.rs', 'src/macros.rs',
             'examples/call.rs', 'examples/call_reply.rs', 'tools/call_reply.py',
             'tools/package_baseline.py']
    return {p: host.sha256(ROOT / p) for p in paths}


def cell(binary, version, profile, out):
    out.mkdir(parents=True, exist_ok=False)
    suffix = 'dylib' if sys.platform == 'darwin' else 'so'
    module = ROOT / 'target' / profile / ('examples/libcall_reply.' + suffix)
    with socket.socket() as reservation:
        reservation.bind(('127.0.0.1', 0))
        port = reservation.getsockname()[1]
    log = out / 'redis.log'
    conf = out / 'redis.conf'
    conf.write_text(f'bind 127.0.0.1\nport {port}\nsave ""\nappendonly no\n'
                    f'enable-module-command yes\ndir {out}\nlogfile {log}\n')
    row = {'host': version, 'profile': profile, 'module_sha256': host.sha256(module),
           'server_sha256': host.sha256(binary), 'cases': [], 'status': 'running'}
    process = subprocess.Popen([str(binary), str(conf)], start_new_session=True,
                               stdout=(out / 'stdout.log').open('w'), stderr=subprocess.STDOUT)
    wire = None

    def bulk(value, expected):
        assert value == expected, (value, expected)
        assert wire.headers[0] == b'$' + str(len(expected)).encode() + b'\r\n', wire.headers

    def later():
        assert wire.query('PING') == b'PONG'
        assert wire.headers == [b'+PONG\r\n'], wire.headers

    try:
        until = time.monotonic() + 8
        while True:
            if process.poll() is not None: raise AssertionError('Owned host exited at startup')
            try:
                wire = Wire(port)
                info = wire.query('INFO', 'server')
                break
            except OSError:
                if wire: wire.close(); wire = None
                if time.monotonic() >= until: raise
                time.sleep(.03)
        row['identity'] = host.verify_server_info(info, process.pid, version)
        (out / 'server-info.txt').write_bytes(info)
        assert wire.query('MODULE', 'LOAD', module) == b'OK'
        for index, payload in enumerate(INPUTS):
            key = f'f17:{index}'
            assert wire.query('SET', key, payload) == b'OK'
            for command in ['callreply.echo', 'callreply.get', 'callreply.owned']:
                bulk(wire.query(command, payload if command.endswith('echo') else key), payload)
                later()
            direct = wire.query('DUMP', key)
            bulk(wire.query('callreply.dump', key), direct)
            later()
            row['cases'].append({'case': 'payload', 'index': index, 'bytes': len(payload),
                                 'sha256': hashlib.sha256(payload).hexdigest(),
                                 'checks': ['ECHO', 'GET', 'owned-after-root-drop', 'DUMP',
                                            'bulk framing', 'later command'], 'result': 'passed'})
        keys = [f'f17:{i}' for i in range(len(INPUTS))]
        assert wire.query('callreply.mget', *keys) == INPUTS
        assert wire.headers[0] == b'*9\r\n' and all(h.startswith(b'$') for h in wire.headers[1:])
        later()
        assert wire.query('callreply.get', 'f17:missing') is None
        assert wire.headers == [b'$-1\r\n']
        assert wire.query('HSET', 'f17:wrongtype', 'field', 'value') == 1
        try:
            wire.query('callreply.get', 'f17:wrongtype')
            raise AssertionError('Expected ordinary wrong-type error')
        except host.BaselineError as error:
            assert 'WRONGTYPE' in str(error), error
        later()
        bulk(wire.query('callreply.status', 'f17:status', 'value'), b'OK')
        assert wire.query('callreply.explicit') == b'OK'
        assert wire.headers == [b'+OK\r\n']
        later()
        row['cases'].append({'case': 'nested/nil/error/wrapped-status/explicit-status', 'result': 'passed'})
        pairs = {k: INPUTS[(i + 1) % len(INPUTS)] for i, k in enumerate(INPUTS)}
        assert wire.query('HSET', 'f17:hash', *(v for pair in pairs.items() for v in pair)) == len(pairs)
        flattened = wire.query('callreply.map', 'f17:hash')
        assert len(flattened) == 2 * len(pairs)
        assert dict(zip(flattened[::2], flattened[1::2])) == pairs
        assert all(h.startswith(b'$') for h in wire.headers[1:])
        later()
        assert wire.query('SADD', 'f17:set', *INPUTS) == len(INPUTS)
        assert set(wire.query('callreply.set', 'f17:set')) == set(INPUTS)
        assert all(h.startswith(b'$') for h in wire.headers[1:])
        later()
        row['cases'].append({'case': 'typed RESP3 map/set keys, owned after root disposal', 'result': 'passed'})
        assert wire.query('callreply.version') == [*map(int, version.split('.')), 0]
        assert 'Failed getting deployment type' not in log.read_text(errors='replace')
        row['cases'].append({'case': 'forced version RMCall and actual OSS result', 'result': 'passed'})
        # The text config setter rejects invalid UTF-8; exclude both invalid-byte payloads.
        for payload in INPUTS[:-2]:
            assert wire.query('CONFIG', 'SET', 'callreply.text', payload) == b'OK'
            expected = wire.query('CONFIG', 'GET', 'callreply.text')
            assert wire.query('callreply.cfgget', 'text') == expected == [b'callreply.text', payload]
            assert all(h.startswith(b'$') for h in wire.headers[1:])
            later()
        row['cases'].append({'case': 'module_config_get exact text/bulk framing', 'result': 'passed'})
        assert wire.query('MODULE', 'UNLOAD', 'callreply') == b'OK'
        later()
        row['status'] = 'passed'
    except BaseException as error:
        row['status'] = 'failed'; row['error'] = repr(error)
        raise
    finally:
        if wire: wire.close()
        host.stop_process(process)
        row['server_exit'] = process.returncode
        host.write_json(out / 'report.json', row)
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server', action='append', required=True)
    parser.add_argument('--profile', action='append', choices=['debug', 'release'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args(); out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    before = sources(); report = {'status': 'running', 'sources_before': before, 'cells': []}
    try:
        for profile in args.profile:
            for server in args.server:
                version, binary = server.split('=', 1)
                report['cells'].append(cell(Path(binary).resolve(), version, profile,
                                             out / (profile + '-' + version)))
        report['sources_after'] = sources()
        assert before == report['sources_after']
        report['status'] = 'passed'
    except BaseException as error:
        report['status'] = 'failed'; report['error'] = repr(error)
        raise
    finally:
        host.write_json(out / 'report.json', report)
    print(f'{len(report["cells"])} owned host/profile cells pass')


if __name__ == '__main__': main()
