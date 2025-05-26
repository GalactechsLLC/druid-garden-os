if [ -z "$build_dir" ]; then
 echo "build_dir Not set"
 exit 1
fi

if [ -f "${build_dir}/compile.sh" ]; then
  echo "Already have armbian_build..."
else
  git clone --depth=1 --branch=v25.05 https://github.com/armbian/build "${build_dir}"
fi