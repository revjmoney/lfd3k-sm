#!/bin/sh
# opusic-sys builds libopus with cmake. The NDK toolchain file defaults the
# ABI to armeabi-v7a unless ANDROID_ABI is a cmake variable, and cmake-rs
# does not forward that variable. This wrapper injects it on configure
# lines. cargo-ndk exports ANDROID_ABI and ANDROID_PLATFORM.
real=${LFD3K_CMAKE:-$(command -v cmake)}
case "$1" in
  --version|--help|-version|-E|--build|--install|--find-package|--open|-P|--system-information)
    exec "$real" "$@"
    ;;
esac
if [ -n "$ANDROID_ABI" ]; then
  plat=${ANDROID_PLATFORM:-android-26}
  case "$plat" in
    android-*) ;;
    *) plat="android-$plat" ;;
  esac
  exec "$real" -DANDROID_ABI="$ANDROID_ABI" -DANDROID_PLATFORM="$plat" "$@"
fi
exec "$real" "$@"
