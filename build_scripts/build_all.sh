#!/bin/bash -e
SOURCE=${BASH_SOURCE[0]}
while [ -L "$SOURCE" ]; do # resolve $SOURCE until the file is no longer a symlink
  DIR=$( cd -P "$( dirname "$SOURCE" )" >/dev/null 2>&1 && pwd )
  SOURCE=$(readlink "$SOURCE")
  [[ $SOURCE != /* ]] && SOURCE=$DIR/$SOURCE # if $SOURCE was a relative symlink, we need to resolve it relative to the path where the symlink file was located
done
script_dir=$( cd -P "$( dirname "$SOURCE" )" >/dev/null 2>&1 && pwd )
source "${script_dir}/build_rpi_4b.sh"
source "${script_dir}/build_rock_4c.sh"
source "${script_dir}/build_rock_5a.sh"
source "${script_dir}/build_generic_x86_64.sh"