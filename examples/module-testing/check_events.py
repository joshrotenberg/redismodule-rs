#!/usr/bin/env python3
"""An author-facing companion-module probe, not a reusable SDK test harness."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("subject", type=Path, help="runtime_events cdylib")
    parser.add_argument("companion", type=Path, help="events cdylib")
    args = parser.parse_args()
    subject, companion = args.subject.resolve(), args.companion.resolve()
    for library in (subject, companion):
        if not library.is_file():
            parser.error(f"build the example first: {library}")
    work = Path(tempfile.mkdtemp(prefix="module-events-", dir="/tmp"))
    socket = work / "redis.sock"
    log = work / "redis.log"
    server_bin = os.environ.get("TEST_REDIS_SERVER_BIN", "redis-server")
    cli_bin = os.environ.get("TEST_REDIS_CLI_BIN", "redis-cli")

    def query(*command, data=None):
        argv = [cli_bin, "-s", str(socket), "--json"]
        if data is not None:
            argv.append("-x")
        reply = subprocess.run(
            [*argv, *command], input=data, capture_output=True, timeout=2, check=True
        )
        return json.loads(reply.stdout)

    failed = True
    print(f"Probe logs retained in {work}", flush=True)
    with log.open("w") as output:
        server = subprocess.Popen(
            [server_bin, "--port", "0", "--unixsocket", str(socket),
             "--unixsocketperm", "700", "--save", "", "--appendonly", "no",
             "--dir", str(work), "--loadmodule", str(subject), "generic",
             "--loadmodule", str(companion)],
            stdout=output, stderr=subprocess.STDOUT,
        )
        try:
            deadline = time.monotonic() + 10
            while True:
                if server.poll() is not None:
                    raise RuntimeError(f"Redis exited with {server.returncode}")
                try:
                    if query("PING") == "PONG":
                        break
                except (subprocess.SubprocessError, json.JSONDecodeError):
                    pass
                if time.monotonic() >= deadline:
                    raise TimeoutError("Redis did not become ready")
                time.sleep(0.05)
            for event in (b"", b"ev\xffnt"):
                sent = query("events.send_bytes", data=event)
                if sent != "Event sent":
                    raise RuntimeError(f"companion returned {sent!r}")
                observed = query("runtime.last")
                expected = [event.decode("utf-8", errors="replace"), "mykey", 0]
                if observed != expected:
                    raise RuntimeError(f"event {event!r}: {observed!r} != {expected!r}")
            if query("PING") != "PONG":
                raise RuntimeError("server did not answer after the callbacks")
            failed = False
            print("Companion empty/binary event probe passed")
        finally:
            if server.poll() is None:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=5)
            if failed:
                print(log.read_text(), flush=True)


if __name__ == "__main__":
    main()
