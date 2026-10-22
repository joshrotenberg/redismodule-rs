#!/usr/bin/env python3
"""Check the documented current SDK floor contract; never start Redis."""
import argparse
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--toolchain", help="optional installed rustup toolchain")
    args = parser.parse_args()
    fixture = Path(__file__).resolve().parent
    cargo = ["cargo"] + ([f"+{args.toolchain}"] if args.toolchain else [])
    logs = fixture / "target" / "check-logs"
    logs.mkdir(parents=True, exist_ok=True)
    cases = [
        ("default-member", [], True),
        ("legacy-alone", ["-p", "floor-legacy"], True),
        ("modern-alone", ["-p", "floor-modern"], True),
        ("default-edge-alone", ["-p", "floor-default-edge"], True),
        ("aligned-6", ["-p", "floor-legacy", "-p", "floor-modern",
                       "--no-default-features", "--features",
                       "floor-legacy/redis-6,floor-modern/redis-6"], True),
        ("aligned-7-2", ["-p", "floor-legacy", "-p", "floor-modern",
                         "--no-default-features", "--features",
                         "floor-legacy/redis-7-2,floor-modern/redis-7-2"], True),
        ("mixed-floors", ["-p", "floor-legacy", "-p", "floor-modern"], False),
        ("whole-workspace", ["--workspace"], False),
        ("dependency-default", ["-p", "floor-modern", "-p", "floor-default-edge",
                                "--no-default-features", "--features",
                                "floor-modern/redis-7-2"], False),
        ("all-features", ["-p", "floor-modern", "--all-features"], False),
        ("missing-floor", ["-p", "floor-modern", "--no-default-features"], False),
    ]
    for name, features, succeeds in cases:
        log = logs / f"{name}.log"
        command = cargo + ["check", "--manifest-path", str(fixture / "Cargo.toml")] + features
        try:
            with log.open("w") as output:
                result = subprocess.run(command, cwd=fixture, stdout=output,
                                        stderr=subprocess.STDOUT, timeout=180)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise SystemExit(f"{name}: {error}; diagnostics: {log}") from error
        output = log.read_text()
        expected_error = "enable exactly one min-redis-compatibility-version-* feature"
        if succeeds:
            valid = result.returncode == 0
        else:
            valid = result.returncode != 0 and expected_error in output
        if not valid:
            print(output, file=sys.stderr)
            raise SystemExit(f"{name}: unexpected result {result.returncode}; diagnostics: {log}")
        print(f"PASS {name}: {'compiled' if succeeds else 'SDK rejected the floor selection'}")
    print(f"All {len(cases)} controls passed; diagnostics: {logs}")


if __name__ == "__main__":
    main()
