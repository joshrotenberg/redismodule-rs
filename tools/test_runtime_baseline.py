import json
import unittest

from runtime_baseline import assert_single_pass, integration_executable, validate_server_cells


class EvidenceTests(unittest.TestCase):
    def test_server_cells_cannot_overwrite_each_others_evidence(self):
        validate_server_cells(["7.2.8=/tmp/redis-a", "8.10.2=/tmp/redis-b"])
        with self.assertRaises(ValueError):
            validate_server_cells(["7.2.8=/tmp/redis-a", "7.2.8=/tmp/redis-b"])
        with self.assertRaises(ValueError):
            validate_server_cells(["../server=/tmp/redis-a"])
        with self.assertRaises(ValueError):
            validate_server_cells(["7.2.8"])

    def test_zero_filtered_or_other_tests_are_not_a_pass(self):
        self.assertFalse(assert_single_pass("test result: ok. 0 passed; 0 failed; 0 ignored;", "test_hello"))
        self.assertFalse(assert_single_pass("test other ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;", "test_hello"))
        self.assertTrue(assert_single_pass("test test_hello ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;", "test_hello"))

    def test_cargo_artifact_must_be_unique_test_executable(self):
        artifact = {"reason": "compiler-artifact", "target": {"name": "integration"},
                    "profile": {"test": True}, "executable": "/tmp/test-a"}
        self.assertEqual(integration_executable(json.dumps(artifact)), "/tmp/test-a")
        with self.assertRaises(RuntimeError):
            integration_executable("build finished")
        second = dict(artifact, executable="/tmp/test-b")
        with self.assertRaises(RuntimeError):
            integration_executable(json.dumps(artifact) + "\n" + json.dumps(second))


if __name__ == "__main__":
    unittest.main()
