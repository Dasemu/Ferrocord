#!/bin/sh
# Fails unless the given binary links only base-system libraries: glibc
# (libc, libm, ld-linux), the GCC runtime every glibc system has
# (libgcc_s) and ALSA's libasound (the sound API on every Linux desktop;
# the main binary needs it too). No codec, GStreamer or C++ runtime.
set -eu
bin="$1"
ldd "$bin"
bad=$(ldd "$bin" | awk '{print $1}' | grep -Ev '^(linux-vdso\.so|libc\.so|libm\.so|libdl\.so|libpthread\.so|librt\.so|libgcc_s\.so|libasound\.so|/lib6?4?/ld-linux|/lib/ld-linux|ld-linux)' || true)
if [ -n "$bad" ]; then
    echo "::error::$bin links libraries beyond the base system:"
    echo "$bad"
    exit 1
fi
echo "OK: $bin needs only glibc, libgcc_s and libasound"
