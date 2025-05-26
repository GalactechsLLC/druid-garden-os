#!/bin/bash -e
SOURCE=${BASH_SOURCE[0]}
while [ -L "$SOURCE" ]; do # resolve $SOURCE until the file is no longer a symlink
  DIR=$( cd -P "$( dirname "$SOURCE" )" >/dev/null 2>&1 && pwd )
  SOURCE=$(readlink "$SOURCE")
  [[ $SOURCE != /* ]] && SOURCE=$DIR/$SOURCE # if $SOURCE was a relative symlink, we need to resolve it relative to the path where the symlink file was located
done
script_dir=$( cd -P "$( dirname "$SOURCE" )" >/dev/null 2>&1 && pwd )
#RPI Specific Variables
export TARGET_ARCH="linux/arm64"
export ARMBIAN_BRANCH="current"
export ARMBIAN_RELEASE="noble"
export ARMBIAN_BOARD="rockpi-4cplus"
export ARMBIAN_IMAGE_PATTERN="*4cplus*.img"
export OUTPUT_FILE_NAME="druid_garden_os_rock_4c_noble.img"
#######################
source "${script_dir}/build.sh"