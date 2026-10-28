#!/usr/bin/env python3
"""Publish a verified release or promote its signed channel metadata without replacing binaries."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

from bootstrap_updater import verify_bundle, verify_rotation
from sign_release import artifact_info, SEMVER


def aws(*args):
    result = subprocess.run(["aws", "--cli-connect-timeout", "15", "--cli-read-timeout", "30", *args],
                            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            timeout=600)
    if result.returncode:
        raise ValueError("AWS operation failed; publication stopped")
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("release_dir", type=Path)
    parser.add_argument("--public-key", required=True, type=Path)
    parser.add_argument("--key-generation", type=int, default=0)
    parser.add_argument("--channel", required=True, choices=("stable", "beta"))
    parser.add_argument("--bucket", default="dg-os-builds")
    parser.add_argument("--publish", action="store_true", help="Actually write to the bucket; otherwise validate locally only")
    args = parser.parse_args()
    try:
        release = args.release_dir.resolve(strict=True)
        anchor = args.public_key.read_text(encoding="ascii").strip()
        metadata, _ = verify_bundle(release, anchor, args.key_generation, args.channel)
        version = metadata["version"]
        if not SEMVER.fullmatch(version):
            raise ValueError("Invalid release version")
        rotation = None
        if metadata.get("next_key"):
            rotation = verify_rotation(release, anchor, args.key_generation)
            if rotation != metadata["next_key"]:
                raise ValueError("Rotation certificate disagrees with signed manifest")
        binaries = []
        for kind, filename in (("artifacts", "druid-garden-os.app"), ("updater_artifacts", "druid-garden-edge-updater.app")):
            for architecture in ("amd64", "aarch64"):
                expected = metadata[kind][architecture]
                relative = f"{architecture}/{filename}"
                if artifact_info(release / relative) != expected:
                    raise ValueError(f"Local signed artifact mismatch: {relative}")
                binaries.append((relative, expected))
        if not args.publish:
            print(f"Verified {version} for {args.channel}; no network or publication performed")
            return
        # A failed list is an error, never evidence that the version does not exist.
        listing = json.loads(aws("s3api", "list-objects-v2", "--bucket", args.bucket,
                                 "--prefix", f"{version}/", "--max-keys", "1", "--output", "json"))
        if listing.get("KeyCount", 0):
            # Same bytes can be promoted between streams or assigned a larger cohort.
            # Download a bounded range and independently check every existing binary.
            with tempfile.TemporaryDirectory(prefix="verify-published-release-") as scratch:
                for relative, expected in binaries:
                    downloaded = Path(scratch) / "artifact"
                    aws("s3api", "get-object", "--bucket", args.bucket, "--key", f"{version}/{relative}",
                        "--range", f"bytes=0-{expected['size']}", str(downloaded))
                    if artifact_info(downloaded) != expected:
                        raise ValueError(f"Immutable published artifact differs: {relative}")
                    downloaded.unlink()
        else:
            aws("s3", "sync", str(release) + "/", f"s3://{args.bucket}/{version}/",
                "--checksum-algorithm", "CRC32", "--cache-control", "public,max-age=31536000,immutable")
        if rotation is not None:
            # Retain per-generation certificates so devices missing the transition
            # can authenticate the new signer when they next reconnect.
            prefix = f"trust/{args.key_generation}/"
            listing = json.loads(aws("s3api", "list-objects-v2", "--bucket", args.bucket,
                                     "--prefix", prefix, "--max-keys", "1", "--output", "json"))
            if listing.get("KeyCount", 0):
                with tempfile.TemporaryDirectory(prefix="verify-published-rotation-") as scratch:
                    for name in ("rotation.json", "rotation.json.sig"):
                        expected = (release / name).read_bytes()
                        destination = Path(scratch) / name
                        aws("s3api", "get-object", "--bucket", args.bucket, "--key", prefix + name,
                            "--range", f"bytes=0-{len(expected)}", str(destination))
                        if destination.read_bytes() != expected:
                            raise ValueError("Immutable rotation certificate differs; refusing replacement")
            else:
                for name in ("rotation.json", "rotation.json.sig"):
                    aws("s3", "cp", str(release / name), f"s3://{args.bucket}/{prefix}{name}",
                        "--checksum-algorithm", "CRC32", "--cache-control", "public,max-age=31536000,immutable")
        # Separate-object publication deliberately fails signature verification during
        # an interrupted transition; clients retry and never install mixed metadata.
        for filename in ("manifest.yaml.sig", "manifest.yaml"):
            aws("s3", "cp", str(release / filename), f"s3://{args.bucket}/channels/{args.channel}/{filename}",
                "--checksum-algorithm", "CRC32", "--cache-control", "no-store")
        print(f"Published verified channel {args.channel} release {version}")
    except (OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        parser.exit(1, f"Release publication failed: {error}\n")


if __name__ == "__main__":
    main()
