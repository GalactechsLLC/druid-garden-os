"""Offline release-signing integration tests; run inside an isolated scratch sandbox."""
import datetime
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

SIGNER = Path(__file__).resolve().parents[1] / "build_scripts" / "sign_release.py"


class ReleaseSigningTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="release-signing-test-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.release = self.root / "release"
        self.payloads = {"amd64": b"dummy-amd64-artifact", "aarch64": b"dummy-arm64-artifact"}
        for arch, payload in self.payloads.items():
            directory = self.release / arch
            directory.mkdir(parents=True)
            (directory / "druid-garden-os.app").write_bytes(payload)
        self.private = self.root / "fixture-private.pem"
        self.public_pem = self.root / "fixture-public.pem"
        self.public_hex = self.root / "fixture-public.hex"
        # Disposable test key only: never read or alter operator keys.
        self.command("openssl", "genpkey", "-algorithm", "ED25519", "-out", str(self.private))
        self.command("openssl", "pkey", "-in", str(self.private), "-pubout", "-out", str(self.public_pem))
        public_der = self.command("openssl", "pkey", "-in", str(self.private), "-pubout", "-outform", "DER").stdout
        self.assertEqual(public_der[:12], bytes.fromhex("302a300506032b6570032100"))
        self.assertEqual(len(public_der), 44)
        self.public_hex.write_text(public_der[12:].hex() + "\n", encoding="ascii")
        self.expires = (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=1)).replace(microsecond=0).isoformat()

    def command(self, *args, success=True):
        result = subprocess.run(args, cwd=self.root, stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
        return result

    def sign(self, *, version="1.2.3", expires=None, public=None, success=True, extra=()):
        return self.command(sys.executable, str(SIGNER), str(self.release),
                            "--version", version, "--expires", expires or self.expires,
                            "--private-key", str(self.private), "--public-key", str(public or self.public_hex),
                            *extra, success=success)

    def verify(self, manifest, signature, *, success=True):
        return self.command("openssl", "pkeyutl", "-verify", "-rawin", "-pubin",
                            "-inkey", str(self.public_pem), "-in", str(manifest),
                            "-sigfile", str(signature), success=success)

    def test_signature_authenticates_exact_manifest_and_artifact_metadata(self):
        self.sign()
        manifest = self.release / "manifest.yaml"
        signature = self.root / "signature.bin"
        signature.write_bytes(bytes.fromhex((self.release / "manifest.yaml.sig").read_text().strip()))
        self.assertEqual(signature.stat().st_size, 64)
        self.verify(manifest, signature)
        metadata = json.loads(manifest.read_text())
        self.assertEqual(metadata["version"], "1.2.3")
        expiry = int(datetime.datetime.fromisoformat(self.expires).timestamp())
        self.assertEqual(metadata["expires_unix"], expiry)
        for arch, payload in self.payloads.items():
            self.assertEqual(metadata["artifacts"][arch], {"sha256": hashlib.sha256(payload).hexdigest(), "size": len(payload)})
        # No YAML reserialization: even a valid-looking altered version must fail.
        manifest.write_bytes(manifest.read_bytes().replace(b"1.2.3", b"9.2.3"))
        self.assertNotEqual(self.verify(manifest, signature, success=False).returncode, 0)

    def test_mismatched_provisioned_key_is_rejected_before_metadata_written(self):
        wrong = self.root / "wrong-public.hex"
        wrong.write_text("00" * 32 + "\n", encoding="ascii")
        self.assertNotEqual(self.sign(public=wrong, success=False).returncode, 0)
        self.assertFalse((self.release / "manifest.yaml").exists())
        self.assertFalse((self.release / "manifest.yaml.sig").exists())

    def test_expired_metadata_and_noncanonical_versions_are_rejected(self):
        self.assertNotEqual(self.sign(expires="2000-01-01T00:00:00Z", success=False).returncode, 0)
        for version in ("01.2.3", "1.2.3-01", "../1.2.3"):
            with self.subTest(version=version):
                self.assertNotEqual(self.sign(version=version, success=False).returncode, 0)
        self.assertFalse((self.release / "manifest.yaml").exists())

    def test_signed_channel_rollout_rotation_and_offline_updater_verification(self):
        for arch in self.payloads:
            (self.release / arch / "druid-garden-edge-updater.app").write_bytes(b"dummy updater " + arch.encode())
        next_key = self.root / "next-key.hex"
        next_key.write_text("11" * 32 + "\n", encoding="ascii")
        self.sign(extra=("--channel", "beta", "--rollout-basis-points", "1250", "--rollout-seed", "cohort-a",
                         "--next-public-key", str(next_key), "--rotation-expires", "2099-01-01T00:00:00Z", "--include-updater", "--updater-revision", "2"))
        metadata = json.loads((self.release / "manifest.yaml").read_text())
        self.assertEqual(metadata["channel"], "beta")
        self.assertEqual(metadata["rollout_basis_points"], 1250)
        self.assertEqual(metadata["next_key"], {"generation": 1, "public_key": "11" * 32})
        installer = SIGNER.with_name("bootstrap_updater.py")
        self.command(sys.executable, str(installer), str(self.release), "--channel", "beta", "--trusted-public-key", str(self.public_hex))
        publisher = SIGNER.with_name("publish_release.py")
        result = self.command(sys.executable, str(publisher), str(self.release), "--channel", "beta", "--public-key", str(self.public_hex))
        self.assertIn(b"no network or publication", result.stdout)
        self.assertNotEqual(self.command(sys.executable, str(installer), str(self.release), "--channel", "stable", "--trusted-public-key", str(self.public_hex), success=False).returncode, 0)
        # Mutating either architecture's updater must fail on that architecture.
        for arch in self.payloads:
            (self.release / arch / "druid-garden-edge-updater.app").write_bytes(b"tampered updater")
        self.assertNotEqual(self.command(sys.executable, str(installer), str(self.release), "--channel", "beta", "--trusted-public-key", str(self.public_hex), success=False).returncode, 0)

    def test_rust_persisted_trust_fixture_is_readable_by_python_bootstrap(self):
        fixture = SIGNER.parents[1] / "dg_edge_updater" / "tests" / "fixtures" / "update-trust.json"
        state = json.loads(fixture.read_bytes())
        self.assertEqual(state, {"generation": 2, "public_key": "11" * 32})

    def test_bootstrap_stops_legacy_timer_before_service_check_and_installs_timer_only(self):
        spec = importlib.util.spec_from_file_location("bootstrap_fixture", SIGNER.with_name("bootstrap_updater.py"))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        calls = []
        def systemctl(*args):
            calls.append(args)
            if "--property=LoadState" in args:
                return b"loaded\n"
            if "--property=SubState" in args:
                return b"exited\n"
            return b""
        units = self.root / "units"
        units.mkdir()
        with mock.patch.object(module, "command", side_effect=systemctl):
            module.quiesce_updater()
            module.install_schedule(units)
        stop_timer = calls.index(("/usr/bin/systemctl", "stop", "druid_garden_edge_updater.timer"))
        inspect_service = next(index for index, call in enumerate(calls) if "--property=SubState" in call)
        self.assertLess(stop_timer, inspect_service)
        enable_calls = [call for call in calls if call[1] == "enable"]
        self.assertEqual(enable_calls, [("/usr/bin/systemctl", "enable", "druid_garden_edge_updater.timer")])
        self.assertFalse(any(call[1] in ("start", "restart") and call[-1] == "druid_garden_os.service" for call in calls))
        self.assertIn("RandomizedDelaySec=15min", (units / "druid_garden_edge_updater.timer").read_text())
        self.assertIn("TimeoutStartSec=15min", (units / "druid_garden_edge_updater.service").read_text())
        recovery = (units / "druid_garden_update_recovery.service").read_text()
        self.assertIn("UMask=0077", recovery)
        self.assertIn("TimeoutStartSec=2min", recovery)
        calls.clear()
        def without_timer(*args):
            calls.append(args)
            return b"not-found\n" if "--property=LoadState" in args else b"exited\n"
        with mock.patch.object(module, "command", side_effect=without_timer):
            module.quiesce_updater()
        self.assertFalse(any(call[1] == "stop" and call[-1].endswith(".timer") for call in calls))
        self.assertIn(("/usr/bin/systemctl", "stop", "druid_garden_edge_updater.service"), calls)

    def test_existing_signed_release_is_not_overwritten(self):
        self.sign()
        paths = [self.release / "manifest.yaml", self.release / "manifest.yaml.sig"]
        original = [path.read_bytes() for path in paths]
        self.assertNotEqual(self.sign(version="1.2.4", success=False).returncode, 0)
        self.assertEqual([path.read_bytes() for path in paths], original)


if __name__ == "__main__":
    unittest.main()
