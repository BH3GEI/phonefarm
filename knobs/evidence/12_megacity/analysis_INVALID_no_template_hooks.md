## 316e5fd8881d/out/fa/fa_1.json

swapchain 2068x935，抓到 3 帧，时间戳 开（可用 128 个，周期 52.083 ns），诊断 {"cb_unseen":0,"dyn_passes":0,"ev_overflow":1,"q_copies":0,"q_outside":0,"sec_merged":0,"submit2":0,"ts_skipped":0}

### 每帧统计

```json
[
  {
    "draws": 574,
    "empty_passes": 0,
    "events": 11,
    "frame": 0,
    "kinds": {
      "copy_buf": 2,
      "dispatch": 1,
      "pass": 8
    },
    "load_attachments": 6,
    "model_load_mb": 108.76,
    "model_store_mb": 173.74,
    "pass_ms_sum": 16.898,
    "passes": 8,
    "passes_timed": 8,
    "query_passes": 0
  },
  {
    "draws": 574,
    "empty_passes": 0,
    "events": 11,
    "frame": 1,
    "kinds": {
      "copy_buf": 2,
      "dispatch": 1,
      "pass": 8
    },
    "load_attachments": 6,
    "model_load_mb": 108.76,
    "model_store_mb": 173.74,
    "pass_ms_sum": 16.595,
    "passes": 8,
    "passes_timed": 8,
    "query_passes": 0
  },
  {
    "draws": 574,
    "empty_passes": 0,
    "events": 11,
    "frame": 2,
    "kinds": {
      "copy_buf": 2,
      "dispatch": 1,
      "pass": 8
    },
    "load_attachments": 6,
    "model_load_mb": 108.76,
    "model_store_mb": 173.74,
    "pass_ms_sum": 0.0,
    "passes": 8,
    "passes_timed": 0,
    "query_passes": 0
  }
]
```

### 疑似多余汇总（每帧平均）

| 规则 | 条数/帧 | 模型字节 MB/帧 | 实测 ms/帧 |
|---|---|---|---|
| dead_pass(疑似) | 4.0 | 119.11 | 9.817 |
| dead_store | 7.0 | 64.97 | 0.000 |
| transient_ok | 2.5 | 32.49 | 0.000 |

### 第 0 帧步骤清单

| # | 步骤 | 尺寸 | 附件 (角色:格式#图 load/store) | draw | 采样引用 | 模型读/写 KB | GPU ms |
|---|---|---|---|---|---|---|---|
| 0 | copy_buf | 0x0 | src - → dst - | - | - | - | - |
| 1 | copy_buf | 0x0 | src - → dst - | - | - | - | - |
| 2 | dispatch | [188, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 3 | pass rp13<br>`2b01c0d5a1d5e34e` | 4096x4096 | D:D16#173 CLEAR/STORE | 299 | 0 | 0 / 33554 | 6.490 |
| 4 | pass rp17<br>`e51e2a0b50cc0287` | 2068x935 | D:D24S8#373 CLEAR/STORE s:CLEAR/STORE<br>C:fmt38#372 CLEAR/STORE | 132 | 0 | 0 / 15469 | 1.704 |
| 5 | pass rp19<br>`c3a64d53ba8837bb` | 2068x935 | D:D24S8#371x4 CLEAR/STORE s:CLEAR/STORE<br>C:R11G11B10F#367x4 CLEAR/STORE | 133 | 0 | 0 / 61875 | 5.975 |
| 6 | pass rp21<br>`b86d017ba7deaeeb` | 1034x467 | C:R8#374 DONT_CARE/STORE | 1 | 0 | 0 / 483 | 0.711 |
| 7 | pass rp38<br>`67c02862fc88532b` | 1034x467 | C:R8#375 LOAD/STORE | 1 | 0 | 483 / 483 | 0.072 |
| 8 | pass rp32<br>`fb2e9bc6efb185c5` | 2068x935 | C:R11G11B10F#367x4 LOAD/STORE<br>R:R11G11B10F#366 DONT_CARE/STORE | 1 | 0 | 30937 / 38672 | 0.871 |
| 9 | pass rp41<br>`d88abfb3b0566231` | 2068x935 | D:D24S8#371x4 LOAD/DONT_CARE s:LOAD/DONT_CARE<br>C:R11G11B10F#367x4 LOAD/DONT_CARE<br>R:R11G11B10F#366 DONT_CARE/STORE | 6 | 0 | 61875 / 7734 | 0.935 |
| 10 | pass rp40<br>`ae9f514c59165b6a` | 2068x935 | D:D24S8#201 LOAD/STORE s:LOAD/STORE<br>C:RGBA8_SRGB#4 LOAD/STORE | 1 | 0 | 15469 / 15469 | 0.140 |

### 疑似多余明细（第 0 帧）

| # | 规则 | 确定 | 模型 KB | 说明 |
|---|---|---|---|---|
| 3 | dead_pass | 疑似 | 33554 | pass rp13 4096x4096 sig=2b01c0d5a1d5e34e 产出的 1 块内容全部没人读; draw 299 笔, pass 内无查询 (不确定点: fragment 里写 storage buffer 层看不见) |
| 3 | transient_ok | 是 | 33554 | 图#173 4096x4096 (D16) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 33554 KB |
| 3 | dead_store | 是 | 33554 | 附件0 (D16 图#173 4096x4096) STORE 后下一次是覆盖/丢弃, 没人读 |
| 4 | dead_pass | 疑似 | 15469 | pass rp17 2068x935 sig=e51e2a0b50cc0287 产出的 3 块内容全部没人读; draw 132 笔, pass 内无查询 (不确定点: fragment 里写 storage buffer 层看不见) |
| 4 | transient_ok | 是 | 7734 | 图#372 2068x935 (fmt38) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 7734 KB |
| 4 | transient_ok | 是 | 7734 | 图#373 2068x935 (D24S8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 7734 KB |
| 4 | dead_store | 是 | 5801 | 附件0 (D24S8 图#373 2068x935) STORE 后下一次是覆盖/丢弃, 没人读 |
| 4 | dead_store | 是 | 1934 | 附件0 (D24S8 图#373(模板) 2068x935) STORE 后下一次是覆盖/丢弃, 没人读 |
| 4 | dead_store | 是 | 7734 | 附件1 (fmt38 图#372 2068x935) STORE 后下一次是覆盖/丢弃, 没人读 |
| 6 | dead_pass | 疑似 | 483 | pass rp21 1034x467 sig=b86d017ba7deaeeb 产出的 1 块内容全部没人读; draw 1 笔, pass 内无查询 (不确定点: fragment 里写 storage buffer 层看不见) |
| 6 | transient_ok | 是 | 483 | 图#374 1034x467 (R8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 483 KB |
| 6 | dead_store | 是 | 483 | 附件0 (R8 图#374 1034x467) STORE 后下一次是覆盖/丢弃, 没人读 |
| 8 | transient_ok | 是 | 15469 | 图#366 2068x935 (R11G11B10F) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 15469 KB |
| 8 | dead_store | 是 | 7734 | 附件1 (R11G11B10F 图#366 2068x935) STORE 后下一次是覆盖/丢弃, 没人读 |
| 9 | dead_pass | 疑似 | 69609 | pass rp41 2068x935 sig=d88abfb3b0566231 产出的 1 块内容全部没人读; draw 6 笔, pass 内无查询 (不确定点: fragment 里写 storage buffer 层看不见) |
| 9 | dead_store | 是 | 7734 | 附件2 (R11G11B10F 图#366 2068x935) STORE 后下一次是覆盖/丢弃, 没人读 |

## 316e5fd8881d/out/fa/fa_2.json

swapchain 2068x935，抓到 3 帧，时间戳 开（可用 128 个，周期 52.083 ns），诊断 {"cb_unseen":0,"dyn_passes":0,"ev_overflow":1,"q_copies":0,"q_outside":0,"sec_merged":0,"submit2":0,"ts_skipped":0}

### 每帧统计

```json
[
  {
    "draws": 565,
    "empty_passes": 0,
    "events": 11,
    "frame": 0,
    "kinds": {
      "copy_buf": 2,
      "dispatch": 1,
      "pass": 8
    },
    "load_attachments": 6,
    "model_load_mb": 108.76,
    "model_store_mb": 173.74,
    "pass_ms_sum": 18.382,
    "passes": 8,
    "passes_timed": 8,
    "query_passes": 0
  },
  {
    "draws": 565,
    "empty_passes": 0,
    "events": 11,
    "frame": 1,
    "kinds": {
      "copy_buf": 2,
      "dispatch": 1,
      "pass": 8
    },
    "load_attachments": 6,
    "model_load_mb": 108.76,
    "model_store_mb": 173.74,
    "pass_ms_sum": 16.79,
    "passes": 8,
    "passes_timed": 8,
    "query_passes": 0
  },
  {
    "draws": 565,
    "empty_passes": 0,
    "events": 11,
    "frame": 2,
    "kinds": {
      "copy_buf": 2,
      "dispatch": 1,
      "pass": 8
    },
    "load_attachments": 6,
    "model_load_mb": 108.76,
    "model_store_mb": 173.74,
    "pass_ms_sum": 0.0,
    "passes": 8,
    "passes_timed": 0,
    "query_passes": 0
  }
]
```

### 疑似多余汇总（每帧平均）

| 规则 | 条数/帧 | 模型字节 MB/帧 | 实测 ms/帧 |
|---|---|---|---|
| dead_pass(疑似) | 4.0 | 119.11 | 10.129 |
| dead_store | 7.0 | 64.97 | 0.000 |
| transient_ok | 2.5 | 32.49 | 0.000 |

### 第 0 帧步骤清单

| # | 步骤 | 尺寸 | 附件 (角色:格式#图 load/store) | draw | 采样引用 | 模型读/写 KB | GPU ms |
|---|---|---|---|---|---|---|---|
| 0 | copy_buf | 0x0 | src - → dst - | - | - | - | - |
| 1 | copy_buf | 0x0 | src - → dst - | - | - | - | - |
| 2 | dispatch | [188, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 3 | pass rp13<br>`2b01c0d5a1d5e34e` | 4096x4096 | D:D16#173 CLEAR/STORE | 296 | 0 | 0 / 33554 | 7.166 |
| 4 | pass rp17<br>`e51e2a0b50cc0287` | 2068x935 | D:D24S8#373 CLEAR/STORE s:CLEAR/STORE<br>C:fmt38#372 CLEAR/STORE | 129 | 0 | 0 / 15469 | 1.960 |
| 5 | pass rp19<br>`c3a64d53ba8837bb` | 2068x935 | D:D24S8#371x4 CLEAR/STORE s:CLEAR/STORE<br>C:R11G11B10F#367x4 CLEAR/STORE | 130 | 0 | 0 / 61875 | 6.608 |
| 6 | pass rp21<br>`b86d017ba7deaeeb` | 1034x467 | C:R8#374 DONT_CARE/STORE | 1 | 0 | 0 / 483 | 0.551 |
| 7 | pass rp38<br>`67c02862fc88532b` | 1034x467 | C:R8#375 LOAD/STORE | 1 | 0 | 483 / 483 | 0.071 |
| 8 | pass rp32<br>`fb2e9bc6efb185c5` | 2068x935 | C:R11G11B10F#367x4 LOAD/STORE<br>R:R11G11B10F#366 DONT_CARE/STORE | 1 | 0 | 30937 / 38672 | 0.857 |
| 9 | pass rp41<br>`d88abfb3b0566231` | 2068x935 | D:D24S8#371x4 LOAD/DONT_CARE s:LOAD/DONT_CARE<br>C:R11G11B10F#367x4 LOAD/DONT_CARE<br>R:R11G11B10F#366 DONT_CARE/STORE | 6 | 0 | 61875 / 7734 | 1.031 |
| 10 | pass rp40<br>`ae9f514c59165b6a` | 2068x935 | D:D24S8#201 LOAD/STORE s:LOAD/STORE<br>C:RGBA8_SRGB#6 LOAD/STORE | 1 | 0 | 15469 / 15469 | 0.139 |

### 疑似多余明细（第 0 帧）

| # | 规则 | 确定 | 模型 KB | 说明 |
|---|---|---|---|---|
| 3 | dead_pass | 疑似 | 33554 | pass rp13 4096x4096 sig=2b01c0d5a1d5e34e 产出的 1 块内容全部没人读; draw 296 笔, pass 内无查询 (不确定点: fragment 里写 storage buffer 层看不见) |
| 3 | transient_ok | 是 | 33554 | 图#173 4096x4096 (D16) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 33554 KB |
| 3 | dead_store | 是 | 33554 | 附件0 (D16 图#173 4096x4096) STORE 后下一次是覆盖/丢弃, 没人读 |
| 4 | dead_pass | 疑似 | 15469 | pass rp17 2068x935 sig=e51e2a0b50cc0287 产出的 3 块内容全部没人读; draw 129 笔, pass 内无查询 (不确定点: fragment 里写 storage buffer 层看不见) |
| 4 | transient_ok | 是 | 7734 | 图#372 2068x935 (fmt38) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 7734 KB |
| 4 | transient_ok | 是 | 7734 | 图#373 2068x935 (D24S8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 7734 KB |
| 4 | dead_store | 是 | 5801 | 附件0 (D24S8 图#373 2068x935) STORE 后下一次是覆盖/丢弃, 没人读 |
| 4 | dead_store | 是 | 1934 | 附件0 (D24S8 图#373(模板) 2068x935) STORE 后下一次是覆盖/丢弃, 没人读 |
| 4 | dead_store | 是 | 7734 | 附件1 (fmt38 图#372 2068x935) STORE 后下一次是覆盖/丢弃, 没人读 |
| 6 | dead_pass | 疑似 | 483 | pass rp21 1034x467 sig=b86d017ba7deaeeb 产出的 1 块内容全部没人读; draw 1 笔, pass 内无查询 (不确定点: fragment 里写 storage buffer 层看不见) |
| 6 | transient_ok | 是 | 483 | 图#374 1034x467 (R8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 483 KB |
| 6 | dead_store | 是 | 483 | 附件0 (R8 图#374 1034x467) STORE 后下一次是覆盖/丢弃, 没人读 |
| 8 | transient_ok | 是 | 15469 | 图#366 2068x935 (R11G11B10F) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 15469 KB |
| 8 | dead_store | 是 | 7734 | 附件1 (R11G11B10F 图#366 2068x935) STORE 后下一次是覆盖/丢弃, 没人读 |
| 9 | dead_pass | 疑似 | 69609 | pass rp41 2068x935 sig=d88abfb3b0566231 产出的 1 块内容全部没人读; draw 6 笔, pass 内无查询 (不确定点: fragment 里写 storage buffer 层看不见) |
| 9 | dead_store | 是 | 7734 | 附件2 (R11G11B10F 图#366 2068x935) STORE 后下一次是覆盖/丢弃, 没人读 |

## 316e5fd8881d/out/fa/fa_3.json

swapchain 2068x935，抓到 3 帧，时间戳 开（可用 128 个，周期 52.083 ns），诊断 {"cb_unseen":0,"dyn_passes":0,"ev_overflow":1,"q_copies":0,"q_outside":0,"sec_merged":0,"submit2":0,"ts_skipped":0}

### 每帧统计

```json
[
  {
    "draws": 565,
    "empty_passes": 0,
    "events": 11,
    "frame": 0,
    "kinds": {
      "copy_buf": 2,
      "dispatch": 1,
      "pass": 8
    },
    "load_attachments": 6,
    "model_load_mb": 108.76,
    "model_store_mb": 173.74,
    "pass_ms_sum": 18.598,
    "passes": 8,
    "passes_timed": 8,
    "query_passes": 0
  },
  {
    "draws": 565,
    "empty_passes": 0,
    "events": 11,
    "frame": 1,
    "kinds": {
      "copy_buf": 2,
      "dispatch": 1,
      "pass": 8
    },
    "load_attachments": 6,
    "model_load_mb": 108.76,
    "model_store_mb": 173.74,
    "pass_ms_sum": 19.256,
    "passes": 8,
    "passes_timed": 8,
    "query_passes": 0
  },
  {
    "draws": 565,
    "empty_passes": 0,
    "events": 11,
    "frame": 2,
    "kinds": {
      "copy_buf": 2,
      "dispatch": 1,
      "pass": 8
    },
    "load_attachments": 6,
    "model_load_mb": 108.76,
    "model_store_mb": 173.74,
    "pass_ms_sum": 0.0,
    "passes": 8,
    "passes_timed": 0,
    "query_passes": 0
  }
]
```

### 疑似多余汇总（每帧平均）

| 规则 | 条数/帧 | 模型字节 MB/帧 | 实测 ms/帧 |
|---|---|---|---|
| dead_pass(疑似) | 4.0 | 119.11 | 10.835 |
| dead_store | 7.0 | 64.97 | 0.000 |
| transient_ok | 2.5 | 32.49 | 0.000 |

### 第 0 帧步骤清单

| # | 步骤 | 尺寸 | 附件 (角色:格式#图 load/store) | draw | 采样引用 | 模型读/写 KB | GPU ms |
|---|---|---|---|---|---|---|---|
| 0 | copy_buf | 0x0 | src - → dst - | - | - | - | - |
| 1 | copy_buf | 0x0 | src - → dst - | - | - | - | - |
| 2 | dispatch | [188, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 3 | pass rp13<br>`2b01c0d5a1d5e34e` | 4096x4096 | D:D16#173 CLEAR/STORE | 296 | 0 | 0 / 33554 | 6.976 |
| 4 | pass rp17<br>`e51e2a0b50cc0287` | 2068x935 | D:D24S8#373 CLEAR/STORE s:CLEAR/STORE<br>C:fmt38#372 CLEAR/STORE | 129 | 0 | 0 / 15469 | 1.962 |
| 5 | pass rp19<br>`c3a64d53ba8837bb` | 2068x935 | D:D24S8#371x4 CLEAR/STORE s:CLEAR/STORE<br>C:R11G11B10F#367x4 CLEAR/STORE | 130 | 0 | 0 / 61875 | 6.682 |
| 6 | pass rp21<br>`b86d017ba7deaeeb` | 1034x467 | C:R8#374 DONT_CARE/STORE | 1 | 0 | 0 / 483 | 0.568 |
| 7 | pass rp38<br>`67c02862fc88532b` | 1034x467 | C:R8#375 LOAD/STORE | 1 | 0 | 483 / 483 | 0.075 |
| 8 | pass rp32<br>`fb2e9bc6efb185c5` | 2068x935 | C:R11G11B10F#367x4 LOAD/STORE<br>R:R11G11B10F#366 DONT_CARE/STORE | 1 | 0 | 30937 / 38672 | 1.028 |
| 9 | pass rp41<br>`d88abfb3b0566231` | 2068x935 | D:D24S8#371x4 LOAD/DONT_CARE s:LOAD/DONT_CARE<br>C:R11G11B10F#367x4 LOAD/DONT_CARE<br>R:R11G11B10F#366 DONT_CARE/STORE | 6 | 0 | 61875 / 7734 | 1.101 |
| 10 | pass rp40<br>`ae9f514c59165b6a` | 2068x935 | D:D24S8#201 LOAD/STORE s:LOAD/STORE<br>C:RGBA8_SRGB#4 LOAD/STORE | 1 | 0 | 15469 / 15469 | 0.206 |

### 疑似多余明细（第 0 帧）

| # | 规则 | 确定 | 模型 KB | 说明 |
|---|---|---|---|---|
| 3 | dead_pass | 疑似 | 33554 | pass rp13 4096x4096 sig=2b01c0d5a1d5e34e 产出的 1 块内容全部没人读; draw 296 笔, pass 内无查询 (不确定点: fragment 里写 storage buffer 层看不见) |
| 3 | transient_ok | 是 | 33554 | 图#173 4096x4096 (D16) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 33554 KB |
| 3 | dead_store | 是 | 33554 | 附件0 (D16 图#173 4096x4096) STORE 后下一次是覆盖/丢弃, 没人读 |
| 4 | dead_pass | 疑似 | 15469 | pass rp17 2068x935 sig=e51e2a0b50cc0287 产出的 3 块内容全部没人读; draw 129 笔, pass 内无查询 (不确定点: fragment 里写 storage buffer 层看不见) |
| 4 | transient_ok | 是 | 7734 | 图#372 2068x935 (fmt38) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 7734 KB |
| 4 | transient_ok | 是 | 7734 | 图#373 2068x935 (D24S8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 7734 KB |
| 4 | dead_store | 是 | 5801 | 附件0 (D24S8 图#373 2068x935) STORE 后下一次是覆盖/丢弃, 没人读 |
| 4 | dead_store | 是 | 1934 | 附件0 (D24S8 图#373(模板) 2068x935) STORE 后下一次是覆盖/丢弃, 没人读 |
| 4 | dead_store | 是 | 7734 | 附件1 (fmt38 图#372 2068x935) STORE 后下一次是覆盖/丢弃, 没人读 |
| 6 | dead_pass | 疑似 | 483 | pass rp21 1034x467 sig=b86d017ba7deaeeb 产出的 1 块内容全部没人读; draw 1 笔, pass 内无查询 (不确定点: fragment 里写 storage buffer 层看不见) |
| 6 | transient_ok | 是 | 483 | 图#374 1034x467 (R8) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 483 KB |
| 6 | dead_store | 是 | 483 | 附件0 (R8 图#374 1034x467) STORE 后下一次是覆盖/丢弃, 没人读 |
| 8 | transient_ok | 是 | 15469 | 图#366 2068x935 (R11G11B10F) 只在 pass 内用、从不被别的步骤读: 可建成 LAZILY_ALLOCATED/transient; 当前每帧仍写回 15469 KB |
| 8 | dead_store | 是 | 7734 | 附件1 (R11G11B10F 图#366 2068x935) STORE 后下一次是覆盖/丢弃, 没人读 |
| 9 | dead_pass | 疑似 | 69609 | pass rp41 2068x935 sig=d88abfb3b0566231 产出的 1 块内容全部没人读; draw 6 笔, pass 内无查询 (不确定点: fragment 里写 storage buffer 层看不见) |
| 9 | dead_store | 是 | 7734 | 附件2 (R11G11B10F 图#366 2068x935) STORE 后下一次是覆盖/丢弃, 没人读 |

