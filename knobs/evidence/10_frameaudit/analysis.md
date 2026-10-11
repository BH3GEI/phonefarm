## fa_1.json

swapchain 2141x969，抓到 3 帧，时间戳 开（可用 528 个，周期 52.083 ns），诊断 {"cb_unseen":0,"dyn_passes":0,"ev_overflow":0,"sec_merged":0,"submit2":0,"ts_skipped":0}

### 每帧统计

```json
[
  {
    "draws": 484,
    "empty_passes": 0,
    "events": 67,
    "frame": 0,
    "kinds": {
      "buf2img": 2,
      "discard": 12,
      "dispatch": 20,
      "pass": 33
    },
    "load_attachments": 12,
    "model_load_mb": 41.59,
    "model_store_mb": 141.87,
    "pass_ms_sum": 27.188,
    "passes": 33,
    "passes_timed": 33
  },
  {
    "draws": 521,
    "empty_passes": 0,
    "events": 67,
    "frame": 1,
    "kinds": {
      "buf2img": 2,
      "discard": 12,
      "dispatch": 20,
      "pass": 33
    },
    "load_attachments": 12,
    "model_load_mb": 41.59,
    "model_store_mb": 141.87,
    "pass_ms_sum": 27.394,
    "passes": 33,
    "passes_timed": 33
  },
  {
    "draws": 484,
    "empty_passes": 0,
    "events": 67,
    "frame": 2,
    "kinds": {
      "buf2img": 2,
      "discard": 12,
      "dispatch": 20,
      "pass": 33
    },
    "load_attachments": 12,
    "model_load_mb": 41.59,
    "model_store_mb": 141.87,
    "pass_ms_sum": 0.0,
    "passes": 33,
    "passes_timed": 0
  }
]
```

### 疑似多余汇总（每帧平均）

| 规则 | 条数/帧 | 模型字节 MB/帧 | 实测 ms/帧 |
|---|---|---|---|
| dead_pass(疑似) | 3.0 | 8.81 | 0.560 |
| dead_store | 3.0 | 2.46 | 0.000 |
| dead_store(疑似) | 1.0 | 0.07 | 0.000 |
| transient_ok | 2.5 | 1.23 | 0.000 |

### 第 0 帧步骤清单

| # | 步骤 | 尺寸 | 附件 (角色:格式#图 load/store) | draw | 采样引用 | 模型读/写 KB | GPU ms |
|---|---|---|---|---|---|---|---|
| 0 | dispatch | [1, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 1 | dispatch | [1, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 2 | dispatch | [10, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 3 | dispatch | [56, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 4 | dispatch | [174, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 5 | dispatch | [46, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 6 | dispatch | [36, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 7 | dispatch | [17, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 8 | dispatch | [31, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 9 | dispatch | [36, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 10 | dispatch | [10, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 11 | dispatch | [10, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 12 | dispatch | [5, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 13 | dispatch | [2, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 14 | dispatch | [21, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 15 | dispatch | [8, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 16 | dispatch | [8, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 17 | pass rp62<br>`0e9c2729bbf28f99` | 128x128 | C:R11G11B10F#2971 DONT_CARE/STORE | 1 | 1 | 0 / 66 | 0.040 |
| 18 | pass rp22<br>`fd73b99ccf776147` | 128x128 | C:RGBA8#1934 LOAD/STORE | 1 | 3 | 66 / 66 | 0.032 |
| 19 | pass rp71<br>`9d0b4db80bf552b8` | 256x256 | C:R16F#2986 DONT_CARE/STORE<br>C:RGBA8#2987 DONT_CARE/STORE | 1 | 2 | 0 / 393 | 0.042 |
| 20 | pass rp92<br>`986b68b70bfd5a72` | 1070x484 | C:RGBA8#2988 CLEAR/STORE<br>C:RGBA8#2990 CLEAR/STORE<br>D:D24S8#2989 CLEAR/STORE s:CLEAR/STORE | 3 | 18 | 0 / 6215 | 0.495 |
| 21 | discard | 0x0 | src - → dst 图#2993 2140x968 | - | - | - | - |
| 22 | discard | 0x0 | src - → dst 图#2992 2140x968 | - | - | - | - |
| 23 | discard | 0x0 | src - → dst 图#3288 2140x968 | - | - | - | - |
| 24 | discard | 0x0 | src - → dst 图#2991 2140x968 | - | - | - | - |
| 25 | discard | 0x0 | src - → dst 图#2913 2140x968 | - | - | - | - |
| 26 | pass rp107<br>`590ca693387e4e8f` | 2140x968 | C:RGBA8#2991 CLEAR/STORE<br>C:RGBA8_SRGB#2992 CLEAR/STORE<br>C:RGBA8_SRGB#2993 CLEAR/STORE<br>C:A2RGB10#3288 CLEAR/STORE<br>D:D24S8#2913 CLEAR/STORE s:CLEAR/STORE | 205 | 268 | 0 / 41430 | 6.112 |
| 27 | discard | 0x0 | src - → dst 图#3003 1070x484 | - | - | - | - |
| 28 | discard | 0x0 | src - → dst 图#2994 1070x484 | - | - | - | - |
| 29 | discard | 0x0 | src - → dst 图#2995 1070x484 | - | - | - | - |
| 30 | pass rp30<br>`872ac5561f4e2ae6` | 1070x484 | C:R32F#2994 CLEAR/STORE<br>C:RGBA8#3003 CLEAR/STORE<br>D:D24S8#2995 CLEAR/STORE s:CLEAR/NONE | 1 | 5 | 0 / 5697 | 0.476 |
| 31 | dispatch | [128, 32, 1] | rd 1 / wr 4 | - | - | - | - |
| 32 | dispatch | [16, 4, 1] | rd 1 / wr 4 | - | - | - | - |
| 33 | dispatch | [2, 1, 1] | rd 1 / wr 4 | - | - | - | - |
| 34 | buf2img | 128x36 | src - → dst 图#2976 128x128 | - | - | - | - |
| 35 | buf2img | 128x36 | src - → dst 图#2977 128x128 | - | - | - | - |
| 36 | pass rp61<br>`ae75a3d4bc624906` | 128x128 | C:RGBA8#2975 LOAD/STORE | 1 | 5 | 66 / 66 | 0.068 |
| 37 | pass rp14<br>`0a072fce0059b44d` | 512x512 | D:D16#3001 CLEAR/STORE | 8 | 8 | 0 / 524 | 0.242 |
| 38 | pass rp31<br>`53582d61f781112e` | 1024x1024 | D:D16#3002 LOAD/STORE | 38 | 17 | 2097 / 2097 | 0.372 |
| 39 | pass rp65<br>`9f1bdf6f576d0886` | 512x512 | C:RGBA8#2984 DONT_CARE/STORE | 3 | 3 | 0 / 1049 | 0.212 |
| 40 | pass rp66<br>`e3d50419e22a3125` | 1070x484 | C:RGBA8#2996 CLEAR/STORE | 3 | 6 | 0 / 2072 | 1.326 |
| 41 | discard | 0x0 | src - → dst 图#3014 1070x484 | - | - | - | - |
| 42 | discard | 0x0 | src - → dst 图#3010 1070x484 | - | - | - | - |
| 43 | discard | 0x0 | src - → dst 图#3013 1070x484 | - | - | - | - |
| 44 | pass rp103<br>`15a51a879df99c32` | 1070x484 | C:RGBA16F#3010 CLEAR/STORE<br>C:R11G11B10F#3013 CLEAR/STORE<br>C:R32F#3014 CLEAR/STORE<br>D:D24S8#3011 CLEAR/NONE s:CLEAR/STORE | 2 | 6 | 0 / 8804 | 0.367 |
| 45 | pass rp98<br>`ae7356e317f7e574` | 1070x484 | C:RGBA16F#3012 CLEAR/STORE<br>D:D24S8#3011 DONT_CARE/NONE s:LOAD/NONE | 1 | 4 | 518 / 4143 | 0.364 |
| 46 | discard | 0x0 | src - → dst 图#3015 1070x484 | - | - | - | - |
| 47 | pass rp55<br>`a55683fdf11f70f3` | 1070x484 | C:RGBA16F#3015 CLEAR/STORE<br>D:D24S8#2995 LOAD/NONE s:CLEAR/NONE | 20 | 26 | 1554 / 4143 | 0.360 |
| 48 | pass rp114<br>`42e61e9172f926c2` | 70x5 | C:RGBA16F#3005 LOAD/STORE | 2 | 8 | 3 / 3 | 0.046 |
| 49 | pass rp114<br>`42e61e9172f926c2` | 70x1 | C:RGBA16F#3007 LOAD/STORE | 2 | 4 | 1 / 1 | 0.028 |
| 50 | pass rp110<br>`4c01dc5736fda7bd` | 2140x968 | C:R11G11B10F#2912 CLEAR/STORE<br>C:R11G11B10F#3004 CLEAR/STORE<br>D:D24S8#2913 LOAD/NONE s:LOAD/NONE | 67 | 35 | 8286 / 16572 | 9.201 |
| 51 | pass rp112<br>`309ab726665125ef` | 2140x968 | C:R8#3016 DONT_CARE/NONE<br>D:D24S8#2913 LOAD/NONE s:LOAD/NONE | 11 | 1 | 8286 / 0 | 0.451 |
| 52 | pass rp60<br>`8b4e9a51c693a466` | 2140x968 | C:A2RGB10#3017 DONT_CARE/STORE<br>D:D24S8#2913 LOAD/NONE s:LOAD/NONE | 21 | 17 | 8286 / 8286 | 1.179 |
| 53 | pass rp57<br>`d6ef9fa3a209e062` | 2140x968 | C:R11G11B10F#3019 DONT_CARE/STORE<br>C:R8#3020 DONT_CARE/STORE | 1 | 7 | 0 / 10358 | 1.339 |
| 54 | pass rp61<br>`ae75a3d4bc624906` | 1070x484 | C:RGBA8#3003 LOAD/STORE | 1 | 5 | 2072 / 2072 | 0.670 |
| 55 | pass rp61<br>`ae75a3d4bc624906` | 1070x484 | C:RGBA8#2996 LOAD/STORE | 1 | 2 | 2072 / 2072 | 0.690 |
| 56 | pass rp67<br>`338c5a2c0e03e642` | 1070x484 | C:RGBA16F#3012 DONT_CARE/STORE | 1 | 3 | 0 / 4143 | 0.233 |
| 57 | pass rp44<br>`c2b9edb60aa8d203` | 1070x484 | C:R11G11B10F#3021 DONT_CARE/STORE<br>C:R8#3022 DONT_CARE/STORE | 1 | 4 | 0 / 2589 | 0.216 |
| 58 | pass rp62<br>`0e9c2729bbf28f99` | 535x242 | C:R11G11B10F#3023 DONT_CARE/STORE | 1 | 5 | 0 / 518 | 0.288 |
| 59 | pass rp62<br>`0e9c2729bbf28f99` | 535x242 | C:R11G11B10F#3025 DONT_CARE/STORE | 1 | 3 | 0 / 518 | 0.084 |
| 60 | pass rp62<br>`0e9c2729bbf28f99` | 535x242 | C:R11G11B10F#3023 DONT_CARE/STORE | 1 | 2 | 0 / 518 | 0.070 |
| 61 | pass rp48<br>`18e11847444a7f3b` | 188x158 | C:R11G11B10F#3027 CLEAR/STORE | 3 | 2 | 0 / 119 | 0.063 |
| 62 | pass rp48<br>`18e11847444a7f3b` | 188x158 | C:R11G11B10F#3026 CLEAR/STORE | 3 | 2 | 0 / 119 | 0.046 |
| 63 | pass rp48<br>`18e11847444a7f3b` | 188x158 | C:R11G11B10F#3027 CLEAR/STORE | 3 | 2 | 0 / 119 | 0.048 |
| 64 | pass rp20<br>`bf9e1161a8dd7758` | 535x242 | C:R11G11B10F#3028 DONT_CARE/STORE | 1 | 3 | 0 / 518 | 0.063 |
| 65 | pass rp118<br>`a2c3d213c4cb1f01` | 2140x968 | C:RGBA8#3029 DONT_CARE/STORE<br>D:D24S8#2913 LOAD/NONE s:LOAD/NONE | 4 | 10 | 8286 / 8286 | 0.767 |
| 66 | pass rp115<br>`d81fff0491a7e1c5` | 2141x969 | C:RGBA8#5 CLEAR/STORE<br>D:D24S8#124 CLEAR/NONE s:CLEAR/NONE | 71 | 42 | 0 / 8299 | 1.198 |

### 疑似多余明细（第 0 帧）

| # | 规则 | 确定 | 模型 KB | 说明 |
|---|---|---|---|---|
| 19 | dead_pass | 疑似 | 393 | pass rp71 256x256 sig=9d0b4db80bf552b8 产出的 2 块内容全部没人读; draw 1 笔 (不确定点: fragment 里写 storage buffer 层看不见) |
| 19 | transient_ok | 是 | 131 | 图#2986 256x256 (R16F) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 131 KB |
| 19 | transient_ok | 是 | 262 | 图#2987 256x256 (RGBA8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 262 KB |
| 19 | dead_store | 是 | 131 | 附件0 (R16F 图#2986 256x256) STORE 后下一次是覆盖/丢弃, 没人读 |
| 19 | dead_store | 是 | 262 | 附件1 (RGBA8 图#2987 256x256) STORE 后下一次是覆盖/丢弃, 没人读 |
| 36 | dead_pass | 疑似 | 131 | pass rp61 128x128 sig=ae75a3d4bc624906 产出的 1 块内容全部没人读; draw 1 笔 (不确定点: fragment 里写 storage buffer 层看不见) |
| 36 | dead_store | 疑似 | 66 | 附件0 (RGBA8 图#2975 128x128) STORE 后到抓帧结束(≥1 整帧)没人读 |
| 44 | transient_ok | 是 | 2072 | 图#3014 1070x484 (R32F) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 2072 KB |
| 44 | dead_store | 是 | 2072 | 附件2 (R32F 图#3014 1070x484) STORE 后下一次是覆盖/丢弃, 没人读 |
| 51 | dead_pass | 疑似 | 8286 | pass rp112 2140x968 sig=309ab726665125ef 产出的 0 块内容一块都不存回显存; draw 11 笔 (不确定点: fragment 里写 storage buffer 层看不见) |
| 51 | transient_ok | 是 | 0 | 图#3016 2140x968 (R8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 0 KB |
| 66 | transient_ok | 是 | 0 | 图#124 2141x969 (D24S8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 0 KB |

## fa_2.json

swapchain 2141x969，抓到 3 帧，时间戳 开（可用 528 个，周期 52.083 ns），诊断 {"cb_unseen":0,"dyn_passes":0,"ev_overflow":0,"sec_merged":0,"submit2":0,"ts_skipped":0}

### 每帧统计

```json
[
  {
    "draws": 488,
    "empty_passes": 0,
    "events": 68,
    "frame": 0,
    "kinds": {
      "buf2img": 2,
      "discard": 12,
      "dispatch": 21,
      "pass": 33
    },
    "load_attachments": 12,
    "model_load_mb": 41.59,
    "model_store_mb": 141.87,
    "pass_ms_sum": 27.53,
    "passes": 33,
    "passes_timed": 33
  },
  {
    "draws": 511,
    "empty_passes": 0,
    "events": 68,
    "frame": 1,
    "kinds": {
      "buf2img": 2,
      "discard": 12,
      "dispatch": 21,
      "pass": 33
    },
    "load_attachments": 12,
    "model_load_mb": 41.59,
    "model_store_mb": 141.87,
    "pass_ms_sum": 28.457,
    "passes": 33,
    "passes_timed": 33
  },
  {
    "draws": 488,
    "empty_passes": 0,
    "events": 68,
    "frame": 2,
    "kinds": {
      "buf2img": 2,
      "discard": 12,
      "dispatch": 21,
      "pass": 33
    },
    "load_attachments": 12,
    "model_load_mb": 41.59,
    "model_store_mb": 141.87,
    "pass_ms_sum": 0.0,
    "passes": 33,
    "passes_timed": 0
  }
]
```

### 疑似多余汇总（每帧平均）

| 规则 | 条数/帧 | 模型字节 MB/帧 | 实测 ms/帧 |
|---|---|---|---|
| dead_pass(疑似) | 3.0 | 8.81 | 0.579 |
| dead_store | 3.0 | 2.46 | 0.000 |
| dead_store(疑似) | 1.0 | 0.07 | 0.000 |
| transient_ok | 2.5 | 1.23 | 0.000 |

### 第 0 帧步骤清单

| # | 步骤 | 尺寸 | 附件 (角色:格式#图 load/store) | draw | 采样引用 | 模型读/写 KB | GPU ms |
|---|---|---|---|---|---|---|---|
| 0 | dispatch | [1, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 1 | dispatch | [1, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 2 | dispatch | [10, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 3 | dispatch | [56, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 4 | dispatch | [174, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 5 | dispatch | [46, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 6 | dispatch | [36, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 7 | dispatch | [17, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 8 | dispatch | [36, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 9 | dispatch | [10, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 10 | dispatch | [36, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 11 | dispatch | [7, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 12 | dispatch | [10, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 13 | dispatch | [5, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 14 | dispatch | [2, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 15 | dispatch | [21, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 16 | dispatch | [8, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 17 | dispatch | [8, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 18 | pass rp62<br>`0e9c2729bbf28f99` | 128x128 | C:R11G11B10F#2971 DONT_CARE/STORE | 1 | 1 | 0 / 66 | 0.062 |
| 19 | pass rp22<br>`fd73b99ccf776147` | 128x128 | C:RGBA8#1934 LOAD/STORE | 1 | 3 | 66 / 66 | 0.034 |
| 20 | pass rp71<br>`9d0b4db80bf552b8` | 256x256 | C:R16F#2986 DONT_CARE/STORE<br>C:RGBA8#2987 DONT_CARE/STORE | 1 | 2 | 0 / 393 | 0.043 |
| 21 | pass rp92<br>`986b68b70bfd5a72` | 1070x484 | C:RGBA8#2988 CLEAR/STORE<br>C:RGBA8#2990 CLEAR/STORE<br>D:D24S8#2989 CLEAR/STORE s:CLEAR/STORE | 3 | 18 | 0 / 6215 | 0.402 |
| 22 | discard | 0x0 | src - → dst 图#2993 2140x968 | - | - | - | - |
| 23 | discard | 0x0 | src - → dst 图#2992 2140x968 | - | - | - | - |
| 24 | discard | 0x0 | src - → dst 图#3288 2140x968 | - | - | - | - |
| 25 | discard | 0x0 | src - → dst 图#2991 2140x968 | - | - | - | - |
| 26 | discard | 0x0 | src - → dst 图#2913 2140x968 | - | - | - | - |
| 27 | pass rp107<br>`590ca693387e4e8f` | 2140x968 | C:RGBA8#2991 CLEAR/STORE<br>C:RGBA8_SRGB#2992 CLEAR/STORE<br>C:RGBA8_SRGB#2993 CLEAR/STORE<br>C:A2RGB10#3288 CLEAR/STORE<br>D:D24S8#2913 CLEAR/STORE s:CLEAR/STORE | 205 | 268 | 0 / 41430 | 6.207 |
| 28 | discard | 0x0 | src - → dst 图#3003 1070x484 | - | - | - | - |
| 29 | discard | 0x0 | src - → dst 图#2994 1070x484 | - | - | - | - |
| 30 | discard | 0x0 | src - → dst 图#2995 1070x484 | - | - | - | - |
| 31 | pass rp30<br>`872ac5561f4e2ae6` | 1070x484 | C:R32F#2994 CLEAR/STORE<br>C:RGBA8#3003 CLEAR/STORE<br>D:D24S8#2995 CLEAR/STORE s:CLEAR/NONE | 1 | 5 | 0 / 5697 | 0.486 |
| 32 | dispatch | [128, 32, 1] | rd 1 / wr 4 | - | - | - | - |
| 33 | dispatch | [16, 4, 1] | rd 1 / wr 4 | - | - | - | - |
| 34 | dispatch | [2, 1, 1] | rd 1 / wr 4 | - | - | - | - |
| 35 | buf2img | 128x36 | src - → dst 图#2982 128x128 | - | - | - | - |
| 36 | buf2img | 128x36 | src - → dst 图#2983 128x128 | - | - | - | - |
| 37 | pass rp61<br>`ae75a3d4bc624906` | 128x128 | C:RGBA8#2981 LOAD/STORE | 1 | 5 | 66 / 66 | 0.074 |
| 38 | pass rp14<br>`0a072fce0059b44d` | 512x512 | D:D16#3001 CLEAR/STORE | 8 | 8 | 0 / 524 | 0.231 |
| 39 | pass rp31<br>`53582d61f781112e` | 1024x1024 | D:D16#3002 LOAD/STORE | 42 | 18 | 2097 / 2097 | 0.414 |
| 40 | pass rp65<br>`9f1bdf6f576d0886` | 512x512 | C:RGBA8#2984 DONT_CARE/STORE | 3 | 3 | 0 / 1049 | 0.223 |
| 41 | pass rp66<br>`e3d50419e22a3125` | 1070x484 | C:RGBA8#2996 CLEAR/STORE | 3 | 6 | 0 / 2072 | 1.342 |
| 42 | discard | 0x0 | src - → dst 图#3014 1070x484 | - | - | - | - |
| 43 | discard | 0x0 | src - → dst 图#3010 1070x484 | - | - | - | - |
| 44 | discard | 0x0 | src - → dst 图#3013 1070x484 | - | - | - | - |
| 45 | pass rp103<br>`15a51a879df99c32` | 1070x484 | C:RGBA16F#3010 CLEAR/STORE<br>C:R11G11B10F#3013 CLEAR/STORE<br>C:R32F#3014 CLEAR/STORE<br>D:D24S8#3011 CLEAR/NONE s:CLEAR/STORE | 2 | 6 | 0 / 8804 | 0.378 |
| 46 | pass rp98<br>`ae7356e317f7e574` | 1070x484 | C:RGBA16F#3012 CLEAR/STORE<br>D:D24S8#3011 DONT_CARE/NONE s:LOAD/NONE | 1 | 4 | 518 / 4143 | 0.372 |
| 47 | discard | 0x0 | src - → dst 图#3015 1070x484 | - | - | - | - |
| 48 | pass rp55<br>`a55683fdf11f70f3` | 1070x484 | C:RGBA16F#3015 CLEAR/STORE<br>D:D24S8#2995 LOAD/NONE s:CLEAR/NONE | 20 | 26 | 1554 / 4143 | 0.370 |
| 49 | pass rp114<br>`42e61e9172f926c2` | 70x5 | C:RGBA16F#3005 LOAD/STORE | 2 | 8 | 3 / 3 | 0.047 |
| 50 | pass rp114<br>`42e61e9172f926c2` | 70x1 | C:RGBA16F#3007 LOAD/STORE | 2 | 4 | 1 / 1 | 0.030 |
| 51 | pass rp110<br>`4c01dc5736fda7bd` | 2140x968 | C:R11G11B10F#2912 CLEAR/STORE<br>C:R11G11B10F#3004 CLEAR/STORE<br>D:D24S8#2913 LOAD/NONE s:LOAD/NONE | 67 | 35 | 8286 / 16572 | 9.276 |
| 52 | pass rp112<br>`309ab726665125ef` | 2140x968 | C:R8#3016 DONT_CARE/NONE<br>D:D24S8#2913 LOAD/NONE s:LOAD/NONE | 11 | 1 | 8286 / 0 | 0.471 |
| 53 | pass rp60<br>`8b4e9a51c693a466` | 2140x968 | C:A2RGB10#3017 DONT_CARE/STORE<br>D:D24S8#2913 LOAD/NONE s:LOAD/NONE | 21 | 17 | 8286 / 8286 | 1.208 |
| 54 | pass rp57<br>`d6ef9fa3a209e062` | 2140x968 | C:R11G11B10F#3019 DONT_CARE/STORE<br>C:R8#3020 DONT_CARE/STORE | 1 | 7 | 0 / 10358 | 1.403 |
| 55 | pass rp61<br>`ae75a3d4bc624906` | 1070x484 | C:RGBA8#3003 LOAD/STORE | 1 | 5 | 2072 / 2072 | 0.662 |
| 56 | pass rp61<br>`ae75a3d4bc624906` | 1070x484 | C:RGBA8#2996 LOAD/STORE | 1 | 2 | 2072 / 2072 | 0.690 |
| 57 | pass rp67<br>`338c5a2c0e03e642` | 1070x484 | C:RGBA16F#3012 DONT_CARE/STORE | 1 | 3 | 0 / 4143 | 0.246 |
| 58 | pass rp44<br>`c2b9edb60aa8d203` | 1070x484 | C:R11G11B10F#3021 DONT_CARE/STORE<br>C:R8#3022 DONT_CARE/STORE | 1 | 4 | 0 / 2589 | 0.219 |
| 59 | pass rp62<br>`0e9c2729bbf28f99` | 535x242 | C:R11G11B10F#3023 DONT_CARE/STORE | 1 | 5 | 0 / 518 | 0.289 |
| 60 | pass rp62<br>`0e9c2729bbf28f99` | 535x242 | C:R11G11B10F#3025 DONT_CARE/STORE | 1 | 3 | 0 / 518 | 0.085 |
| 61 | pass rp62<br>`0e9c2729bbf28f99` | 535x242 | C:R11G11B10F#3023 DONT_CARE/STORE | 1 | 2 | 0 / 518 | 0.071 |
| 62 | pass rp48<br>`18e11847444a7f3b` | 188x158 | C:R11G11B10F#3027 CLEAR/STORE | 3 | 2 | 0 / 119 | 0.064 |
| 63 | pass rp48<br>`18e11847444a7f3b` | 188x158 | C:R11G11B10F#3026 CLEAR/STORE | 3 | 2 | 0 / 119 | 0.046 |
| 64 | pass rp48<br>`18e11847444a7f3b` | 188x158 | C:R11G11B10F#3027 CLEAR/STORE | 3 | 2 | 0 / 119 | 0.048 |
| 65 | pass rp20<br>`bf9e1161a8dd7758` | 535x242 | C:R11G11B10F#3028 DONT_CARE/STORE | 1 | 3 | 0 / 518 | 0.064 |
| 66 | pass rp118<br>`a2c3d213c4cb1f01` | 2140x968 | C:RGBA8#3029 DONT_CARE/STORE<br>D:D24S8#2913 LOAD/NONE s:LOAD/NONE | 4 | 10 | 8286 / 8286 | 0.766 |
| 67 | pass rp115<br>`d81fff0491a7e1c5` | 2141x969 | C:RGBA8#3 CLEAR/STORE<br>D:D24S8#124 CLEAR/NONE s:CLEAR/NONE | 71 | 42 | 0 / 8299 | 1.207 |

### 疑似多余明细（第 0 帧）

| # | 规则 | 确定 | 模型 KB | 说明 |
|---|---|---|---|---|
| 20 | dead_pass | 疑似 | 393 | pass rp71 256x256 sig=9d0b4db80bf552b8 产出的 2 块内容全部没人读; draw 1 笔 (不确定点: fragment 里写 storage buffer 层看不见) |
| 20 | transient_ok | 是 | 131 | 图#2986 256x256 (R16F) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 131 KB |
| 20 | transient_ok | 是 | 262 | 图#2987 256x256 (RGBA8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 262 KB |
| 20 | dead_store | 是 | 131 | 附件0 (R16F 图#2986 256x256) STORE 后下一次是覆盖/丢弃, 没人读 |
| 20 | dead_store | 是 | 262 | 附件1 (RGBA8 图#2987 256x256) STORE 后下一次是覆盖/丢弃, 没人读 |
| 37 | dead_pass | 疑似 | 131 | pass rp61 128x128 sig=ae75a3d4bc624906 产出的 1 块内容全部没人读; draw 1 笔 (不确定点: fragment 里写 storage buffer 层看不见) |
| 37 | dead_store | 疑似 | 66 | 附件0 (RGBA8 图#2981 128x128) STORE 后到抓帧结束(≥1 整帧)没人读 |
| 45 | transient_ok | 是 | 2072 | 图#3014 1070x484 (R32F) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 2072 KB |
| 45 | dead_store | 是 | 2072 | 附件2 (R32F 图#3014 1070x484) STORE 后下一次是覆盖/丢弃, 没人读 |
| 52 | dead_pass | 疑似 | 8286 | pass rp112 2140x968 sig=309ab726665125ef 产出的 0 块内容一块都不存回显存; draw 11 笔 (不确定点: fragment 里写 storage buffer 层看不见) |
| 52 | transient_ok | 是 | 0 | 图#3016 2140x968 (R8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 0 KB |
| 67 | transient_ok | 是 | 0 | 图#124 2141x969 (D24S8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 0 KB |

## fa_3.json

swapchain 2141x969，抓到 3 帧，时间戳 开（可用 528 个，周期 52.083 ns），诊断 {"cb_unseen":0,"dyn_passes":0,"ev_overflow":0,"sec_merged":0,"submit2":0,"ts_skipped":0}

### 每帧统计

```json
[
  {
    "draws": 496,
    "empty_passes": 0,
    "events": 69,
    "frame": 0,
    "kinds": {
      "buf2img": 2,
      "discard": 12,
      "dispatch": 22,
      "pass": 33
    },
    "load_attachments": 12,
    "model_load_mb": 41.59,
    "model_store_mb": 141.87,
    "pass_ms_sum": 27.531,
    "passes": 33,
    "passes_timed": 33
  },
  {
    "draws": 522,
    "empty_passes": 0,
    "events": 69,
    "frame": 1,
    "kinds": {
      "buf2img": 2,
      "discard": 12,
      "dispatch": 22,
      "pass": 33
    },
    "load_attachments": 12,
    "model_load_mb": 41.59,
    "model_store_mb": 141.87,
    "pass_ms_sum": 27.871,
    "passes": 33,
    "passes_timed": 33
  },
  {
    "draws": 496,
    "empty_passes": 0,
    "events": 69,
    "frame": 2,
    "kinds": {
      "buf2img": 2,
      "discard": 12,
      "dispatch": 22,
      "pass": 33
    },
    "load_attachments": 12,
    "model_load_mb": 41.59,
    "model_store_mb": 141.87,
    "pass_ms_sum": 0.0,
    "passes": 33,
    "passes_timed": 0
  }
]
```

### 疑似多余汇总（每帧平均）

| 规则 | 条数/帧 | 模型字节 MB/帧 | 实测 ms/帧 |
|---|---|---|---|
| dead_pass(疑似) | 3.0 | 8.81 | 0.581 |
| dead_store | 3.0 | 2.46 | 0.000 |
| dead_store(疑似) | 1.0 | 0.07 | 0.000 |
| transient_ok | 2.5 | 1.23 | 0.000 |

### 第 0 帧步骤清单

| # | 步骤 | 尺寸 | 附件 (角色:格式#图 load/store) | draw | 采样引用 | 模型读/写 KB | GPU ms |
|---|---|---|---|---|---|---|---|
| 0 | dispatch | [1, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 1 | dispatch | [1, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 2 | dispatch | [10, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 3 | dispatch | [56, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 4 | dispatch | [174, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 5 | dispatch | [46, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 6 | dispatch | [36, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 7 | dispatch | [17, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 8 | dispatch | [36, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 9 | dispatch | [10, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 10 | dispatch | [21, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 11 | dispatch | [8, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 12 | dispatch | [4, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 13 | dispatch | [10, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 14 | dispatch | [5, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 15 | dispatch | [2, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 16 | dispatch | [21, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 17 | dispatch | [8, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 18 | dispatch | [8, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 19 | pass rp62<br>`0e9c2729bbf28f99` | 128x128 | C:R11G11B10F#2971 DONT_CARE/STORE | 1 | 1 | 0 / 66 | 0.056 |
| 20 | pass rp22<br>`fd73b99ccf776147` | 128x128 | C:RGBA8#1934 LOAD/STORE | 1 | 3 | 66 / 66 | 0.035 |
| 21 | pass rp71<br>`9d0b4db80bf552b8` | 256x256 | C:R16F#2986 DONT_CARE/STORE<br>C:RGBA8#2987 DONT_CARE/STORE | 1 | 2 | 0 / 393 | 0.043 |
| 22 | pass rp92<br>`986b68b70bfd5a72` | 1070x484 | C:RGBA8#2988 CLEAR/STORE<br>C:RGBA8#2990 CLEAR/STORE<br>D:D24S8#2989 CLEAR/STORE s:CLEAR/STORE | 3 | 18 | 0 / 6215 | 0.498 |
| 23 | discard | 0x0 | src - → dst 图#2993 2140x968 | - | - | - | - |
| 24 | discard | 0x0 | src - → dst 图#2992 2140x968 | - | - | - | - |
| 25 | discard | 0x0 | src - → dst 图#3288 2140x968 | - | - | - | - |
| 26 | discard | 0x0 | src - → dst 图#2991 2140x968 | - | - | - | - |
| 27 | discard | 0x0 | src - → dst 图#2913 2140x968 | - | - | - | - |
| 28 | pass rp107<br>`590ca693387e4e8f` | 2140x968 | C:RGBA8#2991 CLEAR/STORE<br>C:RGBA8_SRGB#2992 CLEAR/STORE<br>C:RGBA8_SRGB#2993 CLEAR/STORE<br>C:A2RGB10#3288 CLEAR/STORE<br>D:D24S8#2913 CLEAR/STORE s:CLEAR/STORE | 210 | 270 | 0 / 41430 | 6.199 |
| 29 | discard | 0x0 | src - → dst 图#3003 1070x484 | - | - | - | - |
| 30 | discard | 0x0 | src - → dst 图#2994 1070x484 | - | - | - | - |
| 31 | discard | 0x0 | src - → dst 图#2995 1070x484 | - | - | - | - |
| 32 | pass rp30<br>`872ac5561f4e2ae6` | 1070x484 | C:R32F#2994 CLEAR/STORE<br>C:RGBA8#3003 CLEAR/STORE<br>D:D24S8#2995 CLEAR/STORE s:CLEAR/NONE | 1 | 5 | 0 / 5697 | 0.478 |
| 33 | dispatch | [128, 32, 1] | rd 1 / wr 4 | - | - | - | - |
| 34 | dispatch | [16, 4, 1] | rd 1 / wr 4 | - | - | - | - |
| 35 | dispatch | [2, 1, 1] | rd 1 / wr 4 | - | - | - | - |
| 36 | buf2img | 128x36 | src - → dst 图#2982 128x128 | - | - | - | - |
| 37 | buf2img | 128x36 | src - → dst 图#2983 128x128 | - | - | - | - |
| 38 | pass rp61<br>`ae75a3d4bc624906` | 128x128 | C:RGBA8#2981 LOAD/STORE | 1 | 5 | 66 / 66 | 0.069 |
| 39 | pass rp14<br>`0a072fce0059b44d` | 512x512 | D:D16#3001 CLEAR/STORE | 8 | 8 | 0 / 524 | 0.230 |
| 40 | pass rp31<br>`53582d61f781112e` | 1024x1024 | D:D16#3002 LOAD/STORE | 41 | 16 | 2097 / 2097 | 0.436 |
| 41 | pass rp65<br>`9f1bdf6f576d0886` | 512x512 | C:RGBA8#2984 DONT_CARE/STORE | 3 | 3 | 0 / 1049 | 0.221 |
| 42 | pass rp66<br>`e3d50419e22a3125` | 1070x484 | C:RGBA8#2996 CLEAR/STORE | 3 | 6 | 0 / 2072 | 1.338 |
| 43 | discard | 0x0 | src - → dst 图#3014 1070x484 | - | - | - | - |
| 44 | discard | 0x0 | src - → dst 图#3010 1070x484 | - | - | - | - |
| 45 | discard | 0x0 | src - → dst 图#3013 1070x484 | - | - | - | - |
| 46 | pass rp103<br>`15a51a879df99c32` | 1070x484 | C:RGBA16F#3010 CLEAR/STORE<br>C:R11G11B10F#3013 CLEAR/STORE<br>C:R32F#3014 CLEAR/STORE<br>D:D24S8#3011 CLEAR/NONE s:CLEAR/STORE | 2 | 6 | 0 / 8804 | 0.373 |
| 47 | pass rp98<br>`ae7356e317f7e574` | 1070x484 | C:RGBA16F#3012 CLEAR/STORE<br>D:D24S8#3011 DONT_CARE/NONE s:LOAD/NONE | 1 | 4 | 518 / 4143 | 0.364 |
| 48 | discard | 0x0 | src - → dst 图#3015 1070x484 | - | - | - | - |
| 49 | pass rp55<br>`a55683fdf11f70f3` | 1070x484 | C:RGBA16F#3015 CLEAR/STORE<br>D:D24S8#2995 LOAD/NONE s:CLEAR/NONE | 20 | 26 | 1554 / 4143 | 0.363 |
| 50 | pass rp114<br>`42e61e9172f926c2` | 70x5 | C:RGBA16F#3005 LOAD/STORE | 2 | 8 | 3 / 3 | 0.047 |
| 51 | pass rp114<br>`42e61e9172f926c2` | 70x1 | C:RGBA16F#3007 LOAD/STORE | 2 | 4 | 1 / 1 | 0.029 |
| 52 | pass rp110<br>`4c01dc5736fda7bd` | 2140x968 | C:R11G11B10F#2912 CLEAR/STORE<br>C:R11G11B10F#3004 CLEAR/STORE<br>D:D24S8#2913 LOAD/NONE s:LOAD/NONE | 67 | 35 | 8286 / 16572 | 9.257 |
| 53 | pass rp112<br>`309ab726665125ef` | 2140x968 | C:R8#3016 DONT_CARE/NONE<br>D:D24S8#2913 LOAD/NONE s:LOAD/NONE | 11 | 1 | 8286 / 0 | 0.463 |
| 54 | pass rp60<br>`8b4e9a51c693a466` | 2140x968 | C:A2RGB10#3017 DONT_CARE/STORE<br>D:D24S8#2913 LOAD/NONE s:LOAD/NONE | 25 | 20 | 8286 / 8286 | 1.205 |
| 55 | pass rp57<br>`d6ef9fa3a209e062` | 2140x968 | C:R11G11B10F#3019 DONT_CARE/STORE<br>C:R8#3020 DONT_CARE/STORE | 1 | 7 | 0 / 10358 | 1.365 |
| 56 | pass rp61<br>`ae75a3d4bc624906` | 1070x484 | C:RGBA8#3003 LOAD/STORE | 1 | 5 | 2072 / 2072 | 0.667 |
| 57 | pass rp61<br>`ae75a3d4bc624906` | 1070x484 | C:RGBA8#2996 LOAD/STORE | 1 | 2 | 2072 / 2072 | 0.690 |
| 58 | pass rp67<br>`338c5a2c0e03e642` | 1070x484 | C:RGBA16F#3012 DONT_CARE/STORE | 1 | 3 | 0 / 4143 | 0.241 |
| 59 | pass rp44<br>`c2b9edb60aa8d203` | 1070x484 | C:R11G11B10F#3021 DONT_CARE/STORE<br>C:R8#3022 DONT_CARE/STORE | 1 | 4 | 0 / 2589 | 0.222 |
| 60 | pass rp62<br>`0e9c2729bbf28f99` | 535x242 | C:R11G11B10F#3023 DONT_CARE/STORE | 1 | 5 | 0 / 518 | 0.288 |
| 61 | pass rp62<br>`0e9c2729bbf28f99` | 535x242 | C:R11G11B10F#3025 DONT_CARE/STORE | 1 | 3 | 0 / 518 | 0.085 |
| 62 | pass rp62<br>`0e9c2729bbf28f99` | 535x242 | C:R11G11B10F#3023 DONT_CARE/STORE | 1 | 2 | 0 / 518 | 0.071 |
| 63 | pass rp48<br>`18e11847444a7f3b` | 188x158 | C:R11G11B10F#3027 CLEAR/STORE | 3 | 2 | 0 / 119 | 0.063 |
| 64 | pass rp48<br>`18e11847444a7f3b` | 188x158 | C:R11G11B10F#3026 CLEAR/STORE | 3 | 2 | 0 / 119 | 0.046 |
| 65 | pass rp48<br>`18e11847444a7f3b` | 188x158 | C:R11G11B10F#3027 CLEAR/STORE | 3 | 2 | 0 / 119 | 0.048 |
| 66 | pass rp20<br>`bf9e1161a8dd7758` | 535x242 | C:R11G11B10F#3028 DONT_CARE/STORE | 1 | 3 | 0 / 518 | 0.063 |
| 67 | pass rp118<br>`a2c3d213c4cb1f01` | 2140x968 | C:RGBA8#3029 DONT_CARE/STORE<br>D:D24S8#2913 LOAD/NONE s:LOAD/NONE | 4 | 10 | 8286 / 8286 | 0.766 |
| 68 | pass rp115<br>`d81fff0491a7e1c5` | 2141x969 | C:RGBA8#3 CLEAR/STORE<br>D:D24S8#124 CLEAR/NONE s:CLEAR/NONE | 71 | 42 | 0 / 8299 | 1.212 |

### 疑似多余明细（第 0 帧）

| # | 规则 | 确定 | 模型 KB | 说明 |
|---|---|---|---|---|
| 21 | dead_pass | 疑似 | 393 | pass rp71 256x256 sig=9d0b4db80bf552b8 产出的 2 块内容全部没人读; draw 1 笔 (不确定点: fragment 里写 storage buffer 层看不见) |
| 21 | transient_ok | 是 | 131 | 图#2986 256x256 (R16F) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 131 KB |
| 21 | transient_ok | 是 | 262 | 图#2987 256x256 (RGBA8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 262 KB |
| 21 | dead_store | 是 | 131 | 附件0 (R16F 图#2986 256x256) STORE 后下一次是覆盖/丢弃, 没人读 |
| 21 | dead_store | 是 | 262 | 附件1 (RGBA8 图#2987 256x256) STORE 后下一次是覆盖/丢弃, 没人读 |
| 38 | dead_pass | 疑似 | 131 | pass rp61 128x128 sig=ae75a3d4bc624906 产出的 1 块内容全部没人读; draw 1 笔 (不确定点: fragment 里写 storage buffer 层看不见) |
| 38 | dead_store | 疑似 | 66 | 附件0 (RGBA8 图#2981 128x128) STORE 后到抓帧结束(≥1 整帧)没人读 |
| 46 | transient_ok | 是 | 2072 | 图#3014 1070x484 (R32F) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 2072 KB |
| 46 | dead_store | 是 | 2072 | 附件2 (R32F 图#3014 1070x484) STORE 后下一次是覆盖/丢弃, 没人读 |
| 53 | dead_pass | 疑似 | 8286 | pass rp112 2140x968 sig=309ab726665125ef 产出的 0 块内容一块都不存回显存; draw 11 笔 (不确定点: fragment 里写 storage buffer 层看不见) |
| 53 | transient_ok | 是 | 0 | 图#3016 2140x968 (R8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 0 KB |
| 68 | transient_ok | 是 | 0 | 图#124 2141x969 (D24S8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 0 KB |

