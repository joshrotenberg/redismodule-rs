#!/usr/bin/env python3
"""Bounded eviction and RDB-startup probes for issue 68; no host changes.

Usage: policy.py MODULE HELLO_COMPONENT JS_COMPONENT --output result.json
Use the same pinned binaries/components as memory.sh. REDIS_SERVER selects
the server. Each case owns its server, port, data directory and test keys.
"""

import argparse
import concurrent.futures
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import socket
import subprocess
import sys
import tempfile
import time


class RedisError(Exception):
    pass


class Client:
    def __init__(self, port, timeout=5):
        self.socket = socket.create_connection(("127.0.0.1", port), timeout)
        self.file = self.socket.makefile("rb")

    def close(self):
        self.file.close()
        self.socket.close()

    def load(self, name, component):
        previous = self.socket.gettimeout()
        self.socket.settimeout(120)
        try:
            return self.call("WASM.LOAD", name, component)
        finally:
            self.socket.settimeout(previous)

    def call(self, *args):
        args = [arg if isinstance(arg, bytes) else str(arg).encode() for arg in args]
        self.socket.sendall(b"*%d\r\n" % len(args) + b"".join(
            b"$%d\r\n" % len(arg) + arg + b"\r\n" for arg in args))
        return self.read_reply()

    def read_reply(self):
        line = self.file.readline()
        if not line:
            raise EOFError("Redis closed the connection")
        kind, value = line[:1], line[1:-2]
        if kind == b"-":
            raise RedisError(value.decode())
        if kind == b"+":
            return value.decode()
        if kind == b":":
            return int(value)
        if kind == b"*":
            return [self.read_reply() for _ in range(int(value))]
        if kind == b"$":
            length = int(value)
            if length == -1:
                return None
            result = self.file.read(length)
            if len(result) != length or self.file.read(2) != b"\r\n":
                raise EOFError("Incomplete Redis bulk reply")
            return result
        raise ValueError(f"Unexpected RESP reply: {line!r}")


class Server:
    def __init__(self, module, directory, maxmemory=0, persist=True):
        self.module, self.directory, self.maxmemory = module, directory, maxmemory
        self.persist = persist

    def __enter__(self):
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            self.port = listener.getsockname()[1]
        self.process = subprocess.Popen([
            os.environ.get("REDIS_SERVER", "redis-server"), "--bind", "127.0.0.1",
            "--port", str(self.port), "--save", "", "--appendonly", "no",
            "--dir", str(self.directory), "--logfile", str(self.directory / "server.log"),
            "--enable-module-command", "local",
            "--maxmemory", str(self.maxmemory), "--loadmodule", str(self.module),
            *([] if self.persist else ["persist", "no"])])
        self.client = None
        deadline = time.monotonic() + 30
        try:
            while time.monotonic() < deadline:
                if self.process.poll() is not None:
                    raise RuntimeError((self.directory / "server.log").read_text())
                client = None
                try:
                    client = Client(self.port)
                    if client.call("PING") == "PONG":
                        info = parse_info(client.call("INFO", "server"))
                        if int(info["process_id"]) != self.process.pid:
                            raise RuntimeError("Another server answered on the selected port")
                        self.client = client
                        return self
                except (OSError, RedisError):
                    if client:
                        client.close()
                time.sleep(0.05)
            raise TimeoutError("Redis startup exceeded 30 seconds")
        except BaseException:
            self.__exit__(*sys.exc_info())
            raise

    def __exit__(self, kind, error, trace):
        if self.client:
            self.client.close()
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        if error is not None:
            print((self.directory / "server.log").read_text(), file=sys.stderr)

    def snapshot(self):
        info = parse_info(self.client.call("INFO", "memory", "wasm", "stats"))
        names = ["used_memory", "used_memory_peak", "used_memory_rss", "evicted_keys",
                 "wasm_libraries", "wasm_code_bytes", "wasm_compiling",
                 "wasm_instance_memory_bytes", "wasm_instances"]
        result = {name: int(info[name]) for name in names}
        result["ps_rss"] = 1024 * int(subprocess.check_output(
            ["ps", "-o", "rss=", "-p", str(self.process.pid)]))
        result["mem_allocator"] = info["mem_allocator"]
        return result


def parse_info(data):
    return dict(line.split(":", 1) for line in data.decode().splitlines()
                if line and not line.startswith("#"))


def reply(client, *args):
    try:
        return client.call(*args)
    except RedisError as error:
        return str(error)


def require(condition, message="Unexpected Redis reply"):
    if not condition:
        raise RuntimeError(message)


def preload(server):
    keys = [f"policy:user:{i}" for i in range(16)]
    for key in keys:
        require(server.client.call("SET", key, b"x" * (1 << 20)) == "OK")
    return keys


def grow_case(server, hello, eviction):
    require(b"counter" in server.client.load("hello", hello))
    keys = preload(server) if eviction else []
    time.sleep(0.3)
    before = server.snapshot()
    limit = before["used_memory"] + (16 << 20)
    server.client.call("CONFIG", "SET", "maxmemory-policy", "allkeys-lru" if eviction else "noeviction")
    server.client.call("CONFIG", "SET", "maxmemory", limit)
    growth = reply(server.client, "WASM.CALL", "grow", 0, 48 << 20)
    trigger = reply(server.client, "SET", "policy:trigger", "x")
    after = server.snapshot()
    require(isinstance(growth, int), f"Guest growth failed: {growth}")
    require(server.client.call("PING") == "PONG")
    return dict(before=before, after=after, maxmemory=limit, growth=growth,
                trigger=trigger, user_keys_remaining=server.client.call("EXISTS", *keys) if keys else None)


def compile_case(server, javascript, eviction):
    keys = preload(server) if eviction else []
    before = server.snapshot()
    limit = before["used_memory"] + (64 << 20)
    server.client.call("CONFIG", "SET", "maxmemory-policy", "allkeys-lru" if eviction else "noeviction")
    server.client.call("CONFIG", "SET", "maxmemory", limit)
    samples, responses = [], {"OK": 0, "OOM": 0}

    def load():
        client = Client(server.port, timeout=120)
        try:
            return client.load("javascript", javascript)
        finally:
            client.close()

    started = time.monotonic()
    with concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
        future = executor.submit(load)
        while not future.done():
            samples.append(server.snapshot())
            result = reply(server.client, "SET", "policy:trigger", "x")
            if result == "OK":
                responses["OK"] += 1
            elif result.startswith("OOM"):
                responses["OOM"] += 1
            else:
                raise RuntimeError(f"Unexpected SET reply: {result}")
            time.sleep(0.05)
        require(b"counter" in future.result())
    after = server.snapshot()
    require(server.client.call("WASM.DELETE", "javascript") == "OK")
    deleted = server.snapshot()
    purge = reply(server.client, "MEMORY", "PURGE")
    time.sleep(0.3)
    return dict(before=before, after=after, samples=samples,
                maxmemory=limit, seconds=time.monotonic() - started, writes=responses,
                after_delete=deleted, purge_reply=purge, after_purge=server.snapshot(),
                user_keys_remaining=server.client.call("EXISTS", *keys) if keys else None)


def startup_case(module, directory, javascript):
    with Server(module, directory) as server:
        require(b"counter" in server.client.load("javascript", javascript))
        require(server.client.call("SAVE") == "OK")
    started = time.monotonic()
    samples, responses = [], {"OK": 0, "OOM": 0}
    with Server(module, directory, maxmemory=32 << 20) as server:
        ready = time.monotonic() - started
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            sample = server.snapshot()
            samples.append(sample)
            result = reply(server.client, "SET", "policy:trigger", "x")
            if result == "OK":
                responses["OK"] += 1
            elif result.startswith("OOM"):
                responses["OOM"] += 1
            else:
                raise RuntimeError(f"Unexpected SET reply: {result}")
            if sample["wasm_compiling"] == 0:
                break
            time.sleep(0.05)
        else:
            raise TimeoutError("RDB compilation exceeded 120 seconds")
        require(samples[-1]["wasm_libraries"] == 1)
        counter = reply(server.client, "WASM.CALL", "counter", 0)
        require(server.client.call("PING") == "PONG")
        return dict(ready_seconds=ready, seconds=time.monotonic() - started,
                    maxmemory=32 << 20, samples=samples, writes=responses,
                    counter=counter, after=server.snapshot())


def empty_reloads(module, directory):
    with Server(module, directory, persist=False) as server:
        before = server.snapshot()
        checkpoints = {}
        for cycle in range(1, 31):
            require(server.client.call("MODULE", "UNLOAD", "wasm") == "OK")
            require(server.client.call("MODULE", "LOAD", str(module), "persist", "no") == "OK")
            if cycle in [1, 5, 10, 20, 30]:
                checkpoints[cycle] = server.snapshot()
        return dict(before=before, checkpoints=checkpoints)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("module", type=Path)
    parser.add_argument("hello", type=Path)
    parser.add_argument("javascript", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    paths = [args.module.resolve(), args.hello.resolve(), args.javascript.resolve()]
    result = {"started_at": datetime.now(timezone.utc).isoformat(),
        "probe_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "complete": False, "platform": platform.platform(), "server": subprocess.check_output([
        os.environ.get("REDIS_SERVER", "redis-server"), "--version"], text=True).strip(),
        "rayon_threads": os.environ.get("RAYON_NUM_THREADS", "unset"),
        "sha256": {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}, "cases": {}}
    hello, javascript = paths[1].read_bytes(), paths[2].read_bytes()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    for name, action in [
        ("growth-noeviction", lambda s: grow_case(s, hello, False)),
        ("growth-eviction", lambda s: grow_case(s, hello, True)),
        ("compile-noeviction", lambda s: compile_case(s, javascript, False)),
        ("compile-eviction", lambda s: compile_case(s, javascript, True)),
    ]:
        with tempfile.TemporaryDirectory(prefix="wasm-policy-") as temporary:
            with Server(paths[0], Path(temporary)) as server:
                result["cases"][name] = action(server)
        args.output.write_text(json.dumps(result, indent=2) + "\n")
        print(name, "complete", flush=True)
    with tempfile.TemporaryDirectory(prefix="wasm-policy-rdb-") as temporary:
        result["cases"]["startup"] = startup_case(paths[0], Path(temporary), javascript)
    with tempfile.TemporaryDirectory(prefix="wasm-policy-empty-") as temporary:
        result["cases"]["empty-reloads"] = empty_reloads(paths[0], Path(temporary))
    result["complete"] = True
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print("startup complete", flush=True)


if __name__ == "__main__":
    main()
