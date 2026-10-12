## 9dc2c2ff6723/out/sc/fa_on.json

swapchain 2068x935，抓到 3 帧，时间戳 关（可用 0 个，周期 0.000 ns），诊断 {"cb_unseen":0,"dyn_passes":0,"ev_overflow":1,"push_writes":0,"q_copies":0,"q_outside":0,"sec_merged":0,"submit2":0,"tmpl_updates":51375,"ts_skipped":0}

### 每帧统计

```json
[
  {
    "descriptor_refs": 87,
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
    "model_store_mb": 166.0,
    "pass_ms_sum": 0.0,
    "passes": 8,
    "passes_timed": 0,
    "query_passes": 0
  },
  {
    "descriptor_refs": 87,
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
    "model_store_mb": 166.0,
    "pass_ms_sum": 0.0,
    "passes": 8,
    "passes_timed": 0,
    "query_passes": 0
  },
  {
    "descriptor_refs": 87,
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
    "model_store_mb": 166.0,
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

### 第 0 帧步骤清单

| # | 步骤 | 尺寸 | 附件 (角色:格式#图 load/store) | draw | 采样引用 | 模型读/写 KB | GPU ms |
|---|---|---|---|---|---|---|---|
| 0 | copy_buf | 0x0 | src - → dst - | - | - | - | - |
| 1 | copy_buf | 0x0 | src - → dst - | - | - | - | - |
| 2 | dispatch | [188, 1, 1] | rd 0 / wr 0 | - | - | - | - |
| 3 | pass rp13<br>`2b01c0d5a1d5e34e` | 4096x4096 | D:D16#173 CLEAR/STORE | 296 | 1 | 0 / 33554 | - |
| 4 | pass rp17<br>`e51e2a0b50cc0287` | 2068x935 | D:D24S8#373 CLEAR/STORE s:CLEAR/STORE<br>C:fmt38#372 CLEAR/STORE | 129 | 0 | 0 / 15469 | - |
| 5 | pass rp19<br>`c3a64d53ba8837bb` | 2068x935 | D:D24S8#371x4 CLEAR/STORE s:CLEAR/STORE<br>C:R11G11B10F#367x4 CLEAR/STORE | 130 | 73 | 0 / 61875 | - |
| 6 | pass rp21<br>`b86d017ba7deaeeb` | 1034x467 | C:R8#374 DONT_CARE/STORE | 1 | 2 | 0 / 483 | - |
| 7 | pass rp38<br>`67c02862fc88532b` | 1034x467 | C:R8#375 LOAD/STORE | 1 | 1 | 483 / 483 | - |
| 8 | pass rp32<br>`fb2e9bc6efb185c5` | 2068x935 | C:R11G11B10F#367x4 LOAD/STORE<br>R:R11G11B10F#366 DONT_CARE/DONT_CARE | 1 | 1 | 30937 / 30937 | - |
| 9 | pass rp41<br>`d88abfb3b0566231` | 2068x935 | D:D24S8#371x4 LOAD/DONT_CARE s:LOAD/DONT_CARE<br>C:R11G11B10F#367x4 LOAD/DONT_CARE<br>R:R11G11B10F#366 DONT_CARE/STORE | 6 | 8 | 61875 / 7734 | - |
| 10 | pass rp40<br>`ae9f514c59165b6a` | 2068x935 | D:D24S8#201 LOAD/STORE s:LOAD/STORE<br>C:RGBA8_SRGB#5 LOAD/STORE | 1 | 1 | 15469 / 15469 | - |

### 疑似多余明细（第 0 帧）

| # | 规则 | 确定 | 模型 KB | 说明 |
|---|---|---|---|---|

