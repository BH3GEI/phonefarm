## fa_on.json

swapchain 2141x969，抓到 3 帧，时间戳 关（可用 0 个，周期 0.000 ns），诊断 {"cb_unseen":0,"dyn_passes":0,"ev_overflow":0,"q_copies":0,"q_outside":0,"sec_merged":0,"submit2":0,"ts_skipped":0}

### 每帧统计

```json
[
  {
    "draws": 496,
    "empty_passes": 0,
    "events": 74,
    "frame": 0,
    "kinds": {
      "buf2img": 2,
      "discard": 12,
      "dispatch": 27,
      "pass": 33
    },
    "load_attachments": 12,
    "model_load_mb": 41.59,
    "model_store_mb": 139.01,
    "pass_ms_sum": 0.0,
    "passes": 33,
    "passes_timed": 0,
    "query_passes": 1
  },
  {
    "draws": 542,
    "empty_passes": 0,
    "events": 74,
    "frame": 1,
    "kinds": {
      "buf2img": 2,
      "discard": 12,
      "dispatch": 27,
      "pass": 33
    },
    "load_attachments": 12,
    "model_load_mb": 41.59,
    "model_store_mb": 139.01,
    "pass_ms_sum": 0.0,
    "passes": 33,
    "passes_timed": 0,
    "query_passes": 1
  },
  {
    "draws": 498,
    "empty_passes": 0,
    "events": 74,
    "frame": 2,
    "kinds": {
      "buf2img": 2,
      "discard": 12,
      "dispatch": 27,
      "pass": 33
    },
    "load_attachments": 12,
    "model_load_mb": 41.59,
    "model_store_mb": 139.01,
    "pass_ms_sum": 0.0,
    "passes": 33,
    "passes_timed": 0,
    "query_passes": 1
  }
]
```

### 疑似多余汇总（每帧平均）

| 规则 | 条数/帧 | 模型字节 MB/帧 | 实测 ms/帧 |
|---|---|---|---|
| dead_pass(疑似) | 2.0 | 0.13 | 0.000 |
| dead_store(疑似) | 1.0 | 0.07 | 0.000 |
| transient_ok | 2.5 | 0.00 | 0.000 |

### 第 0 帧步骤清单

| # | 步骤 | 尺寸 | 附件 (角色:格式#图 load/store) | draw | 采样引用 | 模型读/写 KB | GPU ms |
|---|---|---|---|---|---|---|---|
| 0 | dispatch | [1, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 1 | dispatch | [1, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 2 | dispatch | [56, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 3 | dispatch | [10, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 4 | dispatch | [174, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 5 | dispatch | [36, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 6 | dispatch | [17, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 7 | dispatch | [31, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 8 | dispatch | [15, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 9 | dispatch | [9, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 10 | dispatch | [21, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 11 | dispatch | [8, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 12 | dispatch | [8, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 13 | dispatch | [36, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 14 | dispatch | [10, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 15 | dispatch | [21, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 16 | dispatch | [8, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 17 | dispatch | [4, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 18 | dispatch | [10, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 19 | dispatch | [5, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 20 | dispatch | [2, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 21 | dispatch | [11, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 22 | dispatch | [5, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 23 | dispatch | [2, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 24 | pass rp61<br>`0e9c2729bbf28f99` | 128x128 | C:R11G11B10F#2985 DONT_CARE/STORE | 1 | 1 | 0 / 66 | - |
| 25 | pass rp22<br>`fd73b99ccf776147` | 128x128 | C:RGBA8#1977 LOAD/STORE | 1 | 3 | 66 / 66 | - |
| 26 | pass rp72<br>`9d0b4db80bf552b8` | 256x256 | C:R16F#3000 DONT_CARE/DONT_CARE<br>C:RGBA8#3001 DONT_CARE/DONT_CARE | 1 | 2 | 0 / 0 | - |
| 27 | pass rp89<br>`986b68b70bfd5a72` | 1070x484 | C:RGBA8#3002 CLEAR/STORE<br>C:RGBA8#3017 CLEAR/STORE<br>D:D24S8#3003 CLEAR/STORE s:CLEAR/STORE | 3 | 18 | 0 / 6215 | - |
| 28 | discard | 0x0 | src - → dst 图#3031 2140x968 | - | - | - | - |
| 29 | discard | 0x0 | src - → dst 图#3006 2140x968 | - | - | - | - |
| 30 | discard | 0x0 | src - → dst 图#3007 2140x968 | - | - | - | - |
| 31 | discard | 0x0 | src - → dst 图#3005 2140x968 | - | - | - | - |
| 32 | discard | 0x0 | src - → dst 图#2926 2140x968 | - | - | - | - |
| 33 | pass rp102<br>`590ca693387e4e8f` | 2140x968 | C:RGBA8#3005 CLEAR/STORE<br>C:RGBA8_SRGB#3007 CLEAR/STORE<br>C:RGBA8_SRGB#3006 CLEAR/STORE<br>C:A2RGB10#3031 CLEAR/STORE<br>D:D24S8#2926 CLEAR/STORE s:CLEAR/STORE | 214 | 270 | 0 / 41430 | - |
| 34 | discard | 0x0 | src - → dst 图#3004 1070x484 | - | - | - | - |
| 35 | discard | 0x0 | src - → dst 图#3215 1070x484 | - | - | - | - |
| 36 | discard | 0x0 | src - → dst 图#3216 1070x484 | - | - | - | - |
| 37 | pass rp30<br>`872ac5561f4e2ae6` | 1070x484 | C:R32F#3215 CLEAR/STORE<br>C:RGBA8#3004 CLEAR/STORE<br>D:D24S8#3216 CLEAR/STORE s:CLEAR/NONE | 1 | 4 | 0 / 5697 | - |
| 38 | dispatch | [128, 32, 1] | rd 1 / wr 4 | - | - | - | - |
| 39 | dispatch | [16, 4, 1] | rd 1 / wr 4 | - | - | - | - |
| 40 | dispatch | [2, 1, 1] | rd 1 / wr 4 | - | - | - | - |
| 41 | buf2img | 128x37 | src - → dst 图#2996 128x128 | - | - | - | - |
| 42 | buf2img | 128x37 | src - → dst 图#2997 128x128 | - | - | - | - |
| 43 | pass rp60<br>`ae75a3d4bc624906` | 128x128 | C:RGBA8#2995 LOAD/STORE | 1 | 5 | 66 / 66 | - |
| 44 | pass rp14<br>`0a072fce0059b44d` | 256x256 | D:D16#3302 CLEAR/STORE | 6 | 7 | 0 / 131 | - |
| 45 | pass rp31<br>`53582d61f781112e` | 1024x1024 | D:D16#3016 LOAD/STORE | 40 | 18 | 2097 / 2097 | - |
| 46 | pass rp68<br>`9f1bdf6f576d0886` | 512x512 | C:RGBA8#2999 DONT_CARE/STORE | 3 | 3 | 0 / 1049 | - |
| 47 | pass rp69<br>`e3d50419e22a3125` | 1070x484 | C:RGBA8#4524 CLEAR/STORE | 3 | 6 | 0 / 2072 | - |
| 48 | discard | 0x0 | src - → dst 图#3028 1070x484 | - | - | - | - |
| 49 | discard | 0x0 | src - → dst 图#3024 1070x484 | - | - | - | - |
| 50 | discard | 0x0 | src - → dst 图#3027 1070x484 | - | - | - | - |
| 51 | pass rp98<br>`15a51a879df99c32` | 1070x484 | C:RGBA16F#3024 CLEAR/STORE<br>C:R11G11B10F#3027 CLEAR/STORE<br>C:R32F#3028 CLEAR/DONT_CARE<br>D:D24S8#3025 CLEAR/NONE s:CLEAR/STORE | 2 | 6 | 0 / 6732 | - |
| 52 | pass rp94<br>`ae7356e317f7e574` | 1070x484 | C:RGBA16F#3026 CLEAR/STORE<br>D:D24S8#3025 DONT_CARE/NONE s:LOAD/NONE | 1 | 4 | 518 / 4143 | - |
| 53 | discard | 0x0 | src - → dst 图#3029 1070x484 | - | - | - | - |
| 54 | pass rp55<br>`a55683fdf11f70f3` | 1070x484 | C:RGBA16F#3029 CLEAR/STORE<br>D:D24S8#3216 LOAD/NONE s:CLEAR/NONE | 20 | 26 | 1554 / 4143 | - |
| 55 | pass rp109<br>`42e61e9172f926c2` | 70x5 | C:RGBA16F#3019 LOAD/STORE | 2 | 8 | 3 / 3 | - |
| 56 | pass rp109<br>`42e61e9172f926c2` | 70x1 | C:RGBA16F#3021 LOAD/STORE | 2 | 4 | 1 / 1 | - |
| 57 | pass rp106<br>`4c01dc5736fda7bd` | 2140x968 | C:R11G11B10F#2925 CLEAR/STORE<br>C:R11G11B10F#3018 CLEAR/STORE<br>D:D24S8#2926 LOAD/NONE s:LOAD/NONE | 67 | 35 | 8286 / 16572 | - |
| 58 | pass rp110<br>`309ab726665125ef` | 2140x968 | C:R8#3030 DONT_CARE/NONE<br>D:D24S8#2926 LOAD/NONE s:LOAD/NONE | 11 | 1 | 8286 / 0 | - |
| 59 | pass rp59<br>`8b4e9a51c693a466` | 2140x968 | C:A2RGB10#3303 DONT_CARE/STORE<br>D:D24S8#2926 LOAD/NONE s:LOAD/NONE | 24 | 19 | 8286 / 8286 | - |
| 60 | pass rp64<br>`d6ef9fa3a209e062` | 2140x968 | C:R11G11B10F#3032 DONT_CARE/STORE<br>C:R8#3217 DONT_CARE/STORE | 1 | 7 | 0 / 10358 | - |
| 61 | pass rp60<br>`ae75a3d4bc624906` | 1070x484 | C:RGBA8#3004 LOAD/STORE | 1 | 5 | 2072 / 2072 | - |
| 62 | pass rp60<br>`ae75a3d4bc624906` | 1070x484 | C:RGBA8#4524 LOAD/STORE | 1 | 2 | 2072 / 2072 | - |
| 63 | pass rp70<br>`338c5a2c0e03e642` | 1070x484 | C:RGBA16F#3026 DONT_CARE/STORE | 1 | 3 | 0 / 4143 | - |
| 64 | pass rp44<br>`c2b9edb60aa8d203` | 1070x484 | C:R11G11B10F#3035 DONT_CARE/STORE<br>C:R8#3036 DONT_CARE/STORE | 1 | 4 | 0 / 2589 | - |
| 65 | pass rp61<br>`0e9c2729bbf28f99` | 535x242 | C:R11G11B10F#3037 DONT_CARE/STORE | 1 | 5 | 0 / 518 | - |
| 66 | pass rp20<br>`bf9e1161a8dd7758` | 535x242 | C:R11G11B10F#3038 DONT_CARE/STORE | 1 | 3 | 0 / 518 | - |
| 67 | pass rp61<br>`0e9c2729bbf28f99` | 535x242 | C:R11G11B10F#3037 DONT_CARE/STORE | 1 | 2 | 0 / 518 | - |
| 68 | pass rp71<br>`c9e3027f313566fa` | 188x158 | C:R11G11B10F#3040 CLEAR/STORE | 3 | 2 | 0 / 119 | - |
| 69 | pass rp48<br>`18e11847444a7f3b` | 188x158 | C:R11G11B10F#3041 CLEAR/STORE | 3 | 2 | 0 / 119 | - |
| 70 | pass rp48<br>`18e11847444a7f3b` | 188x158 | C:R11G11B10F#3040 CLEAR/STORE | 3 | 2 | 0 / 119 | - |
| 71 | pass rp20<br>`bf9e1161a8dd7758` | 535x242 | C:R11G11B10F#3042 DONT_CARE/STORE | 1 | 3 | 0 / 518 | - |
| 72 | pass rp118<br>`a2c3d213c4cb1f01` | 2140x968 | C:RGBA8#3043 DONT_CARE/STORE<br>D:D24S8#2926 LOAD/NONE s:LOAD/NONE | 4 | 10 | 8286 / 8286 | - |
| 73 | pass rp114<br>`d81fff0491a7e1c5` | 2141x969 | C:RGBA8#3 CLEAR/STORE<br>D:D24S8#124 CLEAR/NONE s:CLEAR/NONE | 71 | 42 | 0 / 8299 | - |

### 疑似多余明细（第 0 帧）

| # | 规则 | 确定 | 模型 KB | 说明 |
|---|---|---|---|---|
| 26 | dead_pass | 疑似 | 0 | pass rp72 256x256 sig=9d0b4db80bf552b8 产出的 0 块内容一块都不存回显存; draw 1 笔, pass 内无查询 (不确定点: fragment 里写 storage buffer 层看不见) |
| 26 | transient_ok | 是 | 0 | 图#3000 256x256 (R16F) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 0 KB |
| 26 | transient_ok | 是 | 0 | 图#3001 256x256 (RGBA8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 0 KB |
| 43 | dead_pass | 疑似 | 131 | pass rp60 128x128 sig=ae75a3d4bc624906 产出的 1 块内容全部没人读; draw 1 笔, pass 内无查询 (不确定点: fragment 里写 storage buffer 层看不见) |
| 43 | dead_store | 疑似 | 66 | 附件0 (RGBA8 图#2995 128x128) STORE 后到抓帧结束(≥1 整帧)没人读 |
| 51 | transient_ok | 是 | 0 | 图#3028 1070x484 (R32F) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 0 KB |
| 58 | transient_ok | 是 | 0 | 图#3030 2140x968 (R8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 0 KB |
| 73 | transient_ok | 是 | 0 | 图#124 2141x969 (D24S8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 0 KB |

