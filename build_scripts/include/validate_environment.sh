#!/usr/bin/env bash
set -euo pipefail
# Env validation
if [ -z "${TARGET_ARCH:-}" ]; then
 echo "Required Variable TARGET_ARCH Not Found"
 exit 1
fi
if [ -z "${ARMBIAN_BRANCH:-}" ]; then
 echo "Required Variable ARMBIAN_BRANCH Not Found"
 exit 1
fi
if [ -z "${ARMBIAN_RELEASE:-}" ]; then
 echo "Required Variable ARMBIAN_RELEASE Not Found"
 exit 1
fi
if [ -z "${ARMBIAN_BOARD:-}" ]; then
 echo "Required Variable ARMBIAN_BOARD Not Found"
 exit 1
fi
if [ -z "${ARMBIAN_IMAGE_PATTERN:-}" ]; then
 echo "Required Variable ARMBIAN_IMAGE_PATTERN Not Found"
 exit 1
fi
if [ -z "${OUTPUT_FILE_NAME:-}" ]; then
 echo "Required Variable OUTPUT_FILE_NAME Not Found"
 exit 1
fi

# Until physical-presence enrollment is implemented, an administrator needs SSH
# access to retrieve the per-device bootstrap password. Never build an image
# with both locked accounts and no supported provisioning path.
if [[ -z "${DG_SSH_AUTHORIZED_KEYS_FILE:-}" ]]; then
  echo "Set DG_SSH_AUTHORIZED_KEYS_FILE to provision initial administrator access" >&2
  exit 1
fi
if [[ -n "${DG_SSH_AUTHORIZED_KEYS_FILE:-}" ]]; then
  if [[ ! -f "$DG_SSH_AUTHORIZED_KEYS_FILE" || ! -s "$DG_SSH_AUTHORIZED_KEYS_FILE" ]]; then
    echo "DG_SSH_AUTHORIZED_KEYS_FILE must name a nonempty public authorized_keys file" >&2
    exit 1
  fi
  DG_SSH_AUTHORIZED_KEYS_FILE=$(realpath "$DG_SSH_AUTHORIZED_KEYS_FILE")
  export DG_SSH_AUTHORIZED_KEYS_FILE
fi

DG_UPDATE_KEY_GENERATION=${DG_UPDATE_KEY_GENERATION:-0}
if [[ ! "$DG_UPDATE_KEY_GENERATION" =~ ^(0|[1-9][0-9]{0,17})$ ]]; then
  echo "DG_UPDATE_KEY_GENERATION must be a canonical nonnegative integer of at most 18 digits" >&2
  exit 1
fi
export DG_UPDATE_KEY_GENERATION
if [[ -n "${DG_UPDATE_PUBLIC_KEY_FILE:-}" ]]; then
  if [[ ! -f "$DG_UPDATE_PUBLIC_KEY_FILE" || ! -s "$DG_UPDATE_PUBLIC_KEY_FILE" ]]; then
    echo "DG_UPDATE_PUBLIC_KEY_FILE must contain the trusted Ed25519 public key" >&2
    exit 1
  fi
  if [[ ! $(tr -d '\r\n' < "$DG_UPDATE_PUBLIC_KEY_FILE") =~ ^[[:xdigit:]]{64}$ ]]; then
    echo "DG_UPDATE_PUBLIC_KEY_FILE must contain exactly 64 hexadecimal characters" >&2
    exit 1
  fi
  DG_UPDATE_PUBLIC_KEY_FILE=$(realpath "$DG_UPDATE_PUBLIC_KEY_FILE")
  export DG_UPDATE_PUBLIC_KEY_FILE
fi
