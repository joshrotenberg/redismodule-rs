"""Compare two release builds of the test_helper fixture on owned Redis children."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import socket
import statistics
import subprocess
import time


def request(*args):
    parts = [str(arg).encode() for arg in args]
    return b"*%d\r\n" % len(parts) + b"".join(
        b"$%d\r\n" % len(part) + part + b"\r\n" for part in parts
    )


def receive(sock, size):
    parts = bytearray()
    while len(parts) < size:
        part = sock.recv(size - len(parts))
        if not part:
            raise RuntimeError("Redis closed the benchmark connection")
        parts.extend(part)
    return bytes(parts)


class Server:
    def __init__(self, binary, module, directory):
        directory.mkdir(parents=True, exist_ok=True)
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            self.port = reservation.getsockname()[1]
        self.log = directory / "redis.log"
        self.child = subprocess.Popen(
            [binary, "--bind", "127.0.0.1", "--port", str(self.port),
             "--save", "", "--appendonly", "no", "--dir", str(directory),
             "--logfile", str(self.log), "--loadmodule", str(module)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        try:
            deadline = time.monotonic() + 15
            while True:
                if self.child.poll() is not None:
                    raise RuntimeError(f"Redis exited: {self.log.read_text()}")
                try:
                    with self.connection() as con:
                        con.sendall(request("INFO", "server"))
                        with con.makefile("rb") as reader:
                            header = reader.readline()
                            if not header.startswith(b"$"):
                                raise RuntimeError(f"Unexpected INFO reply: {header!r}")
                            body = reader.read(int(header[1:]))
                            fields = dict(line.split(b":", 1) for line in body.splitlines()
                                          if b":" in line)
                            if fields.get(b"process_id") != str(self.child.pid).encode():
                                raise RuntimeError("Benchmark listener is not the owned child")
                            self.version = fields[b"redis_version"].decode()
                    break
                except (ConnectionRefusedError, TimeoutError):
                    if time.monotonic() >= deadline:
                        raise RuntimeError(f"Redis startup timed out; see {self.log}")
                    time.sleep(0.01)
        except BaseException:
            self.close()
            raise

    def connection(self):
        con = socket.create_connection(("127.0.0.1", self.port), timeout=5)
        con.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        return con

    def close(self):
        if self.child.poll() is None:
            self.child.kill()
        self.child.wait()

    def measure(self, mode, expected, commands, pipeline):
        payload = request("test_helper.reply_guard", mode) * pipeline
        reply = expected * pipeline
        with self.connection() as con:
            start = time.perf_counter_ns()
            for _ in range(commands // pipeline):
                con.sendall(payload)
                actual = receive(con, len(reply))
                if actual != reply:
                    raise RuntimeError(f"Unexpected benchmark reply for {mode}: {actual[:80]!r}")
            return (time.perf_counter_ns() - start) / commands


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--server", default=os.environ.get("TEST_REDIS_SERVER_BIN", "redis-server"))
    parser.add_argument("--work-dir", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=9)
    parser.add_argument("--commands", type=int, default=32768)
    parser.add_argument("--pipeline", type=int, default=128)
    args = parser.parse_args()
    if (args.samples < 3 or args.pipeline < 1 or args.commands < args.pipeline
            or args.commands % args.pipeline):
        parser.error("Use at least 3 samples and a positive whole number of pipelines")
    modules = {"baseline": args.baseline.resolve(), "candidate": args.candidate.resolve()}
    servers = {}
    result = {
        "platform": platform.platform(), "cpu_count": os.cpu_count(),
        "samples": args.samples, "commands_per_sample": args.commands,
        "pipeline": args.pipeline, "server": args.server,
        "module_sha256": {key: hashlib.sha256(path.read_bytes()).hexdigest()
                          for key, path in modules.items()},
        "note": "Amortized wall time includes client I/O and reply verification; no latency threshold is asserted.",
        "cases": {},
    }
    try:
        for name, module in modules.items():
            servers[name] = Server(args.server, module, args.work_dir.resolve() / name)
        result["redis_versions"] = {key: server.version for key, server in servers.items()}
        for mode, reply in [
            ("return", b"+first\r\n"), ("direct", b"+first\r\n"),
            ("forward-only", b"$5\r\nfirst\r\n"),
            ("array-return", b"*3\r\n:1\r\n$-1\r\n*0\r\n"),
        ]:
            samples = {name: [] for name in servers}
            for server in servers.values():
                server.measure(mode, reply, args.pipeline * 16, args.pipeline)
            for sample in range(args.samples):
                order = ["baseline", "candidate"] if sample % 2 == 0 else ["candidate", "baseline"]
                for name in order:
                    samples[name].append(servers[name].measure(
                        mode, reply, args.commands, args.pipeline))
            medians = {key: statistics.median(values) for key, values in samples.items()}
            result["cases"][mode] = {
                "ns_per_command": samples, "median_ns_per_command": medians,
                "median_change_percent": 100 * (medians["candidate"] / medians["baseline"] - 1),
            }
    finally:
        for server in servers.values():
            server.close()
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
