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
  chmod 755 "${OUTPUT_DIR}/amd64/${OUTPUT_FILE}" || echo "Warning: chmod failed for amd64 binary"
  chmod 755 "${OUTPUT_DIR}/amd64/${OUTPUT_UPDATER_FILE}" || echo "Warning: chmod failed for amd64 updater binary"
  cp "${OUTPUT_DIR}/amd64/${OUTPUT_FILE}" "${OUTPUT_DIR}/${OUTPUT_FILE}"
  cp "${OUTPUT_DIR}/amd64/${OUTPUT_UPDATER_FILE}" "${OUTPUT_DIR}/${OUTPUT_UPDATER_FILE}"
else
  chmod 755 "${OUTPUT_DIR}/aarch64/${OUTPUT_FILE}" || echo "Warning: chmod failed for aarch64 binary"
  chmod 755 "${OUTPUT_DIR}/aarch64/${OUTPUT_UPDATER_FILE}" || echo "Warning: chmod failed for aarch64 updater binary"
  cp "${OUTPUT_DIR}/aarch64/${OUTPUT_FILE}" "${OUTPUT_DIR}/${OUTPUT_FILE}"
  cp "${OUTPUT_DIR}/aarch64/${OUTPUT_UPDATER_FILE}" "${OUTPUT_DIR}/${OUTPUT_UPDATER_FILE}"
fi
rm -rf "${OUTPUT_DIR}/aarch64"
rm -rf "${OUTPUT_DIR}/amd64"