# Env validation
if [ -z "${TARGET_ARCH}" ]; then
 echo "Required Variable TARGET_ARCH Not Found"
 exit 1
fi
if [ -z "${ARMBIAN_BRANCH}" ]; then
 echo "Required Variable ARMBIAN_BRANCH Not Found"
 exit 1
fi
if [ -z "${ARMBIAN_RELEASE}" ]; then
 echo "Required Variable ARMBIAN_RELEASE Not Found"
 exit 1
fi
if [ -z "${ARMBIAN_BOARD}" ]; then
 echo "Required Variable ARMBIAN_BOARD Not Found"
 exit 1
fi
if [ -z "${ARMBIAN_IMAGE_PATTERN}" ]; then
 echo "Required Variable ARMBIAN_IMAGE_PATTERN Not Found"
 exit 1
fi
if [ -z "${OUTPUT_FILE_NAME}" ]; then
 echo "Required Variable OUTPUT_FILE_NAME Not Found"
 exit 1
fi