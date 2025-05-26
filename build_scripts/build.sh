echo "Setting Up Script Runtime Dirs"
run_dir=$(pwd)
SOURCE=${BASH_SOURCE[0]}
while [ -L "$SOURCE" ]; do # resolve $SOURCE until the file is no longer a symlink
  DIR=$( cd -P "$( dirname "$SOURCE" )" >/dev/null 2>&1 && pwd )
  SOURCE=$(readlink "$SOURCE")
  [[ $SOURCE != /* ]] && SOURCE=$DIR/$SOURCE # if $SOURCE was a relative symlink, we need to resolve it relative to the path where the symlink file was located
done
script_dir=$( cd -P "$( dirname "$SOURCE" )" >/dev/null 2>&1 && pwd )
root_dir=$( cd -P "${script_dir}/../" >/dev/null 2>&1 && pwd )
echo "${root_dir}"
if [ ! -d "${root_dir}/builds" ]; then
    mkdir -p "${root_dir}/builds"
fi
build_dir=$( cd -P "${root_dir}/builds" >/dev/null 2>&1 && pwd )
if [ ! -d "${root_dir}/build_output" ]; then
    mkdir -p "${root_dir}/build_output"
fi
output_dir=$( cd -P "${root_dir}/build_output" >/dev/null 2>&1 && pwd )
#Common Script Interface
echo "Validating Setup Environment"
source "${script_dir}/include/validate_environment.sh"
sudo echo "Elevating Permissions"
echo "Configuring Build DIR"
source "${script_dir}/include/configure_build_dir.sh"
echo "Configuring Build Overlays"
source "${script_dir}/include/configure_overlays.sh"
echo "Copying Customization Script"
cp "${script_dir}/include/customize_image.sh" "${build_dir}/userpatches/customize-image.sh"
sudo chmod +x "${build_dir}/userpatches/customize-image.sh"
echo "Building DG-OS for ${ARMBIAN_BOARD}"
cd "${build_dir}" || exit
export PREFER_DOCKER=yes
./compile.sh BOARD=${ARMBIAN_BOARD} BRANCH="${ARMBIAN_BRANCH}" RELEASE="${ARMBIAN_RELEASE}" \
  BUILD_MINIMAL=yes BUILD_DESKTOP=no KERNEL_CONFIGURE=no NETWORKING_STACK="network-manager" COMPRESS_OUTPUTIMAGE=sha,gz
# shellcheck disable=SC2086
# shellcheck disable=SC2012
image_name=$(find /home/luna/Galactechs/nevergreen-os/builds/output/images/ \
  -maxdepth 1 -type f -name ${ARMBIAN_IMAGE_PATTERN} -printf '%T@ %f\n' \
  | sort -nr \
  | head -n 1 \
  | cut -d' ' -f2-)
echo "Copying Image to output Folder"
cp "${build_dir}/output/images/${image_name}" "${output_dir}/${OUTPUT_FILE_NAME}"
echo "Zipping Image"
gzip -f "${output_dir}/${OUTPUT_FILE_NAME}"
cd "${run_dir}" || exit