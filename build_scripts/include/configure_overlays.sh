#!/usr/bin/env bash
set -euo pipefail
#Move back to Root Directory
cd "${root_dir}/" || exit
OUTPUT_DIR="${build_dir}/userpatches/overlay/"
OUTPUT_FILE="druid-garden-os.app"
OUTPUT_UPDATER_FILE="druid-garden-edge-updater.app"

#Set up docker for cross builds
docker buildx build \
  -f "${root_dir}/Dockerfile" \
  --platform linux/amd64 \
  --progress=plain \
  --target=bins -o "${OUTPUT_DIR}" .

if [ "${TARGET_ARCH}" == "linux/amd64" ]; then
  chmod 755 "${OUTPUT_DIR}/amd64/${OUTPUT_FILE}"
  chmod 755 "${OUTPUT_DIR}/amd64/${OUTPUT_UPDATER_FILE}"
  cp "${OUTPUT_DIR}/amd64/${OUTPUT_FILE}" "${OUTPUT_DIR}/${OUTPUT_FILE}"
  cp "${OUTPUT_DIR}/amd64/${OUTPUT_UPDATER_FILE}" "${OUTPUT_DIR}/${OUTPUT_UPDATER_FILE}"
else
  chmod 755 "${OUTPUT_DIR}/aarch64/${OUTPUT_FILE}"
  chmod 755 "${OUTPUT_DIR}/aarch64/${OUTPUT_UPDATER_FILE}"
  cp "${OUTPUT_DIR}/aarch64/${OUTPUT_FILE}" "${OUTPUT_DIR}/${OUTPUT_FILE}"
  cp "${OUTPUT_DIR}/aarch64/${OUTPUT_UPDATER_FILE}" "${OUTPUT_DIR}/${OUTPUT_UPDATER_FILE}"
fi
rm -rf "${OUTPUT_DIR}/aarch64"
rm -rf "${OUTPUT_DIR}/amd64"

# Never carry a previous build's provisioning keys into a new image.
rm -f "${OUTPUT_DIR}/druid-authorized-keys"
if [[ -n "${DG_SSH_AUTHORIZED_KEYS_FILE:-}" ]]; then
  install -m 0600 "$DG_SSH_AUTHORIZED_KEYS_FILE" "${OUTPUT_DIR}/druid-authorized-keys"
fi

rm -f "${OUTPUT_DIR}/druid-update-public-key" "${OUTPUT_DIR}/druid-update-key-generation"
if [[ -n "${DG_UPDATE_PUBLIC_KEY_FILE:-}" ]]; then
  install -m 0644 "$DG_UPDATE_PUBLIC_KEY_FILE" "${OUTPUT_DIR}/druid-update-public-key"
  printf '%s\n' "${DG_UPDATE_KEY_GENERATION:-0}" > "${OUTPUT_DIR}/druid-update-key-generation"
fi
