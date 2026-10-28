#!/usr/bin/env python3
"""Create signed, expiring update metadata for existing release binaries."""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile

MAX_ARTIFACT_SIZE = 512 * 1024 * 1024
ED25519_PUBLIC_DER_PREFIX = bytes.fromhex("302a300506032b6570032100")
SEMVER = re.compile(
    r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
)


def openssl(*args):
    result = subprocess.run(
        ["openssl", *args], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, timeout=30, check=False,
    )
    if result.returncode:
        # OpenSSL diagnostics may include key paths; do not echo them or key data.
        raise ValueError("OpenSSL failed; verify the Ed25519 key and OpenSSL installation")
    return result.stdout


def artifact_info(path):
    with open(path, "rb") as binary:
        before = os.fstat(binary.fileno())
        if not stat.S_ISREG(before.st_mode) or not 0 < before.st_size <= MAX_ARTIFACT_SIZE:
            raise ValueError(f"Invalid artifact size or type: {path}")
        digest = hashlib.sha256()
        count = 0
        while chunk := binary.read(1024 * 1024):
            count += len(chunk)
            if count > MAX_ARTIFACT_SIZE:
                raise ValueError(f"Artifact grew beyond size limit: {path}")
            digest.update(chunk)
        after = os.fstat(binary.fileno())
        if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
            after.st_size, after.st_mtime_ns, after.st_ctime_ns
        ) or count != before.st_size:
            raise ValueError(f"Artifact changed while hashing: {path}")
    return {"sha256": digest.hexdigest(), "size": count}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("release_dir", type=Path, help="Directory containing amd64/ and aarch64/ binaries")
    parser.add_argument("--version", required=True, help="Canonical SemVer of this release")
    parser.add_argument("--expires", required=True, help="Expiry in UTC, e.g. 2027-01-01T00:00:00Z")
    parser.add_argument("--private-key", required=True, type=Path, help="Existing unencrypted Ed25519 PEM key (never generated)")
    parser.add_argument("--public-key", required=True, type=Path, help="64-hex public key provisioned on devices")
    parser.add_argument("--channel", choices=("stable", "beta"), default="stable")
    parser.add_argument("--rollout-basis-points", type=int, default=10000, help="0..10000 cohort threshold")
    parser.add_argument("--rollout-seed", help="Stable cohort seed; defaults to release version")
    parser.add_argument("--key-generation", type=int, default=0)
    parser.add_argument("--rotation-expires", help="UTC certificate expiry for offline devices; required with --next-public-key")
    parser.add_argument("--next-public-key", type=Path, help="Authorize rotation to this hex public key at generation+1")
    parser.add_argument("--updater-revision", type=int, help="Monotonically increasing updater migration revision")
    parser.add_argument("--include-updater", action="store_true", help="Also authenticate updater migration binaries")
    parser.add_argument("--force", action="store_true", help="Replace existing local manifest/signature files")
    args = parser.parse_args()
    version = SEMVER.fullmatch(args.version)
    if not version or (version.group(4) and any(
        part.isdigit() and len(part) > 1 and part.startswith("0")
        for part in version.group(4).split(".")
    )):
        parser.error("--version must be canonical SemVer")
    if args.channel == "stable" and version.group(4):
        parser.error("Prereleases must use the beta channel")
    if not 0 <= args.rollout_basis_points <= 10000 or not 0 <= args.key_generation < 2**64 - 1:
        parser.error("Invalid rollout threshold or key generation")
    if args.include_updater and (args.updater_revision is None or not 0 < args.updater_revision < 2**64):
        parser.error("--include-updater requires positive --updater-revision")
    seed = args.rollout_seed or args.version
    if len(seed.encode("utf-8")) > 128 or not seed:
        parser.error("Rollout seed must contain 1..128 characters")
    try:
        expiry = datetime.datetime.fromisoformat(args.expires.replace("Z", "+00:00"))
        if expiry.utcoffset() != datetime.timedelta(0):
            raise ValueError("--expires must include an explicit UTC timezone")
        if expiry.microsecond or expiry <= datetime.datetime.now(datetime.timezone.utc):
            raise ValueError("--expires must be a future UTC time at whole-second precision")
        output = args.release_dir.resolve(strict=True)
        if not output.is_dir():
            raise ValueError("release_dir must be a directory")
        destinations = [output / "manifest.yaml", output / "manifest.yaml.sig"]
        if not args.force and any(path.exists() or path.is_symlink() for path in destinations):
            raise ValueError("Metadata already exists; use --force only to replace local release metadata")
        public_hex = args.public_key.read_text(encoding="ascii").strip()
        if not re.fullmatch(r"[0-9a-fA-F]{64}", public_hex):
            raise ValueError("--public-key must contain exactly 64 hexadecimal characters")
        expected_key = bytes.fromhex(public_hex)
        derived = openssl("pkey", "-in", str(args.private_key.resolve(strict=True)), "-passin", "pass:", "-pubout", "-outform", "DER")
        if derived != ED25519_PUBLIC_DER_PREFIX + expected_key:
            raise ValueError("Signing key is not Ed25519 or does not match the device public key")
        artifacts = {
            arch: artifact_info(output / arch / "druid-garden-os.app")
            for arch in ("amd64", "aarch64")
        }
        metadata = {
            "version": args.version, "expires_unix": int(expiry.timestamp()),
            "channel": args.channel, "key_generation": args.key_generation,
            "rollout_basis_points": args.rollout_basis_points, "rollout_seed": seed,
            "artifacts": artifacts,
        }
        if args.next_public_key:
            next_key = args.next_public_key.read_text(encoding="ascii").strip()
            if not re.fullmatch(r"[0-9a-fA-F]{64}", next_key) or bytes.fromhex(next_key) == expected_key:
                raise ValueError("Next public key must be a different 64-hex Ed25519 key")
            metadata["next_key"] = {"generation": args.key_generation + 1, "public_key": next_key.lower()}
            if not args.rotation_expires:
                raise ValueError("--next-public-key requires explicit --rotation-expires for offline devices")
            rotation_expiry = datetime.datetime.fromisoformat(args.rotation_expires.replace("Z", "+00:00"))
            if rotation_expiry.utcoffset() != datetime.timedelta(0) or rotation_expiry.microsecond or rotation_expiry <= expiry:
                raise ValueError("Rotation expiry must be UTC and later than release expiry")
            rotation = {"purpose": "druid-update-key-rotation-v1", "from_generation": args.key_generation,
                        "expires_unix": int(rotation_expiry.timestamp()), "next_key": metadata["next_key"]}
        if args.include_updater:
            metadata["updater_revision"] = args.updater_revision
            metadata["updater_artifacts"] = {
                arch: artifact_info(output / arch / "druid-garden-edge-updater.app")
                for arch in ("amd64", "aarch64")
            }
        # JSON is a YAML subset, readable by both serde_yaml and the stdlib bootstrap.
        raw = (json.dumps(metadata, sort_keys=True, indent=2) + "\n").encode("utf-8")
        # Sign the exact bytes written to disk. No private key material is copied.
        with tempfile.TemporaryDirectory(prefix=".sign-release-", dir=output) as scratch:
            manifest = Path(scratch) / "manifest.yaml"
            signature_file = Path(scratch) / "manifest.yaml.sig"
            manifest.write_bytes(raw)
            signature = openssl("pkeyutl", "-sign", "-rawin", "-inkey", str(args.private_key.resolve(strict=True)), "-passin", "pass:", "-in", str(manifest))
            if len(signature) != 64:
                raise ValueError("OpenSSL did not return an Ed25519 signature")
            signature_file.write_text(signature.hex() + "\n", encoding="ascii")
            for path in (manifest, signature_file):
                path.chmod(0o644)
                with path.open("rb") as handle:
                    os.fsync(handle.fileno())
            # Publish neither file to any remote. Local pair replacement fails closed
            # if interrupted; publish immutable artifacts before channel metadata.
            os.replace(signature_file, destinations[1])
            os.replace(manifest, destinations[0])
            directory_fd = os.open(output, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
        if args.next_public_key:
            certificate = (json.dumps(rotation, sort_keys=True, indent=2) + "\n").encode()
            rotation_path = output / "rotation.json"
            rotation_sig = output / "rotation.json.sig"
            if not args.force and (rotation_path.exists() or rotation_sig.exists()):
                raise ValueError("Rotation certificate already exists; use --force to replace local metadata")
            with tempfile.TemporaryDirectory(prefix=".sign-rotation-", dir=output) as scratch:
                raw_path = Path(scratch) / "rotation.json"
                raw_path.write_bytes(certificate)
                signed = openssl("pkeyutl", "-sign", "-rawin", "-inkey", str(args.private_key.resolve(strict=True)), "-passin", "pass:", "-in", str(raw_path))
                if len(signed) != 64:
                    raise ValueError("Invalid rotation signature length")
                sig_path = Path(scratch) / "rotation.json.sig"
                sig_path.write_text(signed.hex() + "\n", encoding="ascii")
                for path in (raw_path, sig_path):
                    path.chmod(0o644)
                    with path.open("rb") as handle:
                        os.fsync(handle.fileno())
                os.replace(sig_path, rotation_sig)
                os.replace(raw_path, rotation_path)
                directory_fd = os.open(output, os.O_RDONLY | os.O_DIRECTORY)
                try:
                    os.fsync(directory_fd)
                finally:
                    os.close(directory_fd)
        print(f"Signed release {args.version}: {destinations[0]}")
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        parser.exit(1, f"Signing failed: {error}\n")


if __name__ == "__main__":
    main()
