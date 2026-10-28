#!/usr/bin/env python3
"""Verify an offline signed updater bundle; install only with explicit --install."""
import argparse
import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import stat
import subprocess
import tempfile

MAX_BINARY = 512 * 1024 * 1024
CONFIG = Path("/etc/druid-garden")
DESTINATION = Path("/usr/bin/druid-garden-edge-updater.app")


def bounded_file(path, maximum, trusted=False):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    with os.fdopen(fd, "rb") as file:
        metadata = os.fstat(file.fileno())
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > maximum or metadata.st_nlink != 1:
            raise ValueError("Expected bounded regular file with one link")
        if trusted and (metadata.st_uid != 0 or metadata.st_mode & 0o022):
            raise ValueError("Trust anchor must be root-owned and not writable by group/others")
        data = file.read(maximum + 1)
        if len(data) > maximum:
            raise ValueError("File exceeds limit")
        return data


def atomic_write(path, data, mode):
    with tempfile.NamedTemporaryFile(prefix=".druid-migrate-", dir=path.parent, delete=False) as file:
        temporary = Path(file.name)
        try:
            file.write(data)
            file.flush()
            os.fchmod(file.fileno(), mode)
            os.fsync(file.fileno())
            os.replace(temporary, path)
            fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
        finally:
            temporary.unlink(missing_ok=True)


def command(*arguments):
    result = subprocess.run(arguments, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=60)
    if result.returncode:
        raise ValueError(f"{arguments[0]} failed")
    return result.stdout


def verify_signed_bytes(raw, signature_hex, anchor):
    if not re.fullmatch(r"[0-9a-fA-F]{128}", signature_hex):
        raise ValueError("Invalid Ed25519 signature encoding")
    if not re.fullmatch(r"[0-9a-fA-F]{64}", anchor):
        raise ValueError("Invalid Ed25519 public key encoding")
    with tempfile.TemporaryDirectory(prefix="verify-updater-") as scratch:
        directory = Path(scratch)
        (directory / "public.der").write_bytes(bytes.fromhex("302a300506032b6570032100" + anchor))
        (directory / "signature").write_bytes(bytes.fromhex(signature_hex))
        (directory / "manifest").write_bytes(raw)
        command("openssl", "pkeyutl", "-verify", "-rawin", "-pubin", "-keyform", "DER",
                "-inkey", str(directory / "public.der"), "-sigfile", str(directory / "signature"),
                "-in", str(directory / "manifest"))


def verify_rotation(directory, anchor, generation):
    raw = bounded_file(directory / "rotation.json", 4096)
    signed = bounded_file(directory / "rotation.json.sig", 256).decode("ascii").strip()
    verify_signed_bytes(raw, signed, anchor)
    certificate = json.loads(raw)
    next_key = certificate["next_key"]
    if (certificate["purpose"] != "druid-update-key-rotation-v1" or certificate["from_generation"] != generation
            or type(certificate["expires_unix"]) is not int
            or certificate["expires_unix"] <= int(datetime.datetime.now(datetime.timezone.utc).timestamp())
            or next_key["generation"] != generation + 1
            or not re.fullmatch(r"[0-9a-fA-F]{64}", next_key["public_key"])
            or next_key["public_key"].lower() == anchor.lower()):
        raise ValueError("Invalid or expired rotation certificate")
    return next_key


def verify_bundle(release, anchor, expected_generation, channel):
    raw = bounded_file(release / "manifest.yaml", 65536)
    signature_hex = bounded_file(release / "manifest.yaml.sig", 256).decode("ascii").strip()
    verify_signed_bytes(raw, signature_hex, anchor)
    # Parse only authenticated metadata; signer emits JSON, a valid YAML subset.
    metadata = json.loads(raw)
    if type(metadata["expires_unix"]) is not int or metadata["expires_unix"] <= int(datetime.datetime.now(datetime.timezone.utc).timestamp()):
        raise ValueError("Release metadata expired")
    if metadata["channel"] != channel or metadata["key_generation"] != expected_generation:
        raise ValueError("Release channel/trust generation mismatch")
    revision = metadata["updater_revision"]
    if type(revision) is not int or not 0 < revision < 2**64:
        raise ValueError("Invalid updater migration revision")
    architecture = {"x86_64": "amd64", "aarch64": "aarch64"}.get(platform.machine())
    if not architecture:
        raise ValueError("Unsupported device architecture")
    artifact = metadata["updater_artifacts"][architecture]
    size = artifact["size"]
    if type(size) is not int or not 0 < size <= MAX_BINARY or not re.fullmatch(r"[0-9a-fA-F]{64}", artifact["sha256"]):
        raise ValueError("Invalid updater artifact metadata")
    payload = bounded_file(release / architecture / "druid-garden-edge-updater.app", MAX_BINARY)
    if len(payload) != size or hashlib.sha256(payload).hexdigest() != artifact["sha256"].lower():
        raise ValueError("Updater artifact failed signed size/digest verification")
    return metadata, payload


def quiesce_updater():
    # Older images may not have a timer. Stop an existing timer before inspecting
    # the legacy service, which does not participate in our advisory update lock.
    timer = "druid_garden_edge_updater.timer"
    state = command("/usr/bin/systemctl", "show", "--property=LoadState", "--value", timer).decode().strip()
    if state != "not-found":
        command("/usr/bin/systemctl", "stop", timer)
        command("/usr/bin/systemctl", "disable", timer)
    substate = command("/usr/bin/systemctl", "show", "--property=SubState", "--value", "druid_garden_edge_updater.service").decode().strip()
    if substate not in ("dead", "exited", "failed"):
        raise ValueError("Updater service is running; its timer is stopped; wait for it to finish and retry")
    command("/usr/bin/systemctl", "stop", "druid_garden_edge_updater.service")
    command("/usr/bin/systemctl", "disable", "druid_garden_edge_updater.service")


def install_schedule(units):
    recovery = b"[Unit]\nDescription=Recover interrupted Garden OS update\nBefore=druid_garden_os.service\n[Service]\nType=oneshot\nExecStart=/usr/bin/druid-garden-edge-updater.app --recover-only\nUMask=0077\nRemainAfterExit=yes\nTimeoutStartSec=2min\n"
    service = b"[Unit]\nDescription=Run Druid Garden Updater Service\nAfter=druid_garden_os.service network-online.target\nWants=network-online.target\n[Service]\nType=oneshot\nExecStart=/usr/bin/druid-garden-edge-updater.app\nUMask=0077\nTimeoutStartSec=15min\n"
    timer = b"[Unit]\nDescription=Check for signed Druid Garden OS updates\n[Timer]\nOnBootSec=15min\nOnUnitActiveSec=6h\nRandomizedDelaySec=15min\nUnit=druid_garden_edge_updater.service\n[Install]\nWantedBy=timers.target\n"
    for name, content in (("druid_garden_update_recovery.service", recovery),
                          ("druid_garden_edge_updater.service", service),
                          ("druid_garden_edge_updater.timer", timer)):
        atomic_write(units / name, content, 0o644)
    dropin = units / "druid_garden_os.service.d"
    dropin.mkdir(mode=0o755, exist_ok=True)
    atomic_write(dropin / "20-update-recovery.conf", b"[Unit]\nRequires=druid_garden_update_recovery.service\nAfter=druid_garden_update_recovery.service\n", 0o644)
    command("/usr/bin/systemctl", "daemon-reload")
    # Recovery is pulled in by the OS dependency. Only the jittered timer starts
    # updates automatically; never enable the updater directly at boot.
    command("/usr/bin/systemctl", "enable", "druid_garden_edge_updater.timer")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("release_dir", type=Path)
    parser.add_argument("--trusted-public-key", type=Path, help="Out-of-band bootstrap trust anchor; never take from the release bundle")
    parser.add_argument("--channel", choices=("stable", "beta"), default="stable")
    parser.add_argument("--install", action="store_true", help="Install verified binary and recovery units; requires root")
    args = parser.parse_args()
    try:
        if args.install and os.geteuid() != 0:
            raise ValueError("--install requires root")
        trust_path = CONFIG / "update-trust.yaml"
        if trust_path.exists():
            trust = json.loads(bounded_file(trust_path, 4096, trusted=args.install))
            anchor, generation = trust["public_key"], trust["generation"]
            if args.trusted_public_key:
                supplied = bounded_file(args.trusted_public_key, 128, trusted=args.install).decode("ascii").strip()
                if supplied.lower() != anchor.lower():
                    raise ValueError("Explicit key cannot override existing device trust")
        else:
            existing = CONFIG / "update-public-key"
            key_path = existing if existing.exists() else args.trusted_public_key
            if key_path is None:
                raise ValueError("Provision a trusted public key out of band before migration")
            anchor = bounded_file(key_path, 128, trusted=args.install).decode("ascii").strip()
            generation = 0
        metadata, payload = verify_bundle(args.release_dir, anchor, generation, args.channel)
        if not args.install:
            print(f"Verified updater revision {metadata['updater_revision']}; no device changes made")
            return
        lock_fd = os.open("/run/druid-garden-update.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
        with os.fdopen(lock_fd, "r+b") as lock:
            info = os.fstat(lock.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022 or info.st_nlink != 1:
                raise ValueError("Invalid updater lock file")
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            # Verification happened before taking the lock: recheck trust and expiry
            # so a concurrent key rotation cannot authorize an obsolete signer.
            if trust_path.exists():
                current = json.loads(bounded_file(trust_path, 4096, trusted=True))
                if current["generation"] != generation or current["public_key"].lower() != anchor.lower():
                    raise ValueError("Device trust changed during verification; retry")
            elif (CONFIG / "update-public-key").exists():
                current_key = bounded_file(CONFIG / "update-public-key", 128, trusted=True).decode("ascii").strip()
                if generation != 0 or current_key.lower() != anchor.lower():
                    raise ValueError("Device trust changed during verification; retry")
            if metadata["expires_unix"] <= int(datetime.datetime.now(datetime.timezone.utc).timestamp()):
                raise ValueError("Release expired during verification")
            if Path("/var/lib/druid-garden-updater/transaction.yaml").exists():
                raise ValueError("Recover the pending OS transaction before migrating the updater")
            CONFIG.mkdir(mode=0o755, exist_ok=True)
            info = CONFIG.lstat()
            if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
                raise ValueError("Unsafe updater configuration directory")
            revision_path = CONFIG / "updater-migration-revision"
            previous = int(bounded_file(revision_path, 64, trusted=True)) if revision_path.exists() else 0
            revision = metadata["updater_revision"]
            if revision < previous:
                raise ValueError("Updater downgrade refused")
            if revision == previous and (not DESTINATION.exists() or hashlib.sha256(bounded_file(DESTINATION, MAX_BINARY, trusted=True)).digest() != hashlib.sha256(payload).digest()):
                raise ValueError("Existing revision has different content")
            quiesce_updater()
            if DESTINATION.exists():
                atomic_write(DESTINATION.with_suffix(".app.bootstrap-backup"), bounded_file(DESTINATION, MAX_BINARY, trusted=True), 0o755)
            atomic_write(DESTINATION, payload, 0o755)
            if not (CONFIG / "update-public-key").exists():
                atomic_write(CONFIG / "update-public-key", (anchor + "\n").encode(), 0o644)
            atomic_write(CONFIG / "update-channel", (args.channel + "\n").encode(), 0o644)
            atomic_write(revision_path, (str(revision) + "\n").encode(), 0o600)
            install_schedule(Path("/etc/systemd/system"))
        # Activate scheduling only after releasing the updater lock.
        command("/usr/bin/systemctl", "start", "druid_garden_edge_updater.timer")
        print(f"Installed verified updater revision {revision}; OS service was not restarted")
    except (OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        parser.exit(1, f"Updater migration failed: {error}\n")


if __name__ == "__main__":
    main()
