# tools/tflite

`phonefarm bench` 用的 TFLite `benchmark_model` 真机二进制 (Android arm64, 官方 nightly 预编译,
内置 GPU Delegate 与 delegate 级算子 profiler)。二进制不入库, 运行 `./fetch.sh` 拉取;
也可用 `PF_BENCH_BIN=/path/to/benchmark_model` 或 `--bin` 指定。

2026-09-09 取用版本: nightly latest, 7,170,632 字节, BuildID 494b13980e2bbbcae0fd14d07045c8e0;
在 NX809J (Android 16, Adreno 840) 上验证 OpenCL 后端可用。
