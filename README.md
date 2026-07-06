<div align="center">

# llmd

### ローカル LLM を Android で常駐させ、OpenAI 互換 API を生かし続ける監督デーモン

[![Rust](https://img.shields.io/badge/Rust-000000?style=flat&logo=rust&logoColor=white)](Cargo.toml)
[![Android](https://img.shields.io/badge/Android-3DDC84?style=flat&logo=android&logoColor=white)](#動作環境)
[![llama.cpp](https://img.shields.io/badge/backend-llama.cpp-blue?style=flat)](https://github.com/ggml-org/llama.cpp)
[![Root](https://img.shields.io/badge/Root-required-critical?style=flat)](#動作環境)
[![License: MIT](https://img.shields.io/badge/License-MIT-green?style=flat)](LICENSE)

**OOM で殺されても自動で復活。他アプリからいつでも叩けるローカル LLM。**

[English](README_en.md)

---

</div>

## 概要

llama.cpp の `llama-server` は OpenAI 互換 API を話すので、端末内の任意のアプリが `127.0.0.1:PORT/v1/chat/completions` を叩ける。難しいのはそれを **生かし続ける** こと。Android の low-memory killer は大きな背景プロセスを回収し、生バイナリ用のサービスマネージャも無い。

llmd がそのサービスマネージャになる。`llama-server` を子プロセスで起動し、`/health` を監視し、OOM・クラッシュで死んだら指数バックオフで再起動する。

## 特徴

| 機能 | 内容 |
|------|------|
| 常駐監督 | `llama-server` を子で起動し、死んだら指数バックオフ（最大 30 秒）で自動復活 |
| ヘルス監視 | `/health` を raw ソケットで GET（HTTP クライアント依存なし） |
| そのまま API | llama-server の OpenAI 互換エンドポイントをそのまま公開 |
| 管理 | pidfile を書き、`status` / `stop` を提供 |

## 動作環境

- **root 推奨**（大きなモデルを常駐させるため）
- バックエンドの `llama-server`（Android 向けにビルドしたもの）と GGUF モデルは利用者が用意する
- 検証済み: Android 14 / android-34 / x86_64 エミュレータ + Qwen3-0.6B
- 実機（aarch64）は各バイナリを再ビルドが必要

## ビルド

要 Rust + Android NDK + [cargo-ndk](https://github.com/bbqsrc/cargo-ndk)。

```bash
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/<version>
cargo ndk -t x86_64 --platform 34 build --release      # エミュレータ
cargo ndk -t arm64-v8a --platform 34 build --release   # 実機
```

`llama-server` 自体は llama.cpp を Android 向けにクロスビルドする（`GGML_NATIVE=OFF`、`LLAMA_BUILD_SERVER=ON` 等）。

## 使い方

```bash
adb push target/x86_64-linux-android/release/llmd /data/local/tmp/
# llama-server と model.gguf も /data/local/tmp/ に置く

# 常駐起動（`--` 以降は llama-server にそのまま渡す）
adb shell "setsid /data/local/tmp/llmd run \
  --server /data/local/tmp/llama-server --model /data/local/tmp/model.gguf --port 8080 \
  --log /data/local/tmp/llmd.log -- -ngl 0 -c 2048 </dev/null >/dev/null 2>&1 &"

# 状態確認
adb shell /data/local/tmp/llmd status

# 推論（adb forward tcp:8080 tcp:8080 の後、ホストから）
curl http://127.0.0.1:8080/v1/chat/completions -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","content":"hello"}]}'

# 停止
adb shell /data/local/tmp/llmd stop
```

引数なしで実行すると使い方を表示する。

## Attribution

バックエンドに [llama.cpp](https://github.com/ggml-org/llama.cpp)（MIT）の `llama-server` を監督下で起動する。llama.cpp 本体は同梱しない。

## ライセンス

MIT License — 詳細は [LICENSE](LICENSE) を参照。
