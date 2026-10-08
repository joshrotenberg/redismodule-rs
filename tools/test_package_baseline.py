"""Checks for the provenance gate, independent of Cargo and a Redis process."""

import hashlib
from pathlib import Path
import tempfile
import unittest

import package_baseline as baseline


class RegistryTests(unittest.TestCase):
    def test_dependency_alias_and_registry_routing(self):
        manifest = {
            "package": {"name": "redis-module", "version": "2.0.8"},
            "dependencies": {
                "common": {"package": "redis-module-common", "version": "0.1.1"},
                "serde": {"version": "1", "default-features": False, "features": ["derive"]},
            },
            "target": {"cfg(unix)": {"build-dependencies": {"cc": {"version": "1"}}}},
        }
        entry = baseline.index_entry(manifest, "candidate-digest")
        dependencies = {dependency["name"]: dependency for dependency in entry["deps"]}
        self.assertEqual(dependencies["common"]["package"], "redis-module-common")
        self.assertIsNone(dependencies["common"]["registry"])
        self.assertEqual(dependencies["serde"]["registry"], baseline.CRATES_IO)
        self.assertFalse(dependencies["serde"]["default_features"])
        self.assertEqual(dependencies["cc"]["kind"], "build")
        self.assertEqual(dependencies["cc"]["target"], "cfg(unix)")


class ServerIdentityTests(unittest.TestCase):
    INFO = b"# Server\r\nredis_version:7.2.8\r\nprocess_id:123\r\n"

    def test_owned_server_with_expected_version_is_accepted(self):
        self.assertEqual(baseline.verify_server_info(self.INFO, 123, "7.2.8")["process_id"], "123")

    def test_other_process_is_rejected(self):
        with self.assertRaisesRegex(baseline.BaselineError, "not the owned server"):
            baseline.verify_server_info(self.INFO, 124, "7.2.8")

    def test_other_version_is_rejected(self):
        with self.assertRaisesRegex(baseline.BaselineError, "Expected Redis 8.10.2"):
            baseline.verify_server_info(self.INFO, 123, "8.10.2")


class ProvenanceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.home = Path(self.temporary.name)
        self.registry = "sparse+http://127.0.0.1:12345/index/"
        self.source = self.registry
        self.metadata = {"packages": []}
        self.lock = {"package": []}
        self.archives = {}
        for name in baseline.PACKAGES:
            directory = self.home / "registry/src/candidate" / name
            directory.mkdir(parents=True)
            content = b"the exact archived manifest bytes"
            (directory / "Cargo.toml").write_bytes(content)
            cache = self.home / "registry/cache/candidate" / f"{name}-1.0.0.crate"
            cache.parent.mkdir(parents=True, exist_ok=True)
            cache.write_bytes(name.encode())
            archive_digest = baseline.sha256(cache)
            self.metadata["packages"].append({"name": name, "version": "1.0.0", "source": self.source,
                                               "manifest_path": str(directory / "Cargo.toml")})
            self.lock["package"].append({"name": name, "version": "1.0.0", "source": self.source,
                                         "checksum": archive_digest})
            self.archives[name] = {"version": "1.0.0", "sha256": archive_digest,
                                   "files": {"Cargo.toml": hashlib.sha256(content).hexdigest()}}

    def verify(self):
        return baseline.verify_metadata(self.metadata, self.lock, self.registry, self.archives,
                                        self.home, set(baseline.PACKAGES))

    def test_exact_archive_graph_is_accepted(self):
        self.assertEqual(len(self.verify()), 4)

    def test_same_version_upstream_package_is_rejected(self):
        self.metadata["packages"][0]["source"] = "registry+" + baseline.CRATES_IO
        with self.assertRaisesRegex(baseline.BaselineError, "outside this candidate"):
            self.verify()

    def test_wrong_archive_checksum_is_rejected(self):
        self.lock["package"][0]["checksum"] = "some-other-archive"
        with self.assertRaisesRegex(baseline.BaselineError, "lock source/checksum"):
            self.verify()

    def test_changed_extracted_file_is_rejected(self):
        Path(self.metadata["packages"][0]["manifest_path"]).write_text("not the archived file")
        with self.assertRaisesRegex(baseline.BaselineError, "Extracted file differs"):
            self.verify()

    def test_missing_candidate_package_is_rejected(self):
        self.metadata["packages"].pop()
        with self.assertRaisesRegex(baseline.BaselineError, "Unexpected candidate package graph"):
            self.verify()

    def test_wrong_cached_archive_is_rejected(self):
        archive = self.home / "registry/cache/candidate" / f"{baseline.PACKAGES[0]}-1.0.0.crate"
        archive.write_text("some-other-archive")
        with self.assertRaisesRegex(baseline.BaselineError, "cached archive"):
            self.verify()


if __name__ == "__main__":
    unittest.main()
