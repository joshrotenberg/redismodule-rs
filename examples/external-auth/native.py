#!/usr/bin/env python3
"""Owned-loopback native checks. Positive execution requires explicit selection."""

import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import platform
import re
import shutil
import socket
import subprocess
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
PACKAGE = Path(__file__).resolve().parent
ADMIN = b"fake-native-admin"
SECRETS = [ADMIN, b"fake-allow", b"fake-block", b"fake-deny", b"fake-late", b"fake-stall", b"fake-mismatch"]


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def prove_module_binding(args):
    # The SDK's admission clock must be this module's own nonpreemptible API
    # slot. Inspect the consumer itself, separately from auth_async.so's proof.
    # This only reads ELF data; it never patches an API slot or loads code.
    data = args.module.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    assert data[:6] == b"\x7fELF\x02\x01" and int.from_bytes(data[18:20], "little") == 62
    readelf = shutil.which("readelf")
    assert readelf, "existing readelf required; no installation or fallback"

    def inspect(*arguments):
        return subprocess.check_output([readelf, *arguments], text=True, timeout=15,
                                       env=dict(os.environ, LC_ALL="C"))

    version = inspect("--version")
    assert version.startswith("GNU readelf ")
    symbols = inspect("--wide", "--symbols", str(args.module))
    relocations = inspect("--wide", "--relocs", str(args.module))
    (args.output / "module-symbols.txt").write_text(symbols)
    (args.output / "module-relocations.txt").write_text(relocations)
    name, table, rows = "RedisModule_MonotonicMicroseconds", None, []
    for line in symbols.splitlines():
        if line.startswith("Symbol table "):
            table = line.split("'")[1]
        match = re.match(r"^\s*\d+:\s+([0-9a-fA-F]+)\s+(\d+)\s+(\S+)\s+(\S+)\s+(\S+)\s+(\S+)\s+(\S+)", line)
        if match and match[7].split("@", 1)[0] == name:
            rows.append({"table": table, "value": int(match[1], 16), "size": int(match[2]),
                         "type": match[3], "bind": match[4], "visibility": match[5], "section": match[6]})
    assert sum(row["table"] == ".symtab" for row in rows) == 1
    assert all(row["type"] == "OBJECT" and row["size"] == 8 and row["section"].isdigit()
               and row["value"] != 0 and (row["bind"] == "LOCAL" or row["visibility"] == "HIDDEN") for row in rows)
    assert len({row["value"] for row in rows}) == 1 and name not in relocations
    assert sha(args.module) == digest
    proof = {"module": str(args.module), "module_sha256": digest, "clock_slot": name,
             "definitions": rows, "named_clock_relocations": 0, "readelf": str(Path(readelf).resolve()),
             "readelf_sha256": sha(Path(readelf).resolve()), "readelf_version": version}
    (args.output / "module-binding-proof.json").write_text(json.dumps(proof, indent=2) + "\n")
    return digest


class Denied(Exception):
    pass


class Connection:
    def __init__(self, port):
        self.socket = socket.create_connection(("127.0.0.1", port), timeout=2)
        self.socket.settimeout(2)
        self.stream = self.socket.makefile("rb")

    def send(self, *args):
        values = [value if isinstance(value, bytes) else str(value).encode() for value in args]
        data = b"*%d\r\n" % len(values)
        for value in values:
            data += b"$%d\r\n" % len(value) + value + b"\r\n"
        self.socket.sendall(data)

    def read(self):
        line = self.stream.readline(65536)
        if not line.endswith(b"\r\n"):
            raise RuntimeError("incomplete owned Redis response")
        tag, body = line[:1], line[1:-2]
        if tag == b"-":
            raise Denied(body.decode())
        if tag == b"+":
            return body
        if tag == b":":
            return int(body)
        if tag == b"$":
            size = int(body)
            if size == -1:
                return None
            assert 0 <= size <= 8 * 1024 * 1024
            value = self.stream.read(size)
            assert len(value) == size and self.stream.read(2) == b"\r\n"
            return value
        if tag in (b"*", b"%", b"~"):
            count = int(body)
            if count == -1:
                return None
            assert 0 <= count <= 65536
            return [self.read() for _ in range(count * (2 if tag == b"%" else 1))]
        if tag == b"_":
            return None
        raise RuntimeError("unexpected owned Redis response type")

    def cmd(self, *args):
        self.send(*args)
        return self.read()

    def close(self):
        self.stream.close()
        self.socket.close()


def wait_for(description, predicate, seconds=3):
    deadline = time.monotonic() + seconds
    while True:
        if predicate():
            return
        if time.monotonic() >= deadline:
            raise AssertionError("timed out: " + description)
        time.sleep(0.005)


class Stub:
    def __init__(self):
        self.events = []
        self.started = threading.Event()
        self.release = threading.Event()
        self.stop = threading.Event()
        owner = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_):
                pass

            def do_POST(self):
                self.connection.settimeout(1)
                assert self.path == "/verify"
                assert self.headers["Content-Type"] == "application/json"
                length = int(self.headers["Content-Length"])
                assert 0 < length <= 2048
                value = json.loads(self.rfile.read(length))
                assert set(value) == {"version", "username", "password"} and value["version"] == 1
                username, password = value["username"], value["password"]
                owner.events.append({"username": username, "received": time.monotonic(), "request_bytes": length})
                owner.started.set()
                if password in ("fake-stall", "fake-block"):
                    assert owner.release.wait(3), "owned stalled provider was not released"
                elif password == "fake-late":
                    owner.stop.wait(0.3)
                role = "writer" if username == "bob" or password == "fake-mismatch" else "reader"
                body = json.dumps({"version": 1, "allowed": password != "fake-deny", "username": username, "role": role}).encode()
                try:
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.send_header("Connection", "close")
                    self.end_headers()
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.server.timeout = 0.05
        self.address = "%s:%d" % self.server.server_address
        self.errors = []

        def serve():
            try:
                while not self.stop.is_set():
                    self.server.handle_request()
            except BaseException as error:
                self.errors.append(type(error).__name__)

        # Capture handler failures, rather than HTTPServer's default print-and-continue.
        self.server.handle_error = lambda *_: self.errors.append("HTTP handler failure")
        self.thread = threading.Thread(target=serve, name="owned-http-stub")
        self.thread.start()

    def close(self):
        self.release.set()
        self.stop.set()
        self.thread.join(4)
        self.server.server_close()
        assert not self.thread.is_alive() and not self.errors, self.errors


class Runtime:
    def __init__(self, args, label):
        self.args = args
        self.directory = args.output / label
        self.directory.mkdir()
        self.connections = []
        self.drain_attempts = []
        self.process = None
        self.log = (self.directory / "redis.log").open("wb")
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            self.port = reservation.getsockname()[1]

    def connection(self, admin=False):
        connection = Connection(self.port)
        self.connections.append(connection)
        if admin:
            assert connection.cmd("AUTH", "default", ADMIN) == b"OK"
        return connection

    def __enter__(self):
        try:
            assert sha(self.args.module) == self.args.proven_module, "consumer changed after pre-load binding proof"
            argv = [str(self.args.server), "--bind", "127.0.0.1", "--port", str(self.port),
                    "--save", "", "--appendonly", "no", "--dir", str(self.directory),
                    "--enable-module-command", "local", "--requirepass", ADMIN.decode(),
                    "--loadmodule", str(self.args.module)]
            self.process = subprocess.Popen(argv, stdout=self.log, stderr=subprocess.STDOUT)

            def ready():
                if self.process.poll() is not None:
                    raise AssertionError("owned Redis exited during startup")
                try:
                    connection = self.connection(admin=True)
                except (ConnectionRefusedError, ConnectionResetError):
                    return False
                info = connection.cmd("INFO", "server").decode()
                assert ("process_id:%d\r\n" % self.process.pid) in info
                self.admin = connection
                self.info = info
                return True

            wait_for("owned password-protected Redis startup", ready, 15)
            native = Path("/proc/%d/exe" % self.process.pid)
            if self.args.mode == "positive":
                assert sha(native) == self.args.accepted
            loaded = self.admin.cmd("MODULE", "LIST")
            entries = [dict(zip(row[::2], row[1::2])) for row in loaded]
            selected = [row for row in entries if row[b"name"] == b"external_auth"]
            assert len(selected) == 1 and Path(os.fsdecode(selected[0][b"path"])).resolve() == self.args.module
            module_hash = sha(self.args.module)
            retained = self.directory / ("module-" + module_hash + ".so")
            retained.write_bytes(self.args.module.read_bytes())
            self.identity = {"pid": self.process.pid, "port": self.port, "info": self.info,
                             "module_sha256": module_hash, "retained_module": retained.name,
                             "native_process_sha256": sha(native), "native_binary_sha256": sha(self.args.server)}
            (self.directory / "identity.json").write_text(json.dumps(self.identity, indent=2) + "\n")
            return self
        except BaseException:
            self.close()
            raise

    def configure(self, endpoint, prepare=True):
        assert self.admin.cmd("external_auth.configure", endpoint) == b"OK"
        assert self.admin.cmd("external_auth.map", "alice", "reader") == b"OK"
        assert self.admin.cmd("external_auth.map", "bob", "writer") == b"OK"
        if prepare:
            assert self.admin.cmd("external_auth.prepare") > 0
            assert self.admin.cmd("external_auth.activate") == b"OK"

    def drain(self):
        def drained():
            try:
                return self.admin.cmd("external_auth.drain") == b"OK"
            except Denied as error:
                message = str(error)
                self.drain_attempts.append({"at": time.monotonic(), "message": message})
                # These retain ownership and require another ordinary command.
                # The same fixed three-second deadline still applies. An
                # unavailable admission that never recovers remains a failure.
                assert message in {
                    "ERR external auth drain pending",
                    "ERR client identity cleanup is still pending; retry drain",
                    "ERR asynchronous authentication is unavailable",
                }
                return False
        wait_for("nonblocking application and SDK drain", drained)

    def close(self):
        for connection in self.connections:
            connection.close()
        self.connections.clear()
        if self.process is not None:
            self.process.kill()
            self.process.wait(timeout=3)
            assert not Path("/proc/%d" % self.process.pid).exists()
            (self.directory / "cleanup.json").write_text(json.dumps({"pid": self.process.pid,
                "owned_child_waited": True, "drain_pending_attempts": self.drain_attempts}, indent=2) + "\n")
            self.process = None
        self.log.close()

    def __exit__(self, *_):
        self.close()


def denied(connection, *command):
    try:
        connection.cmd(*command)
    except Denied as error:
        assert str(error).startswith("WRONGPASS")
        assert not any(secret.decode() in str(error) for secret in SECRETS)
        return
    raise AssertionError("authentication unexpectedly succeeded")


def close_stub(stub, args, label):
    try:
        stub.close()
    finally:
        (args.output / (label + "-stub.json")).write_text(json.dumps({"events": stub.events,
            "errors": stub.errors, "owned_thread_joined": not stub.thread.is_alive()}, indent=2) + "\n")


def blocked_success(runtime, stub, username, hello=False):
    stub.started.clear()
    stub.release.clear()
    client = runtime.connection()
    command = ("HELLO", 2, "AUTH", username, "fake-block") if hello else ("AUTH", username, "fake-block")
    client.send(*command)
    assert stub.started.wait(1)
    address = "addr=127.0.0.1:%d" % client.socket.getsockname()[1]
    rows = runtime.admin.cmd("CLIENT", "LIST").decode().splitlines()
    row = next(line for line in rows if address in line.split())
    fields = dict(field.split("=", 1) for field in row.split())
    assert "b" in fields["flags"] and fields["cmd"] == ("hello" if hello else "auth")
    stub.release.set()
    response = client.read()
    if hello:
        assert dict(zip(response[::2], response[1::2]))[b"proto"] == 2
    else:
        assert response == b"OK"
    return client, row


def refusal(args):
    stub = Stub()
    try:
        with Runtime(args, "refusal") as runtime:
            version_text = next(line.split(":", 1)[1] for line in runtime.info.splitlines() if line.startswith("redis_version:"))
            version = tuple(int(part) for part in version_text.split(".")[:2])
            partial = version < (7, 4)
            try:
                runtime.configure(stub.address, prepare=False)
            except Denied as error:
                assert partial and str(error) == "ERR transactional ACL updates require Redis 7.4 or later"
            else:
                assert not partial
            # An early ACL failure must retain the created handle and require
            # explicit cleanup. Configuration never silently rolls ownership back.
            try:
                runtime.admin.cmd("MODULE", "UNLOAD", "external_auth")
            except Denied:
                pass
            else:
                raise AssertionError("configured or partial user ownership was not retained")
            for command in ["external_auth.activate", "external_auth.prepare", "external_auth.activate"]:
                try:
                    runtime.admin.cmd(command)
                except Denied:
                    pass
                else:
                    raise AssertionError("unprepared/unsupported runtime activated")
            assert runtime.admin.cmd("external_auth.status") == [b"configured", 0, 0]
            assert runtime.connection(admin=True).cmd("PING") == b"PONG"
            assert not stub.events
            runtime.drain()
            assert runtime.admin.cmd("MODULE", "UNLOAD", "external_auth") == b"OK"
    finally:
        close_stub(stub, args, "refusal")
    return {"activation_refused_before_worker_or_network": True, "provider_requests": len(stub.events),
            "partial_ACL_configuration_refused": partial, "retained_configuration_drained": True}


def bootstrap(args):
    stub = Stub()
    try:
        with Runtime(args, "bootstrap") as runtime:
            # Native password authentication works during both bootstrap states.
            assert runtime.admin.cmd("external_auth.status") == [b"inactive", 0, 0]
            runtime.configure(stub.address, prepare=False)
            try:
                runtime.admin.cmd("external_auth.activate")
            except Denied:
                pass
            else:
                raise AssertionError("unprepared activation succeeded")
            assert runtime.connection(admin=True).cmd("PING") == b"PONG"
            assert runtime.admin.cmd("external_auth.prepare") > 0
            monitor = runtime.connection(admin=True)
            assert monitor.cmd("MONITOR") == b"OK"
            assert runtime.admin.cmd("CONFIG", "SET", "slowlog-log-slower-than", 0) == b"OK"
            assert runtime.admin.cmd("external_auth.activate") == b"OK"
            assert runtime.admin.cmd("PING") == b"PONG"  # Existing admin stays usable.
            denied(runtime.connection(), "AUTH", "default", ADMIN)
            reader, blocked_auth = blocked_success(runtime, stub, "alice")
            writer, blocked_hello = blocked_success(runtime, stub, "bob", hello=True)
            assert writer.cmd("SET", "http:key", "value") == b"OK"
            assert reader.cmd("GET", "http:key") == b"value"
            for client, command in [(reader, ("SET", "http:key", "no")), (reader, ("GET", "other:key")), (writer, ("CONFIG", "GET", "port")), (reader, ("external_auth.drain",))]:
                try:
                    client.cmd(*command)
                except Denied as error:
                    assert str(error).startswith("NOPERM")
                else:
                    raise AssertionError("configured ACL was not enforced")
            for username, password in [("alice", "fake-deny"), ("alice", "fake-mismatch"), ("unmapped", "fake-allow"), (b"\xff", "fake-allow"), ("alice", b"\xff")]:
                denied(runtime.connection(), "AUTH", username, password)
            slowlog = runtime.admin.cmd("SLOWLOG", "GET", 128)
            audit = runtime.admin.cmd("ACL", "LOG", 128)
            assert audit, "denials must produce native ACL audit entries"
            monitor.socket.settimeout(0.05)
            observed = []
            try:
                while True:
                    observed.append(monitor.read())
            except (TimeoutError, socket.timeout):
                pass
            for evidence in [repr(slowlog).encode(), repr(audit).encode(), repr(observed).encode()]:
                assert not any(secret in evidence for secret in SECRETS)
            (runtime.directory / "privacy-raw.txt").write_text(
                "SLOWLOG:\n" + repr(slowlog) + "\nACL LOG:\n" + repr(audit) + "\nMONITOR:\n" + repr(observed) + "\n")
            report = {"AUTH": True, "HELLO_AUTH": True, "reader_writer_ACL": True,
                      "admin_connection_survived_activation": True, "slowlog_entries": len(slowlog),
                      "audit_entries": len(audit), "monitor_entries": len(observed), "secrets_absent": True,
                      "blocked_AUTH_client": blocked_auth, "blocked_HELLO_client": blocked_hello,
                      "provider_requests": len(stub.events)}
            (runtime.directory / "privacy.json").write_text(json.dumps(report, indent=2) + "\n")
            runtime.drain()
            assert runtime.admin.cmd("MODULE", "UNLOAD", "external_auth") == b"OK"
            return report
    finally:
        close_stub(stub, args, "bootstrap")


def late_disconnect_and_drain(args):
    stub = Stub()
    try:
        with Runtime(args, "late-disconnect-drain") as runtime:
            runtime.configure(stub.address)
            client = runtime.connection()
            start = time.monotonic()
            denied(client, "AUTH", "alice", "fake-late")
            elapsed = time.monotonic() - start
            assert elapsed >= 0.15
            # The provider's allow arrives after the SDK deadline. It cannot
            # authenticate the connection or create a second AUTH response.
            time.sleep(0.2)
            try:
                client.cmd("GET", "http:key")
            except Denied as error:
                assert str(error).startswith("NOAUTH")
            else:
                raise AssertionError("late result authenticated expired client")
            stub.started.clear()
            disconnected = runtime.connection()
            disconnected.send("HELLO", 2, "AUTH", "alice", "fake-stall")
            assert stub.started.wait(1)
            disconnected.close()
            runtime.connections.remove(disconnected)
            before = time.monotonic()
            assert runtime.admin.cmd("PING") == b"PONG"
            responsiveness = time.monotonic() - before
            assert responsiveness < 0.2
            try:
                runtime.admin.cmd("MODULE", "UNLOAD", "external_auth")
            except Denied:
                pass
            else:
                raise AssertionError("unload accepted retained worker ownership")
            before = time.monotonic()
            runtime.drain()
            drain_seconds = time.monotonic() - before
            stub.release.set()
            assert runtime.admin.cmd("external_auth.status")[0:2] == [b"inactive", 0]
            assert runtime.admin.cmd("MODULE", "UNLOAD", "external_auth") == b"OK"
            assert runtime.admin.cmd("MODULE", "LOAD", str(args.module)) == b"OK"
            assert runtime.admin.cmd("external_auth.status") == [b"inactive", 0, 0]
            runtime.drain()
            assert runtime.admin.cmd("MODULE", "UNLOAD", "external_auth") == b"OK"
            return {"expired_AUTH_seconds": elapsed, "stalled_provider_ping_seconds": responsiveness,
                    "cancel_drain_seconds": drain_seconds, "late_allow_lost": True,
                    "blocked_disconnect": True, "unload_refused_until_join": True, "reload": True}
    finally:
        close_stub(stub, args, "late-disconnect-drain")


def connect_timeout_and_concurrency(args):
    # Saturate an owned loopback accept queue. No existing service is probed.
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen(0)
    address = listener.getsockname()
    blockers = []
    try:
        for _ in range(8):
            client = socket.socket()
            client.settimeout(0.05)
            try:
                client.connect(address)
                blockers.append(client)
            except TimeoutError:
                client.close()
                break
        else:
            raise AssertionError("owned accept queue did not saturate")
        with Runtime(args, "connect-timeout-concurrency") as runtime:
            runtime.configure("%s:%d" % address)
            start = time.monotonic()
            denied(runtime.connection(), "AUTH", "alice", "fake-allow")
            elapsed = time.monotonic() - start
            assert 0.07 <= elapsed < 1
            # Retained finished handles also consume the four admission slots.
            for _ in range(3):
                denied(runtime.connection(), "AUTH", "alice", "fake-allow")
            before = runtime.admin.cmd("external_auth.status")
            assert before[1:] == [4, 4]
            denied(runtime.connection(), "AUTH", "alice", "fake-allow")
            assert runtime.admin.cmd("external_auth.status") == before
            runtime.drain()
            assert runtime.admin.cmd("MODULE", "UNLOAD", "external_auth") == b"OK"
            return {"connect_timeout_seconds": elapsed, "retained_handle_bound": 4, "fifth_request_no_worker": True}
    finally:
        for client in blockers:
            client.close()
        listener.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--module", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--floor", choices=["6-0", "7-2"], required=True)
    parser.add_argument("--mode", choices=["positive", "refusal"], required=True)
    args = parser.parse_args()
    args.server, args.module, args.output = args.server.resolve(), args.module.resolve(), args.output.resolve()
    assert platform.system() == "Linux" and platform.machine() == "x86_64"
    manifest = json.loads((ROOT / ".github/experimental-auth/accepted-runtime.json").read_text())
    args.accepted = manifest["artifact"]["sha256"]
    if args.mode == "positive":
        assert os.environ.get("EXTERNAL_AUTH_POSITIVE") == "1", "positive execution not explicitly selected"
        assert sha(args.server) == args.accepted
    else:
        assert sha(args.server) != args.accepted, "refusal lane requires unsupported runtime"
    args.output.mkdir()
    args.proven_module = prove_module_binding(args)
    environment = {"host": platform.uname()._asdict(), "source_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
                   "SDK_pin": "a2943e7870e00da01093759bc468978e85e01dc7", "floor": args.floor,
                   "lock_sha256": sha(PACKAGE / "Cargo.lock"), "server_sha256": sha(args.server),
                   "module_sha256": sha(args.module), "mode": args.mode,
                   "source_sha256": {str(p.relative_to(ROOT)): sha(p) for p in sorted(PACKAGE.rglob("*")) if p.is_file() and "__pycache__" not in p.parts},
                   "rustc": subprocess.check_output(["rustc", "-Vv"], text=True),
                   "cargo": subprocess.check_output(["cargo", "-V"], text=True)}
    (args.output / "environment.json").write_text(json.dumps(environment, indent=2) + "\n")
    report = {}
    try:
        cases = {"unsupported_refusal": refusal} if args.mode == "refusal" else {
            "bootstrap_AUTH_HELLO_ACL_privacy": bootstrap,
            "late_disconnect_cancel_drain_reload": late_disconnect_and_drain,
            "owned_connect_timeout_concurrency": connect_timeout_and_concurrency,
        }
        for name, case in cases.items():
            report[name] = case(args)
            print("EXTERNAL_AUTH_CASE_EXECUTED:" + name, flush=True)
    finally:
        (args.output / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    assert set(report) == set(cases)


if __name__ == "__main__":
    main()
