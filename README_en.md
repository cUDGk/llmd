<div align="center">

# llmd

### A supervisor that keeps a local LLM resident on Android and its OpenAI-compatible API alive

[![Rust](https://img.shields.io/badge/Rust-000000?style=flat&logo=rust&logoColor=white)](Cargo.toml)
[![Android](https://img.shields.io/badge/Android-3DDC84?style=flat&logo=android&logoColor=white)](#requirements)
[![llama.cpp](https://img.shields.io/badge/backend-llama.cpp-blue?style=flat)](https://github.com/ggml-org/llama.cpp)
[![Root](https://img.shields.io/badge/Root-required-critical?style=flat)](#requirements)
[![License: MIT](https://img.shields.io/badge/License-MIT-green?style=flat)](LICENSE)

**Auto-restarts even when killed by OOM — a local LLM any app can hit, any time.**

[日本語](README.md)

---

</div>

## Overview

llama.cpp's `llama-server` speaks the OpenAI API, so any app on the device can hit `127.0.0.1:PORT/v1/chat/completions`. The hard part is keeping it **alive**: Android's low-memory killer reaps large background processes, and there's no service manager for a raw binary.

llmd is that service manager. It launches `llama-server` as a child, watches `/health`, and respawns it with exponential backoff whenever it dies to OOM or a crash.

## Features

| Capability | Detail |
|------------|--------|
| Resident supervision | Launches `llama-server` as a child; respawns with exponential backoff (max 30s) on death |
| Health monitoring | GETs `/health` over a raw socket (no HTTP-client dependency) |
| API as-is | Exposes llama-server's OpenAI-compatible endpoint unchanged |
| Management | Writes a pidfile; provides `status` / `stop` |

## Requirements

- **Root recommended** (to keep a large model resident)
- You supply the backend `llama-server` (built for Android) and a GGUF model
- Verified on: Android 14 / android-34 / x86_64 emulator + Qwen3-0.6B
- Real devices (aarch64) need the binaries rebuilt

## Build

Needs Rust + Android NDK + [cargo-ndk](https://github.com/bbqsrc/cargo-ndk).

```bash
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/<version>
cargo ndk -t x86_64 --platform 34 build --release      # emulator
cargo ndk -t arm64-v8a --platform 34 build --release   # real device
```

Build `llama-server` itself by cross-compiling llama.cpp for Android (`GGML_NATIVE=OFF`, `LLAMA_BUILD_SERVER=ON`, etc.).

## Usage

```bash
adb push target/x86_64-linux-android/release/llmd /data/local/tmp/
# also place llama-server and model.gguf under /data/local/tmp/

# run resident (everything after `--` is forwarded to llama-server)
adb shell "setsid /data/local/tmp/llmd run \
  --server /data/local/tmp/llama-server --model /data/local/tmp/model.gguf --port 8080 \
  --log /data/local/tmp/llmd.log -- -ngl 0 -c 2048 </dev/null >/dev/null 2>&1 &"

# check status
adb shell /data/local/tmp/llmd status

# inference (after `adb forward tcp:8080 tcp:8080`, from the host)
curl http://127.0.0.1:8080/v1/chat/completions -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","content":"hello"}]}'

# stop
adb shell /data/local/tmp/llmd stop
```

Run with no arguments to print usage.

## Attribution

Launches the `llama-server` from [llama.cpp](https://github.com/ggml-org/llama.cpp) (MIT) under supervision. llama.cpp itself is not bundled.

## License

MIT License — see [LICENSE](LICENSE).
