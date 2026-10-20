// npuprobe: 层被游戏进程加载时, 验证游戏进程 (普通应用, 无 root) 能不能用 NPU 跑超分。
// 不开线程: 原神会杀掉层自己创建的线程 (2026-10-20 实测, 只开线程写日志也会 SIGSEGV)。
// 所以全部工作在游戏线程的 vkQueuePresentKHR 里同步做: 第 INIT_FRAME 帧初始化 (卡一帧),
// 之后 RUN_FRAMES 帧每帧推理一次并计时。
// debug.knobs.npuprobe=1 启用完整探测; =2 只开设备写日志, 不加载任何库; =3 只开线程写日志 (排查游戏为什么退出)。
// 全程只读, 不碰游戏的 Vulkan 调用。
//
// 需要和层放在同一个 nativeLibraryDir 的文件 (游戏沙箱只读得到自己的目录):
//   libtensorflowlite_jni.so      TFLite 运行时 (tensorflow-lite 2.16.1 AAR)
//   libQnnTFLiteDelegate.so libQnnHtp.so libQnnHtpV81Stub.so libQnnSystem.so libQnnHtpPrepare.so
//   libQnnHtpV81Skel.so libQnnHtpV81.so    DSP 侧, 由 ADSP_LIBRARY_PATH 找到
//   libnpusr_model.so              超分模型 (.tflite 改名, 只有 lib*.so 会被安装器保留权限)
//
// 全部经 dlopen/dlsym 调用: 文件不在时只打日志, 层照常工作。
// 结果写 logcat (tag refknobs-npu) 和应用外部目录的 npuprobe.txt
// (有的 ROM 把 log.tag 设成 S, logcat 全部静音)。
#include <android/log.h>
#include <stdarg.h>
#include <stdio.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/system_properties.h>
#include <time.h>
#include <unistd.h>
#include <string>

namespace {

FILE* g_out = nullptr;
int g_mode = 0;

void nlog(const char* fmt, ...) __attribute__((format(printf, 1, 2)));
void nlog(const char* fmt, ...) {
    char buf[512];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, sizeof buf, fmt, ap);
    va_end(ap);
    __android_log_print(ANDROID_LOG_INFO, "refknobs-npu", "%s", buf);
    if (g_out) { fprintf(g_out, "%s\n", buf); fflush(g_out); }
}
#define NLOG(...) nlog(__VA_ARGS__)

void open_out() {
    char pkg[256] = {0};
    FILE* f = fopen("/proc/self/cmdline", "r");
    if (f) { fread(pkg, 1, sizeof pkg - 1, f); fclose(f); }
    std::string path = std::string("/storage/emulated/0/Android/data/") + pkg + "/files/npuprobe.txt";
    g_out = fopen(path.c_str(), "a");
}

double now_ms() {
    timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1e3 + t.tv_nsec / 1e6;
}

std::string self_dir() {
    Dl_info info{};
    if (!dladdr((void*)&self_dir, &info) || !info.dli_fname) return "";
    std::string p = info.dli_fname;
    return p.substr(0, p.rfind('/'));
}

template <typename T>
bool sym(void* h, const char* name, T& out) {
    out = reinterpret_cast<T>(dlsym(h, name));
    if (!out) NLOG("缺符号 %s", name);
    return out != nullptr;
}

const uint64_t INIT_FRAME = 300, RUN_FRAMES = 60;
void* g_it = nullptr;
int (*g_invoke)(void*) = nullptr;
double g_sum = 0, g_max = 0;
int g_runs = 0;

void* probe(void*) {
    open_out();
    std::string dir = self_dir();
    NLOG("开始, 目录 %s, uid %d", dir.c_str(), getuid());

    if (g_mode == 3) { NLOG("模式 3, 不碰设备"); return nullptr; }
    int fd = open("/dev/fastrpc-cdsp", O_RDONLY | O_NONBLOCK);
    NLOG("open /dev/fastrpc-cdsp: %s", fd >= 0 ? "成功" : strerror(errno));
    if (fd >= 0) close(fd);
    if (g_mode == 2) { NLOG("模式 2, 到此为止"); return nullptr; }

    std::string adsp = dir + ";/vendor/lib/rfsa/adsp;/vendor/dsp/cdsp";
    setenv("ADSP_LIBRARY_PATH", adsp.c_str(), 1);

    void* tfl = dlopen((dir + "/libtensorflowlite_jni.so").c_str(), RTLD_NOW | RTLD_LOCAL);
    void* qd = dlopen((dir + "/libQnnTFLiteDelegate.so").c_str(), RTLD_NOW | RTLD_LOCAL);
    if (!tfl || !qd) { NLOG("dlopen 失败: %s", dlerror()); return nullptr; }

    using CreateDel = void* (*)(char**, char**, size_t, void (*)(const char*));
    using ModelCreate = void* (*)(const char*);
    using OptCreate = void* (*)();
    using OptAddDel = void (*)(void*, void*);
    using InterpCreate = void* (*)(void*, void*);
    using Status = int (*)(void*);
    using GetIn = void* (*)(void*, int);
    using Bytes = size_t (*)(const void*);
    using Data = void* (*)(const void*);
    CreateDel create_del; ModelCreate model_create; OptCreate opt_create; OptAddDel opt_add;
    InterpCreate interp_create; Status alloc, invoke; GetIn get_in; Bytes bytes; Data data;
    if (!sym(qd, "tflite_plugin_create_delegate", create_del) ||
        !sym(tfl, "TfLiteModelCreateFromFile", model_create) ||
        !sym(tfl, "TfLiteInterpreterOptionsCreate", opt_create) ||
        !sym(tfl, "TfLiteInterpreterOptionsAddDelegate", opt_add) ||
        !sym(tfl, "TfLiteInterpreterCreate", interp_create) ||
        !sym(tfl, "TfLiteInterpreterAllocateTensors", alloc) ||
        !sym(tfl, "TfLiteInterpreterInvoke", invoke) ||
        !sym(tfl, "TfLiteInterpreterGetInputTensor", get_in) ||
        !sym(tfl, "TfLiteTensorByteSize", bytes) ||
        !sym(tfl, "TfLiteTensorData", data))
        return nullptr;

    std::string lib = dir + "/libQnnHtp.so";
    const char* keys[] = {"backend_type", "library_path", "skel_library_dir", "htp_performance_mode"};
    const char* vals[] = {"htp", lib.c_str(), dir.c_str(), "8"};   // 8 = balanced
    double t0 = now_ms();
    void* del = create_del((char**)keys, (char**)vals, 4, nullptr);
    void* model = model_create((dir + "/libnpusr_model.so").c_str());
    if (!del || !model) { NLOG("代理或模型创建失败 del=%p model=%p", del, model); return nullptr; }
    void* opt = opt_create();
    opt_add(opt, del);
    void* it = interp_create(model, opt);
    if (!it || alloc(it) != 0) { NLOG("解释器创建失败"); return nullptr; }
    NLOG("初始化 %.0f ms", now_ms() - t0);

    void* in = get_in(it, 0);
    memset(data(in), 128, bytes(in));
    g_it = it;
    g_invoke = invoke;
    return nullptr;   // 不释放: 退出时析构会 abort (见 tools 里的 handoff 测试)
}

__attribute__((constructor)) void npuprobe_init() {
    char v[PROP_VALUE_MAX] = {0};
    if (__system_property_get("debug.knobs.npuprobe", v) <= 0) return;
    g_mode = atoi(v);
    if (g_mode < 1 || g_mode > 3) g_mode = 0;
}

}  // namespace

// vk_layer_refknobs.cpp 的 rk_QueuePresentKHR 每帧调用
void npuprobe_on_present(uint64_t frame) {
    if (!g_mode || frame < INIT_FRAME) return;
    if (frame == INIT_FRAME) {
        double t0 = now_ms();
        probe(nullptr);
        NLOG("第 %llu 帧初始化, 卡了 %.0f ms", (unsigned long long)frame, now_ms() - t0);
        return;
    }
    if (!g_it || g_runs >= (int)RUN_FRAMES) return;
    double a = now_ms();
    if (g_invoke(g_it) != 0) { NLOG("推理失败"); g_it = nullptr; return; }
    double d = now_ms() - a;
    g_sum += d; if (d > g_max) g_max = d;
    if (++g_runs == (int)RUN_FRAMES)
        NLOG("游戏线程内 NPU 推理成功: %d 帧, 平均 %.2f ms, 最长 %.2f ms", g_runs, g_sum / g_runs, g_max);
}
