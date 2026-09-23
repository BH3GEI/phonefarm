// vk_layer_refknobs.cpp — 灰档只读起步层: pass-through + render pass 计数
//
// 用途 (FEASIBILITY.md 问题 1/2):
//   - 挂上即证明"机制能给非 debuggable 应用注入 Vulkan layer"
//   - 纯只读、零改写 → 是探反作弊的低风险空层
//   - 顺带补 render pass 级归因: 数每帧 vkCmdBeginRenderPass 次数
//
// 证据走双通道: logcat 的 refknobs 标签 (立项时以为本机 logcat 哑掉, 实测只是对 shell uid
// 不可读, `su -c logcat` 正常), 以及经 /proc/self/cmdline 取包名后写进该应用自己的外部
// files 目录的 JSON (harness 用 root pull)。任一条成立即可判定层挂上了。
//
// 自包含: 只 include <vulkan/vulkan.h>, layer 协商所需的少量结构体按稳定 ABI 就地声明,
// 不依赖 vk_layer.h 是否在 NDK 里。
#include <vulkan/vulkan.h>
#include <android/log.h>
#include <atomic>
#include <cstdio>
#include <cstring>
#include <map>
#include <mutex>
#include <string>
#include <vector>
#include <sys/system_properties.h>
#include <unistd.h>

// ── layer 协商接口 (来自 vk_layer.h, ABI 稳定) ──
//
// 陷阱 (2026-09-23 实测踩到, 层挂上了但 CreateInstance 找不到链): 这两个 sType 在
// vulkan_core.h 里是**枚举量, 不是宏**, 所以 `#ifndef` 恒为真 —— 早先版本用
// `#ifndef ... #define ((VkStructureType)1000000000)` 兜底, 结果是无条件覆盖成错值。
// 正确值就是核心枚举里的 47 / 48 (Android 的 libvulkan 也按这个填)。这里写死并
// static_assert 钉住, 不再给"兜底"留口子。
static_assert(VK_STRUCTURE_TYPE_LOADER_INSTANCE_CREATE_INFO == 47, "loader instance sType 必须是 47");
static_assert(VK_STRUCTURE_TYPE_LOADER_DEVICE_CREATE_INFO == 48, "loader device sType 必须是 48");

typedef enum { RK_LAYER_LINK_INFO = 0 } RkLayerFunction;

typedef struct RkInstLink {
    struct RkInstLink* pNext;
    PFN_vkGetInstanceProcAddr pfnNextGetInstanceProcAddr;
} RkInstLink;
typedef struct {
    VkStructureType sType;
    const void* pNext;
    RkLayerFunction function;
    union { RkInstLink* pLayerInfo; } u;
} RkInstCreateInfo;

typedef struct RkDevLink {
    struct RkDevLink* pNext;
    PFN_vkGetInstanceProcAddr pfnNextGetInstanceProcAddr;
    PFN_vkGetDeviceProcAddr pfnNextGetDeviceProcAddr;
} RkDevLink;
typedef struct {
    VkStructureType sType;
    const void* pNext;
    RkLayerFunction function;
    union { RkDevLink* pLayerInfo; } u;
} RkDevCreateInfo;

typedef struct {
    VkStructureType sType;
    void* pNext;
    uint32_t loaderLayerInterfaceVersion;
    PFN_vkGetInstanceProcAddr pfnGetInstanceProcAddr;
    PFN_vkGetDeviceProcAddr pfnGetDeviceProcAddr;
    PFN_vkVoidFunction pfnGetPhysicalDeviceProcAddr;
} RkNegotiate;

#define RK_EXPORT __attribute__((visibility("default")))

// 必须与 `setprop debug.vulkan.layers` 里写的名字逐字相同, 否则加载器枚举到了
// 也不会选中本层。(实测: 本机生效的是这个属性, 不是 settings global gpu_debug_layers)
#define RK_LAYER_NAME "VK_LAYER_refknobs_readonly"

extern "C" RK_EXPORT VkResult VKAPI_CALL
vkEnumerateInstanceLayerProperties(uint32_t*, VkLayerProperties*);
extern "C" RK_EXPORT VkResult VKAPI_CALL
vkEnumerateInstanceExtensionProperties(const char*, uint32_t*, VkExtensionProperties*);
extern "C" RK_EXPORT VkResult VKAPI_CALL
vkEnumerateDeviceLayerProperties(VkPhysicalDevice, uint32_t*, VkLayerProperties*);
extern "C" RK_EXPORT VkResult VKAPI_CALL
vkEnumerateDeviceExtensionProperties(VkPhysicalDevice, const char*, uint32_t*, VkExtensionProperties*);

// dispatchable handle 的 dispatch key = 其首个指针 (loader 约定)
static inline void* key(void* h) { return *reinterpret_cast<void**>(h); }

struct InstDisp {
    PFN_vkGetInstanceProcAddr gipa = nullptr;
    VkInstance instance = VK_NULL_HANDLE;
    PFN_vkDestroyInstance DestroyInstance = nullptr;
    PFN_vkEnumerateDeviceExtensionProperties EnumDevExt = nullptr;
};
struct DevDisp {
    PFN_vkGetDeviceProcAddr gdpa = nullptr;
    PFN_vkDestroyDevice DestroyDevice = nullptr;
    PFN_vkCmdBeginRenderPass CmdBeginRenderPass = nullptr;
    PFN_vkCmdBeginRenderPass2 CmdBeginRenderPass2 = nullptr;
    PFN_vkQueuePresentKHR QueuePresentKHR = nullptr;
    PFN_vkCreateRenderPass CreateRenderPass = nullptr;
    PFN_vkCreateRenderPass2 CreateRenderPass2 = nullptr;
    PFN_vkDestroyRenderPass DestroyRenderPass = nullptr;
};

static std::mutex g_mtx;
static std::map<void*, InstDisp> g_inst;
static std::map<void*, DevDisp> g_dev;
static std::atomic<uint64_t> g_frames{0};
static std::atomic<uint64_t> g_rp{0};

// ── 改写档: LoadOp LOAD → DONT_CARE ──
//
// 默认关闭, 靠属性 `debug.knobs.loadop=1` 打开 —— 同一个 .so 既是只读探针也是改写旋钮,
// 不用为两种模式各编一份, 也方便 harness 在 A/B 两臂之间只切一个开关。
//
// 只改 loadOp, **不动 initialLayout**。refbench 的白档 knob.loadop=on 除了 DONT_CARE
// 还把 initialLayout 设成 UNDEFINED(等于允许驱动直接丢弃旧内容), 所以本改写是白档那个
// 答案的**真子集**; 两者效果可能不完全相等, 这一点如实记在 FEASIBILITY 里, 不含糊过去。
static bool g_rewrite_loadop = false;
static std::atomic<uint64_t> g_rp_created{0};   // 第几个被创建的 render pass (自报里的 pass 序号)

struct Effective {
    uint64_t pass;        // render pass 创建序号
    uint32_t attachment;  // 该 pass 里的第几个 attachment
    // 这个被改写的 pass 被 vkCmdBeginRenderPass **录制**了多少次。
    // 注意是录制不是提交: 应用若预录命令缓冲反复提交, 这个数会偏小(录一次提交五千次);
    // 录了却没提交则会偏大。refbench 每帧重录, 所以那里的数字精确。
    // harness 只应把它当作"这条改写到底有没有被用上"的**布尔判据**, 不要当调用次数用。
    uint64_t begins;
};
static std::vector<Effective> g_effective;
// 被改写过的 VkRenderPass 句柄 -> 它对应的**全部** g_effective 下标。
// 一个 pass 可能有多个 attachment 被改, 只记最后一个会让其余条目永远 begins=0,
// harness 会把它们误读成"没生效"。
static std::map<VkRenderPass, std::vector<size_t>> g_rewritten;
// effective 不设上限会在长跑的真游戏里无限涨 (每 300 帧还要整份序列化+fsync)。
static const size_t RK_MAX_EFFECTIVE = 256;
static uint64_t g_effective_dropped = 0;

static bool read_bool_prop(const char* name) {
    char v[PROP_VALUE_MAX] = {0};
    if (__system_property_get(name, v) <= 0) return false;
    return v[0] == '1' || v[0] == 't' || v[0] == 'y';
}

// 证据双通道。原本只走文件系统, 因为立项时以为本机 logcat 哑掉 —— 2026-09-23 实测发现
// logcat 只是对 shell uid 不可读, `su -c logcat` 一切正常 (refbench 与原神都验过)。
// 文件通道在这两个应用上都能写, 但换个目标应用未必 (存储沙箱/SELinux 各不相同),
// 所以两条都留: 任何一条成立即可判定层挂上了。
#define RK_LOG(...) __android_log_print(ANDROID_LOG_INFO, "refknobs", __VA_ARGS__)

static std::string self_pkg() {
    char cmd[256] = {0};
    FILE* f = fopen("/proc/self/cmdline", "r");
    if (f) { size_t n = fread(cmd, 1, sizeof cmd - 1, f); cmd[n < sizeof cmd ? n : sizeof cmd - 1] = 0; fclose(f); }
    std::string pkg = cmd[0] ? std::string(cmd) : std::string("unknown");
    size_t colon = pkg.find(':');
    if (colon != std::string::npos) pkg = pkg.substr(0, colon);
    return pkg;
}

static void write_marker() {
    std::string pkg = self_pkg();

    // effective 按 contract/knob.md 的形状: 层只自报"实际改了什么", 不判断改得好不好。
    std::string eff;
    bool used = false;   // 有没有哪条改写真的被用上
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        for (const auto& e : g_effective) {
            char one[160];
            snprintf(one, sizeof one,
                     "%s{\"pass\":%llu,\"attachment\":%u,\"field\":\"loadOp\","
                     "\"from\":\"LOAD\",\"to\":\"DONT_CARE\",\"begins\":%llu}",
                     eff.empty() ? "" : ",", (unsigned long long)e.pass, e.attachment,
                     (unsigned long long)e.begins);
            eff += one;
            if (e.begins) used = true;
        }
    }
    // harness 据此判断该轮算不算数。两种"没生效"要分开报:
    //   没找到 LOAD 可改 vs 改了但应用根本没用那个 pass 对象
    const char* unavail = "null";
    if (g_rewrite_loadop && eff.empty())      unavail = "\"no LOAD attachment seen\"";
    else if (g_rewrite_loadop && !used)       unavail = "\"rewritten pass never bound\"";

    std::string json =
        std::string("{\"knob\":\"") + (g_rewrite_loadop ? "gray_loadop_dontcare" : "gray_readonly_probe")
        + "\",\"layer_loaded\":true,\"pkg\":\"" + pkg
        + "\",\"effective\":[" + eff + "],\"failed\":[],\"unavailable_reason\":" + unavail
        + ",\"readonly_stats\":{\"frames\":" + std::to_string(g_frames.load())
        + ",\"render_pass_begins\":" + std::to_string(g_rp.load())
        + ",\"render_passes_created\":" + std::to_string(g_rp_created.load()) + "}}";

    // 通道 1: logcat (root 可读, 不受目标应用存储沙箱影响)
    RK_LOG("%s", json.c_str());

    // 通道 2: 文件。三个候选落点, 头一个写得进就停。
    const std::string cands[] = {
        "/storage/emulated/0/Android/data/" + pkg + "/files/knobs_layer_out.json",
        "/data/data/" + pkg + "/knobs_layer_out.json",
        "/data/local/tmp/knobs_layer_out." + pkg + ".json",
    };
    for (const auto& path : cands) {
        FILE* o = fopen(path.c_str(), "w");
        if (!o) continue;
        fprintf(o, "%s\n", json.c_str());
        fflush(o); fsync(fileno(o)); fclose(o);
        return;
    }
    RK_LOG("marker file unwritable for pkg=%s (logcat 通道仍然成立)", pkg.c_str());
}

static PFN_vkVoidFunction dispatch_instance(VkInstance, const char*);
static PFN_vkVoidFunction dispatch_device(VkDevice, const char*);

// 应用问"这个物理设备有哪些扩展?"时, 只有 pLayerName 指名道姓问本层, 才轮到我们回答
// (答案是"本层不提供任何扩展")。pLayerName 为空 = 问的是底层实现, **必须转发**。
//
// 2026-09-23 实测: 早先版本无条件答 0 个扩展, 于是 refbench 找不到 VK_KHR_swapchain,
// 建不出交换链 (swapchain 0x0), frames_submitted=0 —— 层把应用弄坏了却不报错。
// 这是"只读层"最容易破功的地方: 只读指不改渲染行为, 不等于可以乱答枚举。
static VKAPI_ATTR VkResult VKAPI_CALL rk_EnumerateDeviceExtensionProperties(
    VkPhysicalDevice phys, const char* pLayerName, uint32_t* pCount, VkExtensionProperties* pProps) {
    if (pLayerName && strcmp(pLayerName, RK_LAYER_NAME) == 0) { *pCount = 0; return VK_SUCCESS; }
    PFN_vkEnumerateDeviceExtensionProperties next = nullptr;
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto it = g_inst.find(key(phys));
        // 物理设备与实例共用 dispatch key; 万一对不上而本进程只有一个实例, 就用那一个。
        if (it == g_inst.end() && g_inst.size() == 1) it = g_inst.begin();
        if (it != g_inst.end()) next = it->second.EnumDevExt;
    }
    // 查不到下层就**不能**假装"设备没有扩展" —— 那正是本函数要修的那个静默故障。
    // 宁可响亮地失败, 也不要让应用以为 VK_KHR_swapchain 不存在。
    if (!next) {
        RK_LOG("EnumerateDeviceExtensionProperties: 查不到下层函数, 如实报错而不是答 0 个扩展");
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    return next(phys, pLayerName, pCount, pProps);
}

// ── 拦截: instance ──
static VKAPI_ATTR VkResult VKAPI_CALL rk_CreateInstance(
    const VkInstanceCreateInfo* ci, const VkAllocationCallbacks* a, VkInstance* pInst) {
    g_rewrite_loadop = read_bool_prop("debug.knobs.loadop");
    RK_LOG("rk_CreateInstance entered (pkg=%s) rewrite_loadop=%d",
           self_pkg().c_str(), (int)g_rewrite_loadop);
    auto* link = reinterpret_cast<RkInstCreateInfo*>(const_cast<void*>(ci->pNext));
    while (link && !(link->sType == VK_STRUCTURE_TYPE_LOADER_INSTANCE_CREATE_INFO &&
                     link->function == RK_LAYER_LINK_INFO))
        link = reinterpret_cast<RkInstCreateInfo*>(const_cast<void*>(link->pNext));
    if (!link) {
        RK_LOG("no LOADER_INSTANCE_CREATE_INFO in pNext chain; dumping chain:");
        struct Hdr { VkStructureType sType; const void* pNext; };
        auto* h = reinterpret_cast<const Hdr*>(ci->pNext);
        for (int i = 0; h && i < 8; i++) {
            RK_LOG("  pNext[%d] sType=%d", i, (int)h->sType);
            h = reinterpret_cast<const Hdr*>(h->pNext);
        }
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    PFN_vkGetInstanceProcAddr gipa = link->u.pLayerInfo->pfnNextGetInstanceProcAddr;
    auto createFunc = (PFN_vkCreateInstance)gipa(VK_NULL_HANDLE, "vkCreateInstance");
    if (!createFunc) { RK_LOG("next vkCreateInstance is null"); return VK_ERROR_INITIALIZATION_FAILED; }
    link->u.pLayerInfo = link->u.pLayerInfo->pNext;  // 推进链
    VkResult r = createFunc(ci, a, pInst);
    RK_LOG("next vkCreateInstance -> %d", (int)r);
    if (r != VK_SUCCESS) return r;
    InstDisp d;
    d.gipa = gipa;
    d.instance = *pInst;
    d.DestroyInstance = (PFN_vkDestroyInstance)gipa(*pInst, "vkDestroyInstance");
    d.EnumDevExt = (PFN_vkEnumerateDeviceExtensionProperties)
        gipa(*pInst, "vkEnumerateDeviceExtensionProperties");
    { std::lock_guard<std::mutex> lk(g_mtx); g_inst[key(*pInst)] = d; }
    write_marker();  // 挂上就落标记 → 证明问题 1
    return r;
}

static VKAPI_ATTR void VKAPI_CALL rk_DestroyInstance(VkInstance inst, const VkAllocationCallbacks* a) {
    InstDisp d;
    { std::lock_guard<std::mutex> lk(g_mtx); auto it = g_inst.find(key(inst)); if (it == g_inst.end()) return; d = it->second; g_inst.erase(it); }
    if (d.DestroyInstance) d.DestroyInstance(inst, a);
}

static VKAPI_ATTR VkResult VKAPI_CALL rk_CreateDevice(
    VkPhysicalDevice phys, const VkDeviceCreateInfo* ci, const VkAllocationCallbacks* a, VkDevice* pDev) {
    auto* link = reinterpret_cast<RkDevCreateInfo*>(const_cast<void*>(ci->pNext));
    while (link && !(link->sType == VK_STRUCTURE_TYPE_LOADER_DEVICE_CREATE_INFO &&
                     link->function == RK_LAYER_LINK_INFO))
        link = reinterpret_cast<RkDevCreateInfo*>(const_cast<void*>(link->pNext));
    if (!link) { RK_LOG("no LOADER_DEVICE_CREATE_INFO in pNext chain"); return VK_ERROR_INITIALIZATION_FAILED; }
    PFN_vkGetInstanceProcAddr gipa = link->u.pLayerInfo->pfnNextGetInstanceProcAddr;
    PFN_vkGetDeviceProcAddr gdpa = link->u.pLayerInfo->pfnNextGetDeviceProcAddr;
    VkInstance inst = VK_NULL_HANDLE;
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto it = g_inst.find(key(phys));
        if (it == g_inst.end() && g_inst.size() == 1) it = g_inst.begin();
        if (it != g_inst.end()) inst = it->second.instance;
    }
    auto createFunc = (PFN_vkCreateDevice)gipa(inst, "vkCreateDevice");
    // inst 没查到时 gipa(VK_NULL_HANDLE, "vkCreateDevice") 按规范返回 NULL, 不能直接调
    if (!createFunc) { RK_LOG("next vkCreateDevice is null (inst lookup miss?)"); return VK_ERROR_INITIALIZATION_FAILED; }
    link->u.pLayerInfo = link->u.pLayerInfo->pNext;
    VkResult r = createFunc(phys, ci, a, pDev);
    if (r != VK_SUCCESS) return r;
    DevDisp d;
    d.gdpa = gdpa;
    d.DestroyDevice = (PFN_vkDestroyDevice)gdpa(*pDev, "vkDestroyDevice");
    d.CmdBeginRenderPass = (PFN_vkCmdBeginRenderPass)gdpa(*pDev, "vkCmdBeginRenderPass");
    d.CmdBeginRenderPass2 = (PFN_vkCmdBeginRenderPass2)gdpa(*pDev, "vkCmdBeginRenderPass2");
    d.QueuePresentKHR = (PFN_vkQueuePresentKHR)gdpa(*pDev, "vkQueuePresentKHR");
    d.CreateRenderPass = (PFN_vkCreateRenderPass)gdpa(*pDev, "vkCreateRenderPass");
    d.CreateRenderPass2 = (PFN_vkCreateRenderPass2)gdpa(*pDev, "vkCreateRenderPass2");
    if (!d.CreateRenderPass2)
        d.CreateRenderPass2 = (PFN_vkCreateRenderPass2)gdpa(*pDev, "vkCreateRenderPass2KHR");
    d.DestroyRenderPass = (PFN_vkDestroyRenderPass)gdpa(*pDev, "vkDestroyRenderPass");
    { std::lock_guard<std::mutex> lk(g_mtx); g_dev[key(*pDev)] = d; }
    return r;
}

static VKAPI_ATTR void VKAPI_CALL rk_DestroyDevice(VkDevice dev, const VkAllocationCallbacks* a) {
    DevDisp d;
    { std::lock_guard<std::mutex> lk(g_mtx); auto it = g_dev.find(key(dev)); if (it == g_dev.end()) return; d = it->second; g_dev.erase(it); }
    write_marker();  // 收尾再落一次最终计数
    if (d.DestroyDevice) d.DestroyDevice(dev, a);
}

// ── 拦截: 只读计数 ──
//
// 取下层函数指针。原来写的是 `g_dev[key(cb)].XXX`: map 的 operator[] 在查不到时会
// **默认构造一条记录**(于是拿到空指针), 紧接着就把空指针当函数调 —— 渲染线程当场段错误,
// 而且现象看起来像"游戏被反作弊杀了", 极难归因。这里改成只读查找 + 空指针校验。
// 这几条失败分支一旦触发就是每次 draw 都触发, 直接 RK_LOG 会把 logcat 刷爆
// (共享设备上尤其不礼貌)。只报第一次。
#define RK_LOG_ONCE(...) do { static std::atomic<bool> said{false}; \
    if (!said.exchange(true)) RK_LOG(__VA_ARGS__); } while (0)

static bool dev_of(void* k, DevDisp* out) {
    std::lock_guard<std::mutex> lk(g_mtx);
    auto it = g_dev.find(k);
    // 队列/命令缓冲与设备共用 dispatch key; 万一对不上而本进程只有一个设备, 就用那一个。
    if (it == g_dev.end() && g_dev.size() == 1) it = g_dev.begin();
    if (it == g_dev.end()) return false;
    *out = it->second;
    return true;
}

// ── 改写: LoadOp LOAD → DONT_CARE ──
//
// 钩 vkCreateRenderPass 而不是 vkCmdBeginRenderPass: loadOp 是**创建期**就烘进
// VkRenderPass 对象的, BeginRenderPass 时已经改不动了。
//
// 只在 g_rewrite_loadop 打开时改; 关着的时候这两个钩子就是纯转发, 与只读档等价。
static VKAPI_ATTR VkResult VKAPI_CALL rk_CreateRenderPass(
    VkDevice dev, const VkRenderPassCreateInfo* ci, const VkAllocationCallbacks* a, VkRenderPass* out) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.CreateRenderPass) {
        RK_LOG_ONCE("CreateRenderPass: 查不到下层函数");
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    uint64_t idx = g_rp_created++;
    if (!g_rewrite_loadop || !ci || ci->attachmentCount == 0)
        return d.CreateRenderPass(dev, ci, a, out);

    // pAttachments 是 const, 必须整份拷出来再改
    std::vector<VkAttachmentDescription> atts(ci->pAttachments, ci->pAttachments + ci->attachmentCount);
    std::vector<Effective> hits;
    for (uint32_t i = 0; i < atts.size(); i++) {
        if (atts[i].loadOp != VK_ATTACHMENT_LOAD_OP_LOAD) continue;
        atts[i].loadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE;
        hits.push_back({idx, i, 0});
    }
    if (hits.empty()) return d.CreateRenderPass(dev, ci, a, out);

    VkRenderPassCreateInfo mod = *ci;
    mod.pAttachments = atts.data();
    VkResult r = d.CreateRenderPass(dev, &mod, a, out);
    if (r != VK_SUCCESS) {   // 改写导致创建失败就如实报, 不偷偷回退成原样
        RK_LOG("CreateRenderPass(改写后) 失败: %d, pass=%llu", (int)r, (unsigned long long)idx);
        return r;
    }
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto& idxs = g_rewritten[*out];
        for (auto& h : hits) {
            if (g_effective.size() >= RK_MAX_EFFECTIVE) { g_effective_dropped++; continue; }
            idxs.push_back(g_effective.size());
            g_effective.push_back(h);
        }
    }
    RK_LOG("改写 pass=%llu: %zu 个 attachment 的 loadOp LOAD -> DONT_CARE",
           (unsigned long long)idx, hits.size());
    return r;
}

static VKAPI_ATTR VkResult VKAPI_CALL rk_CreateRenderPass2(
    VkDevice dev, const VkRenderPassCreateInfo2* ci, const VkAllocationCallbacks* a, VkRenderPass* out) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.CreateRenderPass2) {
        RK_LOG_ONCE("CreateRenderPass2: 查不到下层函数");
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    uint64_t idx = g_rp_created++;
    if (!g_rewrite_loadop || !ci || ci->attachmentCount == 0)
        return d.CreateRenderPass2(dev, ci, a, out);

    std::vector<VkAttachmentDescription2> atts(ci->pAttachments, ci->pAttachments + ci->attachmentCount);
    std::vector<Effective> hits;
    for (uint32_t i = 0; i < atts.size(); i++) {
        if (atts[i].loadOp != VK_ATTACHMENT_LOAD_OP_LOAD) continue;
        atts[i].loadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE;
        hits.push_back({idx, i, 0});
    }
    if (hits.empty()) return d.CreateRenderPass2(dev, ci, a, out);

    VkRenderPassCreateInfo2 mod = *ci;
    mod.pAttachments = atts.data();
    VkResult r = d.CreateRenderPass2(dev, &mod, a, out);
    if (r != VK_SUCCESS) {
        RK_LOG("CreateRenderPass2(改写后) 失败: %d, pass=%llu", (int)r, (unsigned long long)idx);
        return r;
    }
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto& idxs = g_rewritten[*out];
        for (auto& h : hits) {
            if (g_effective.size() >= RK_MAX_EFFECTIVE) { g_effective_dropped++; continue; }
            idxs.push_back(g_effective.size());
            g_effective.push_back(h);
        }
    }
    RK_LOG("改写2 pass=%llu: %zu 个 attachment 的 loadOp LOAD -> DONT_CARE",
           (unsigned long long)idx, hits.size());
    return r;
}

// 记一次"被改写的 render pass 真的被用来开 pass 了"。
// 没有这个计数, effective 只能说明"我改过这个对象", 说不了"应用真的用了它" ——
// 实测 refbench 会把 LOAD 和 DONT_CARE 两个 pass 对象都建出来、只绑其中一个,
// 于是不加区分的 effective 会把一次空改写报成生效。
static void note_begin(VkRenderPass rp) {
    // 只读档没有任何改写, 这里不该占 CmdBeginRenderPass 的热路径去抢全局锁
    if (!g_rewrite_loadop || rp == VK_NULL_HANDLE) return;
    std::lock_guard<std::mutex> lk(g_mtx);
    auto it = g_rewritten.find(rp);
    if (it == g_rewritten.end()) return;
    for (size_t i : it->second) g_effective[i].begins++;
}

// 句柄回收是真事: 驱动会复用非 dispatchable handle 的数值。不摘掉已销毁的句柄, 后来
// 某个**没被改写**的 pass 复用到同一个数值, 就会把 begins 记到一条已死的改写上 ——
// 正好是 begins 这个计数要防的那种假阳性。
static VKAPI_ATTR void VKAPI_CALL rk_DestroyRenderPass(
    VkDevice dev, VkRenderPass rp, const VkAllocationCallbacks* a) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.DestroyRenderPass) {
        RK_LOG_ONCE("DestroyRenderPass: 查不到下层函数");
        return;
    }
    { std::lock_guard<std::mutex> lk(g_mtx); g_rewritten.erase(rp); }
    d.DestroyRenderPass(dev, rp, a);
}

static VKAPI_ATTR void VKAPI_CALL rk_CmdBeginRenderPass(
    VkCommandBuffer cb, const VkRenderPassBeginInfo* bi, VkSubpassContents c) {
    DevDisp d;
    if (!dev_of(key(cb), &d) || !d.CmdBeginRenderPass) {
        RK_LOG_ONCE("CmdBeginRenderPass: 查不到下层函数, 本次不计数也不转发");
        return;
    }
    g_rp++;
    note_begin(bi ? bi->renderPass : VK_NULL_HANDLE);
    d.CmdBeginRenderPass(cb, bi, c);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdBeginRenderPass2(
    VkCommandBuffer cb, const VkRenderPassBeginInfo* bi, const VkSubpassBeginInfo* si) {
    DevDisp d;
    // 驱动不支持 RenderPass2 时这里就是空的 (应用可能从 instance gipa 拿到本钩子,
    // 那条路没有 dispatch_device 里的可用性判断)。宁可不转发也不能调空指针。
    if (!dev_of(key(cb), &d) || !d.CmdBeginRenderPass2) {
        RK_LOG_ONCE("CmdBeginRenderPass2: 查不到下层函数, 本次不计数也不转发");
        return;
    }
    g_rp++;
    note_begin(bi ? bi->renderPass : VK_NULL_HANDLE);
    d.CmdBeginRenderPass2(cb, bi, si);
}
static VKAPI_ATTR VkResult VKAPI_CALL rk_QueuePresentKHR(VkQueue q, const VkPresentInfoKHR* pi) {
    DevDisp d;
    if (!dev_of(key(q), &d) || !d.QueuePresentKHR) {
        RK_LOG_ONCE("QueuePresentKHR: 查不到下层函数");
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    uint64_t f = ++g_frames;
    if ((f % 300) == 0) write_marker();  // 周期落盘, 无需等退出
    return d.QueuePresentKHR(q, pi);
}

// ── proc addr 分发 ──
#define RK_HOOK(n, fn) if (!strcmp(name, n)) return reinterpret_cast<PFN_vkVoidFunction>(fn)

static PFN_vkVoidFunction dispatch_instance(VkInstance instance, const char* name) {
    RK_HOOK("vkGetInstanceProcAddr", dispatch_instance);
    // 注意: vkEnumerateInstance{Layer,Extension}Properties 这里**不挂**。那两个导出符号
    // 只为加载器 dlsym 发现层而存在; 应用调的是加载器自己的全局版本, 挂上去只会把
    // "本层没有扩展" 当成 "整个实例没有扩展" 答回去。
    // vkEnumerateDeviceExtensionProperties 必须挂**转发版**, 原因见 rk_EnumerateDeviceExtensionProperties。
    RK_HOOK("vkEnumerateDeviceExtensionProperties", rk_EnumerateDeviceExtensionProperties);
    RK_HOOK("vkCreateInstance", rk_CreateInstance);
    RK_HOOK("vkDestroyInstance", rk_DestroyInstance);
    RK_HOOK("vkCreateDevice", rk_CreateDevice);
    RK_HOOK("vkGetDeviceProcAddr", dispatch_device);
    // 设备级钩子也从 instance gipa 暴露, 供 loader 建表
    RK_HOOK("vkDestroyDevice", rk_DestroyDevice);
    RK_HOOK("vkCmdBeginRenderPass", rk_CmdBeginRenderPass);
    RK_HOOK("vkCmdBeginRenderPass2", rk_CmdBeginRenderPass2);
    RK_HOOK("vkCmdBeginRenderPass2KHR", rk_CmdBeginRenderPass2);
    RK_HOOK("vkCreateRenderPass", rk_CreateRenderPass);
    RK_HOOK("vkDestroyRenderPass", rk_DestroyRenderPass);
    RK_HOOK("vkCreateRenderPass2", rk_CreateRenderPass2);
    RK_HOOK("vkCreateRenderPass2KHR", rk_CreateRenderPass2);
    RK_HOOK("vkQueuePresentKHR", rk_QueuePresentKHR);
    if (instance == VK_NULL_HANDLE) return nullptr;
    InstDisp d;
    { std::lock_guard<std::mutex> lk(g_mtx); auto it = g_inst.find(key(instance)); if (it == g_inst.end()) return nullptr; d = it->second; }
    return d.gipa ? d.gipa(instance, name) : nullptr;
}

static PFN_vkVoidFunction dispatch_device(VkDevice dev, const char* name) {
    RK_HOOK("vkGetDeviceProcAddr", dispatch_device);
    RK_HOOK("vkDestroyDevice", rk_DestroyDevice);
    RK_HOOK("vkQueuePresentKHR", rk_QueuePresentKHR);
    RK_HOOK("vkCmdBeginRenderPass", rk_CmdBeginRenderPass);
    RK_HOOK("vkCreateRenderPass", rk_CreateRenderPass);
    RK_HOOK("vkDestroyRenderPass", rk_DestroyRenderPass);
    if (dev == VK_NULL_HANDLE) return nullptr;
    DevDisp d;
    { std::lock_guard<std::mutex> lk(g_mtx); auto it = g_dev.find(key(dev)); if (it == g_dev.end()) return nullptr; d = it->second; }
    // 可选功能只在底层存在时才返回我们的钩子, 否则原样转发, 避免调到空指针
    if (!strcmp(name, "vkCmdBeginRenderPass2") || !strcmp(name, "vkCmdBeginRenderPass2KHR"))
        return d.CmdBeginRenderPass2 ? reinterpret_cast<PFN_vkVoidFunction>(rk_CmdBeginRenderPass2)
                                     : (d.gdpa ? d.gdpa(dev, name) : nullptr);
    if (!strcmp(name, "vkCreateRenderPass2") || !strcmp(name, "vkCreateRenderPass2KHR"))
        return d.CreateRenderPass2 ? reinterpret_cast<PFN_vkVoidFunction>(rk_CreateRenderPass2)
                                   : (d.gdpa ? d.gdpa(dev, name) : nullptr);
    return d.gdpa ? d.gdpa(dev, name) : nullptr;
}

// ── 枚举接口 ──
//
// Android 的 Vulkan 加载器**不读 JSON manifest** (那是桌面 loader 的约定), 它直接对 .so
// dlsym。layers_extensions.cpp::LayerLibrary::EnumerateLayers 拿不到下面两个符号就判
//   E vulkan: layer library '...' missing some instance enumeration functions
// 并整个丢弃这个层 —— 2026-09-23 在 refbench 上实测到过。所以这两个是上机硬要求。
static const VkLayerProperties kLayerProps = {
    RK_LAYER_NAME,
    VK_MAKE_VERSION(1, 1, 0),  // spec version
    1,                         // implementation version
    "refknobs read-only probe: pass-through + render pass counting",
};

// 按 Vulkan 约定回填数组并处理"缓冲区不够"的 VK_INCOMPLETE。
static VkResult copy_layer_props(uint32_t* pCount, VkLayerProperties* pProps) {
    if (!pProps) { *pCount = 1; return VK_SUCCESS; }
    if (*pCount < 1) { *pCount = 0; return VK_INCOMPLETE; }
    *pProps = kLayerProps;
    *pCount = 1;
    return VK_SUCCESS;
}

// 本层不引入任何扩展, 恒定返回 0 个。pLayerName 为空 = 问的是底层实现, 不归我们答。
static VkResult zero_extensions(const char* pLayerName, uint32_t* pCount) {
    if (pLayerName && strcmp(pLayerName, RK_LAYER_NAME) != 0) return VK_ERROR_LAYER_NOT_PRESENT;
    *pCount = 0;
    return VK_SUCCESS;
}

extern "C" RK_EXPORT VkResult VKAPI_CALL
vkEnumerateInstanceLayerProperties(uint32_t* pCount, VkLayerProperties* pProps) {
    return copy_layer_props(pCount, pProps);
}
extern "C" RK_EXPORT VkResult VKAPI_CALL
vkEnumerateInstanceExtensionProperties(const char* pLayerName, uint32_t* pCount, VkExtensionProperties*) {
    return zero_extensions(pLayerName, pCount);
}
// 设备级枚举在加载器里是可选的, 但给全避免不同 Android 版本的分歧。
extern "C" RK_EXPORT VkResult VKAPI_CALL
vkEnumerateDeviceLayerProperties(VkPhysicalDevice, uint32_t* pCount, VkLayerProperties* pProps) {
    return copy_layer_props(pCount, pProps);
}
extern "C" RK_EXPORT VkResult VKAPI_CALL
vkEnumerateDeviceExtensionProperties(VkPhysicalDevice, const char* pLayerName, uint32_t* pCount,
                                     VkExtensionProperties*) {
    return zero_extensions(pLayerName, pCount);
}

// ── 导出入口 ──
extern "C" RK_EXPORT VkResult VKAPI_CALL vkNegotiateLoaderLayerInterfaceVersion(RkNegotiate* v) {
    if (v->loaderLayerInterfaceVersion > 2) v->loaderLayerInterfaceVersion = 2;
    v->pfnGetInstanceProcAddr = dispatch_instance;
    v->pfnGetDeviceProcAddr = dispatch_device;
    v->pfnGetPhysicalDeviceProcAddr = nullptr;
    return VK_SUCCESS;
}
extern "C" RK_EXPORT PFN_vkVoidFunction VKAPI_CALL vkGetInstanceProcAddr(VkInstance i, const char* n) {
    return dispatch_instance(i, n);
}
extern "C" RK_EXPORT PFN_vkVoidFunction VKAPI_CALL vkGetDeviceProcAddr(VkDevice d, const char* n) {
    return dispatch_device(d, n);
}
