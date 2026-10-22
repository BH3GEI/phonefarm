#!/system/bin/sh
# vendor_recon.sh — 查清红魔上「谁、在什么时候、改了哪个频率节点、按哪份配置改」(设备端, su 下跑)。
#
#   vendor_recon.sh static <输出目录>
#       只读: 进程、init 服务定义、属性、settings、HAL/服务清单、候选配置文件清单 (含 sha256、
#       SELinux 标签), 并把候选配置文件和含原神包名的文件**复制**到 <输出目录>/files/ 以便拉回。
#       不改任何系统状态。<输出目录> 必须是新目录。
#   vendor_recon.sh watch <秒> <输出文件>
#       只读: 每 0.5 秒记一次频率边界节点 + 前台应用 + 候选服务 pid。期间由人/主机端启动原神,
#       看「游戏切前台后多久、哪个节点、变成什么值」。
#   vendor_recon.sh attrib <秒> <输出文件>
#       写节点的进程归属: 在 cpufreq / kgsl 的 sysfs 写入函数上挂 kprobe (ftrace), 记下每次写入的
#       进程名、pid、写入的值。**会临时改 ftrace 状态**: 新建 ftrace 实例 o2 并追加 o2 组的 kprobe;
#       结束时关掉、撤掉本脚本加的那几条 kprobe 和实例 o2, 其余 ftrace 状态前后比对。
#       不要和测量 (ftrace_capture.sh) 同时跑。
set -u

KGSL=/sys/class/kgsl/kgsl-3d0
CPU=/sys/devices/system/cpu/cpufreq
PKG="${O2_PKG:-com.miHoYo.Yuanshen}"
# 候选服务/应用的名字片段 (perf2-hal / zperfcubeservice 来自 phonefarm 代码注释, 未见原始 ps 记录)
PAT='perf|zperf|cube|game|nubia|zte|thermal|powerhal|power-hal|hint|boost|gpuservice|mpctl|iop'

fg_app() { dumpsys window 2>/dev/null | grep -E 'mCurrentFocus' | head -1 | sed 's/.* \([^ ]*\)\/.*/\1/;s/}//'; }

cand_pids() { ps -A -o PID,NAME 2>/dev/null | grep -iE "$PAT" | grep -v grep | awk '{printf "%s:%s ", $1, $2}'; }

static_mode() {
  out="$1"
  if [ -e "$out" ]; then echo "O2_REFUSE $out 已存在"; exit 2; fi
  mkdir -p "$out/files" || exit 2

  { echo "## ps"; ps -A -o PID,PPID,USER,LABEL,NAME,ARGS 2>/dev/null | grep -iE "$PAT" | grep -v grep; } > "$out/ps.txt"

  { echo "## getprop (perf/game/厂商相关)"
    getprop | grep -iE 'perf|zperf|cube|game|nubia|zte|redmagic|thermal|boost|hint|init\.svc\.' ; } > "$out/props.txt"

  { for ns in global system secure; do
      echo "## settings $ns"
      settings list "$ns" 2>/dev/null | grep -iE 'game|perf|nubia|zte|red|boost|cube|power|thermal|fps|frame'
    done; } > "$out/settings.txt"

  { echo "## init 服务定义 (含候选名字的 .rc)"
    for d in /system/etc/init /system_ext/etc/init /product/etc/init /vendor/etc/init /odm/etc/init /vendor/odm/etc/init; do
      [ -d "$d" ] || continue
      for f in $(grep -liE 'zperf|perf2|perf-hal|perfd|cube|gamemode|game_?space|nubia|zte' "$d"/*.rc 2>/dev/null); do
        echo "### $f"; ls -lZ "$f"; cat "$f"
      done
    done; } > "$out/init_rc.txt"

  { echo "## HAL / binder 服务"
    lshal --types=b,c,l 2>/dev/null | grep -iE 'perf|power|thermal|game|zte|nubia'
    service list 2>/dev/null | grep -iE 'perf|power|thermal|game|zte|nubia|cube'
    echo "## 可执行文件"
    for d in /vendor/bin /vendor/bin/hw /system_ext/bin /system/bin /odm/bin /system/bin/hw /system_ext/bin/hw; do
      ls -lZ "$d" 2>/dev/null | grep -iE 'perf|cube|game|thermal|zte|nubia' | sed "s|^|$d: |"
    done; } > "$out/services.txt"

  # 候选配置文件: 名字像性能/游戏配置的, 和内容里出现原神包名的
  : > "$out/config_files.txt"
  for d in /vendor/etc /odm/etc /vendor/odm/etc /system/etc /system_ext/etc /product/etc /my_product/etc \
           /data/vendor /data/system /data/misc; do
    [ -d "$d" ] || continue
    find "$d" -maxdepth 5 -type f \( -iname '*perf*' -o -iname '*boost*' -o -iname '*game*' -o -iname '*cube*' \
         -o -iname '*zperf*' -o -iname '*thermal*' -o -iname '*powerhint*' -o -iname '*whitelist*' \) 2>/dev/null
    grep -rlI -e "$PKG" -e 'Yuanshen' "$d" 2>/dev/null
  done | sort -u | while IFS= read -r f; do
    sz=$(stat -c %s "$f" 2>/dev/null); [ -n "$sz" ] || continue
    sha=$(sha256sum "$f" 2>/dev/null | cut -d' ' -f1)
    ctx=$(ls -Z "$f" 2>/dev/null | awk '{print $1}')
    echo "$f size=$sz sha256=$sha ctx=$ctx" >> "$out/config_files.txt"
    # 只复制, 原件不动; 太大的 (>4MB, 多半是数据库/模型) 只记不复制
    if [ "$sz" -le 4194304 ] 2>/dev/null; then
      dst="$out/files$(echo "$f")"; mkdir -p "$(dirname "$dst")"; cp -p "$f" "$dst" 2>/dev/null
    fi
  done

  # 厂商游戏应用自己的数据目录 (云端下发的游戏配置常落在这里): 只列文件名, 复制 shared_prefs/files 下的小文件
  for app in cn.nubia.gameassist cn.nubia.gamelab cn.nubia.neogamecenter; do
    d=/data/data/$app; [ -d "$d" ] || continue
    echo "## $app" >> "$out/app_data.txt"
    find "$d" -maxdepth 3 -type f 2>/dev/null | head -400 >> "$out/app_data.txt"
    for f in $(find "$d/shared_prefs" "$d/files" -maxdepth 2 -type f -size -1024k 2>/dev/null); do
      dst="$out/files$f"; mkdir -p "$(dirname "$dst")"; cp -p "$f" "$dst" 2>/dev/null
    done
  done

  # 每个候选进程: 可执行文件、打开着的频率节点、映射进来的配置文件
  { for pn in $(ps -A -o PID,NAME | grep -iE "$PAT" | grep -v grep | awk '{print $1}'); do
      echo "### pid=$pn $(cat /proc/$pn/cmdline 2>/dev/null | tr '\0' ' ')"
      echo "exe=$(readlink /proc/$pn/exe 2>/dev/null)"
      ls -l /proc/$pn/fd 2>/dev/null | grep -E 'cpufreq|kgsl|devfreq|bus_dcvs|msm_performance|\.xml|\.conf|\.json|\.db|\.txt'
      grep -E '\.xml|\.conf|\.json|\.db' /proc/$pn/maps 2>/dev/null | awk '{print $6}' | sort -u
    done; } > "$out/proc_files.txt"

  { echo "## 频率边界 (此刻)"; for p in $CPU/policy*; do
      echo "$p max=$(cat $p/scaling_max_freq) min=$(cat $p/scaling_min_freq) gov=$(cat $p/scaling_governor)"; done
    for f in max_pwrlevel min_pwrlevel thermal_pwrlevel max_gpuclk default_pwrlevel; do echo "kgsl.$f=$(cat $KGSL/$f 2>/dev/null)"; done
    ls /sys/module/msm_performance/parameters 2>/dev/null | sed 's/^/msm_performance: /'
    for f in cpu_max_freq cpu_min_freq; do echo "msm_performance.$f=$(cat /sys/module/msm_performance/parameters/$f 2>/dev/null)"; done
    echo "fg=$(fg_app)"; } > "$out/nodes_now.txt"

  logcat -d -b all 2>/dev/null | grep -iE 'perf2|zperf|cube|gameassist|gamemode|perf-hal|perfhal|PerfService|boost' | tail -3000 > "$out/logcat_vendor.txt"
  echo "O2_RECON_DONE $out"
}

watch_mode() {
  secs="$1"; out="$2"
  if [ -e "$out" ]; then echo "O2_REFUSE $out 已存在"; exit 2; fi
  n=0; end=$((secs * 2))
  echo "# t_ms fg policy0.max policy6.max policy0.min policy6.min kgsl.max kgsl.min kgsl.thermal gpuclk pids" > "$out"
  t0=$(date +%s%3N 2>/dev/null || date +%s)
  while [ "$n" -lt "$end" ]; do
    t=$(( $(date +%s%3N 2>/dev/null || date +%s) - t0 ))
    line="$t $(fg_app)"
    for p in policy0 policy6; do line="$line $(cat $CPU/$p/scaling_max_freq 2>/dev/null)"; done
    for p in policy0 policy6; do line="$line $(cat $CPU/$p/scaling_min_freq 2>/dev/null)"; done
    for f in max_pwrlevel min_pwrlevel thermal_pwrlevel gpuclk; do line="$line $(cat $KGSL/$f 2>/dev/null)"; done
    # pid 每 5 秒记一次就够
    [ $((n % 10)) -eq 0 ] && line="$line [$(cand_pids)]"
    echo "$line" >> "$out"
    n=$((n + 1)); sleep 0.5
  done
  echo "O2_WATCH_DONE $out"
}

attrib_mode() {
  secs="$1"; out="$2"
  if [ -e "$out" ]; then echo "O2_REFUSE $out 已存在"; exit 2; fi
  T=/sys/kernel/tracing; [ -d "$T/instances" ] || T=/sys/kernel/debug/tracing
  if [ ! -w "$T/kprobe_events" ]; then echo "O2_ATTRIB_UNAVAILABLE 没有可写的 kprobe_events" > "$out"; return; fi
  if [ -d "$T/instances/o2" ]; then echo "O2_REFUSE ftrace 实例 o2 已存在 (上次没收尾?), 先人工查看"; exit 2; fi
  before=$(cat "$T/kprobe_events" "$T/tracing_on" "$T/current_tracer" 2>/dev/null | md5sum)
  added=""
  # 每个写入函数挂一条; 符号名随内核版本不同, 只挂 kallsyms 里真有的
  for spec in "cpu_smax store_scaling_max_freq 2" "cpu_smin store_scaling_min_freq 2" \
              "kgsl_max max_pwrlevel_store 3" "kgsl_max2 kgsl_pwrctrl_max_pwrlevel_store 3" \
              "kgsl_min min_pwrlevel_store 3" "kgsl_gclk max_gpuclk_store 3" "kgsl_mhz max_clock_mhz_store 3" \
              "msmperf_max set_cpu_max_freq 1"; do
    set -- $spec; ev=$1; sym=$2; arg=$3
    awk -v s="$sym" '$3==s{f=1} END{exit !f}' /proc/kallsyms 2>/dev/null || continue
    if echo "p:o2/$ev $sym val=+0(\$arg$arg):string" >> "$T/kprobe_events" 2>/dev/null; then
      added="$added $ev"
    elif echo "p:o2/$ev $sym" >> "$T/kprobe_events" 2>/dev/null; then
      added="$added $ev"
    fi
  done
  if [ -z "$added" ]; then echo "O2_ATTRIB_UNAVAILABLE kallsyms 里找不到写入函数" > "$out"; return; fi
  mkdir "$T/instances/o2" || { for ev in $added; do echo "-:o2/$ev" >> "$T/kprobe_events"; done; exit 2; }
  for ev in $added; do echo 1 > "$T/instances/o2/events/o2/$ev/enable" 2>/dev/null; done
  echo 1 > "$T/instances/o2/tracing_on"
  echo "# kprobes:$added  (进程名-pid 写入值); 期间启动/退出原神" > "$out"
  ( cat "$T/instances/o2/trace_pipe" >> "$out" ) &
  rp=$!
  sleep "$secs"
  kill "$rp" 2>/dev/null
  echo 0 > "$T/instances/o2/tracing_on"
  for ev in $added; do echo 0 > "$T/instances/o2/events/o2/$ev/enable" 2>/dev/null; done
  # 收尾: 撤掉本脚本自己加的实例和 kprobe (恢复进入前的 ftrace 状态)
  rmdir "$T/instances/o2" 2>/dev/null
  for ev in $added; do echo "-:o2/$ev" >> "$T/kprobe_events" 2>/dev/null; done
  after=$(cat "$T/kprobe_events" "$T/tracing_on" "$T/current_tracer" 2>/dev/null | md5sum)
  if [ "$before" = "$after" ]; then echo "O2_ATTRIB_DONE ftrace 状态已复原" | tee -a "$out"
  else echo "O2_ATTRIB_FTRACE_DIFF ftrace 状态与进入前不同, 需人工核对 $T/kprobe_events" | tee -a "$out"; fi
}

case "${1:-}" in
  static) static_mode "${2:?缺输出目录}" ;;
  watch)  watch_mode "${2:?缺秒数}" "${3:?缺输出文件}" ;;
  attrib) attrib_mode "${2:?缺秒数}" "${3:?缺输出文件}" ;;
  *) echo "用法: $0 static <目录> | watch <秒> <文件> | attrib <秒> <文件>"; exit 2 ;;
esac
