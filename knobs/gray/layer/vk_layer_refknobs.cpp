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
#include <cstdlib>
#include <cstring>
#include <map>
#include <mutex>
#include <set>
#include <string>
#include <vector>
#include <sys/system_properties.h>
#include <unistd.h>
#if __has_include("copy_spv.h")
#include "copy_spv.h"
#else
static const unsigned char kCopySpv[] = {0};
static const unsigned int kCopySpvLen = 0;
static const unsigned char kUpopSpv[] = {0};
static const unsigned int kUpopSpvLen = 0;
#endif

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
    PFN_vkCreateFramebuffer CreateFramebuffer = nullptr;
    PFN_vkCreateSwapchainKHR CreateSwapchainKHR = nullptr;
    PFN_vkCmdDraw CmdDraw = nullptr;
    PFN_vkCmdDrawIndexed CmdDrawIndexed = nullptr;
    PFN_vkCmdBlitImage CmdBlitImage = nullptr;
    PFN_vkCreateImage CreateImage = nullptr;
    PFN_vkCreateImageView CreateImageView = nullptr;
    PFN_vkUpdateDescriptorSets UpdateDescriptorSets = nullptr;
    PFN_vkCmdBindDescriptorSets CmdBindDescriptorSets = nullptr;
    PFN_vkBeginCommandBuffer BeginCommandBuffer = nullptr;
    PFN_vkCmdExecuteCommands CmdExecuteCommands = nullptr;
    PFN_vkQueueSubmit QueueSubmit = nullptr;
    PFN_vkFreeCommandBuffers FreeCommandBuffers = nullptr;
    PFN_vkDestroyFramebuffer DestroyFramebuffer = nullptr;
    PFN_vkDestroyImage DestroyImage = nullptr;
    PFN_vkDestroyImageView DestroyImageView = nullptr;
    // copyprobe: 建 pipeline / 显存 / 描述符 / 注入 dispatch 用
    PFN_vkCreateShaderModule CreateShaderModule = nullptr;
    PFN_vkDestroyShaderModule DestroyShaderModule = nullptr;
    PFN_vkCreatePipelineLayout CreatePipelineLayout = nullptr;
    PFN_vkDestroyPipelineLayout DestroyPipelineLayout = nullptr;
    PFN_vkCreateComputePipelines CreateComputePipelines = nullptr;
    PFN_vkDestroyPipeline DestroyPipeline = nullptr;
    PFN_vkCreateDescriptorSetLayout CreateDescriptorSetLayout = nullptr;
    PFN_vkDestroyDescriptorSetLayout DestroyDescriptorSetLayout = nullptr;
    PFN_vkCreateDescriptorPool CreateDescriptorPool = nullptr;
    PFN_vkDestroyDescriptorPool DestroyDescriptorPool = nullptr;
    PFN_vkAllocateDescriptorSets AllocateDescriptorSets = nullptr;
    PFN_vkFreeDescriptorSets FreeDescriptorSets = nullptr;
    PFN_vkAllocateMemory AllocateMemory = nullptr;
    PFN_vkFreeMemory FreeMemory = nullptr;
    PFN_vkBindImageMemory BindImageMemory = nullptr;
    PFN_vkCmdPipelineBarrier CmdPipelineBarrier = nullptr;
    PFN_vkCmdBindPipeline CmdBindPipeline = nullptr;
    PFN_vkCmdDispatch CmdDispatch = nullptr;
    // frameaudit: 一帧步骤清单要看的命令 (只读转发) + 时间戳
    PFN_vkCmdEndRenderPass CmdEndRenderPass = nullptr;
    PFN_vkCmdEndRenderPass2 CmdEndRenderPass2 = nullptr;
    PFN_vkCmdNextSubpass CmdNextSubpass = nullptr;
    PFN_vkCmdNextSubpass2 CmdNextSubpass2 = nullptr;
    PFN_vkCmdBeginRendering CmdBeginRendering = nullptr;
    PFN_vkCmdEndRendering CmdEndRendering = nullptr;
    PFN_vkCmdDrawIndirect CmdDrawIndirect = nullptr;
    PFN_vkCmdDrawIndexedIndirect CmdDrawIndexedIndirect = nullptr;
    PFN_vkCmdDispatchIndirect CmdDispatchIndirect = nullptr;
    PFN_vkCmdCopyImage CmdCopyImage = nullptr;
    PFN_vkCmdCopyBuffer CmdCopyBuffer = nullptr;
    PFN_vkCmdCopyBufferToImage CmdCopyBufferToImage = nullptr;
    PFN_vkCmdCopyImageToBuffer CmdCopyImageToBuffer = nullptr;
    PFN_vkCmdResolveImage CmdResolveImage = nullptr;
    PFN_vkCmdClearColorImage CmdClearColorImage = nullptr;
    PFN_vkCmdClearDepthStencilImage CmdClearDepthStencilImage = nullptr;
    PFN_vkCmdClearAttachments CmdClearAttachments = nullptr;
    PFN_vkCmdFillBuffer CmdFillBuffer = nullptr;
    PFN_vkCmdUpdateBuffer CmdUpdateBuffer = nullptr;
    PFN_vkCmdPipelineBarrier2 CmdPipelineBarrier2 = nullptr;
    PFN_vkQueueSubmit2 QueueSubmit2 = nullptr;
    PFN_vkGetSwapchainImagesKHR GetSwapchainImagesKHR = nullptr;
    PFN_vkCreateQueryPool CreateQueryPool = nullptr;
    PFN_vkDestroyQueryPool DestroyQueryPool = nullptr;
    PFN_vkCmdResetQueryPool CmdResetQueryPool = nullptr;
    PFN_vkCmdWriteTimestamp CmdWriteTimestamp = nullptr;
    PFN_vkGetQueryPoolResults GetQueryPoolResults = nullptr;
    PFN_vkCmdBeginQuery CmdBeginQuery = nullptr;            // frameaudit: pass 里有没有查询 (遮挡查询的 pass 不能当白画)
    PFN_vkCmdEndQuery CmdEndQuery = nullptr;
    PFN_vkCmdCopyQueryPoolResults CmdCopyQueryPoolResults = nullptr;
    // 描述符模板 / push 描述符: Unity 6 等引擎不走 vkUpdateDescriptorSets, 不钩就看不见「读」
    PFN_vkCreateDescriptorUpdateTemplate CreateDescriptorUpdateTemplate = nullptr;
    PFN_vkDestroyDescriptorUpdateTemplate DestroyDescriptorUpdateTemplate = nullptr;
    PFN_vkUpdateDescriptorSetWithTemplate UpdateDescriptorSetWithTemplate = nullptr;
    PFN_vkCmdPushDescriptorSetKHR CmdPushDescriptorSetKHR = nullptr;
    PFN_vkCmdPushDescriptorSetWithTemplateKHR CmdPushDescriptorSetWithTemplateKHR = nullptr;
    float ts_period_ns = 0.f;                               // VkPhysicalDeviceLimits::timestampPeriod
    VkDevice dev = VK_NULL_HANDLE;
    VkPhysicalDevice phys = VK_NULL_HANDLE;                 // 查显存类型用
    PFN_vkGetPhysicalDeviceMemoryProperties GetPhysMemProps = nullptr;  // instance 级, CreateDevice 时经 gipa 取
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
static bool g_copyprobe = false;   // debug.knobs.copyprobe=1: 1:1 拷贝探针 (是改写, 非只读)
static bool g_upop = false;        // debug.knobs.upop=1: 真超分算子 (gen1_loc3), dst 是送显分辨率
// debug.knobs.frameaudit=1: 只读的「一帧步骤清单」(每个 pass 的附件/尺寸/格式/loadOp/storeOp、
// pass 之外的拷贝/清屏/blit/resolve/dispatch、draw 数、描述符引用了哪些图), 隐含 passdump 溯源。
// 不改任何渲染; 带 debug.knobs.frameaudit_ts=1 时另在 pass 前后插 GPU 时间戳 (只多写查询, 不碰画面)。
static bool g_frameaudit = false;
static bool g_frameaudit_ts = false;
static std::atomic<uint64_t> g_rp_created{0};   // 第几个被创建的 render pass (自报里的 pass 序号)

struct Effective {
    uint64_t pass;        // render pass 创建序号
    uint32_t attachment;  // 该 pass 里的第几个 attachment
    const char* field;    // "loadOp" / "storeOp" / "stencilStoreOp"
    const char* from;
    const char* to;
    // 这个被改写的 pass 被 vkCmdBeginRenderPass **录制**了多少次。
    // 注意是录制不是提交: 应用若预录命令缓冲反复提交, 这个数会偏小(录一次提交五千次);
    // 录了却没提交则会偏大。refbench 每帧重录, 所以那里的数字精确。
    // harness 只应把它当作"这条改写到底有没有被用上"的**布尔判据**, 不要当调用次数用。
    uint64_t begins;
};
static std::vector<Effective> g_effective;

// ── 改写: StoreOp STORE → DONT_CARE（阶段 2「去掉多余写回」开关）──
// 分块 GPU 上，一个 pass 结束时把附件从 tile 写回显存要花带宽；之后没人再读的附件（典型是深度/模板）
// 写回纯属浪费。属性 debug.knobs.storeop 取值：
//   "ds"                  所有被 subpass 当深度/模板用的附件，storeOp 与 stencilStoreOp 的 STORE → DONT_CARE
//   "<sig>:<i>[,<sig>:<i>…]"  只改特征码以 <sig> 开头（≥ 8 位十六进制，取自 frame-audit 的 "sig"）的 pass 的第 i 个附件
//   空 / "0"               关
// 送显的附件（finalLayout = PRESENT_SRC）永远不改。是否真被用上照旧看 effective[].begins。
// 风险：之后若有 pass 采样了被改的附件，画面会出错 —— 所以阶段 2 必须配画面比对，不能只看帧时。
static int g_storeop_mode = 0;                                  // 0 关 / 1 ds / 2 指定
static std::vector<std::pair<std::string, uint32_t>> g_storeop_pick;   // (sig 前缀, 附件下标)
static std::map<VkRenderPass, uint64_t> g_rp_sig;               // 改写前原始描述的特征码
// 现场切换 (debug.knobs.storeop_live=1): 应用拿到的是**原样**的 render pass, 层另建一个只差
// load/store 的「孪生」pass; 开关开着时在 BeginRenderPass 里换成孪生的。两者按规范兼容
// (兼容性不看 load/store op), framebuffer 与 pipeline 照用。好处: 同一局游戏里随时开关,
// 同一位置同一视角截图做逐像素比对, 不用重启 (重启后视角/动画都对不齐)。
// 开关状态由 debug.knobs.storeop_on 控制, 每 30 帧读一次。
static bool g_storeop_live = false;
static std::atomic<bool> g_storeop_on{false};
static std::map<VkRenderPass, VkRenderPass> g_twin;              // 原样 pass -> 改写后的孪生

// pass 特征码：附件个数、subpass 个数、每个附件的格式/采样/load/store/stencil load/store/初末布局 的 FNV-1a。
// 用原始（改写前）描述算，所以和不挂改写时 frame-audit 报出来的 sig 一致，可以直接拿来写规则。
template <typename Att>
static uint64_t rp_sig(uint32_t n_att, const Att* a, uint32_t n_sub) {
    uint64_t h = 1469598103934665603ULL;
    auto mix = [&](uint32_t v) { for (int k = 0; k < 4; k++) { h ^= (v >> (8 * k)) & 0xFF; h *= 1099511628211ULL; } };
    mix(n_att); mix(n_sub);
    for (uint32_t i = 0; i < n_att; i++) {
        mix(a[i].format); mix(a[i].samples); mix(a[i].loadOp); mix(a[i].storeOp);
        mix(a[i].stencilLoadOp); mix(a[i].stencilStoreOp); mix(a[i].initialLayout); mix(a[i].finalLayout);
    }
    return h;
}
static std::string sig_hex(uint64_t s) { char b[17]; snprintf(b, sizeof b, "%016llx", (unsigned long long)s); return b; }

static void parse_storeop_prop(const char* v) {
    g_storeop_mode = 0; g_storeop_pick.clear();
    if (!v || !v[0] || !strcmp(v, "0")) return;
    if (!strcmp(v, "ds")) { g_storeop_mode = 1; return; }
    std::string all(v);
    size_t pos = 0;
    while (pos <= all.size()) {
        size_t comma = all.find(',', pos);
        std::string tok = all.substr(pos, comma == std::string::npos ? std::string::npos : comma - pos);
        size_t colon = tok.find(':');
        if (colon != std::string::npos && colon >= 8)
            g_storeop_pick.push_back({tok.substr(0, colon), (uint32_t)strtoul(tok.c_str() + colon + 1, nullptr, 10)});
        if (comma == std::string::npos) break;
        pos = comma + 1;
    }
    if (!g_storeop_pick.empty()) g_storeop_mode = 2;
}

// 一个 pass 里哪些附件被当深度/模板用（两种 CreateInfo 的 subpass 结构字段名相同）
template <typename CI>
static std::vector<bool> ds_roles(const CI* ci) {
    std::vector<bool> ds(ci->attachmentCount, false);
    for (uint32_t s = 0; s < ci->subpassCount; s++) {
        auto* r = ci->pSubpasses[s].pDepthStencilAttachment;
        if (r && r->attachment != VK_ATTACHMENT_UNUSED && r->attachment < ds.size()) ds[r->attachment] = true;
    }
    return ds;
}

// 对拷出来的附件数组施加 storeOp 改写，命中的记进 hits
template <typename CI, typename Att>
static void apply_storeop(const CI* ci, Att* atts, uint64_t idx, uint64_t sig, std::vector<Effective>& hits) {
    if (!g_storeop_mode) return;
    std::vector<bool> ds = ds_roles(ci);
    std::string sh = sig_hex(sig);
    for (uint32_t i = 0; i < ci->attachmentCount; i++) {
        if (atts[i].finalLayout == VK_IMAGE_LAYOUT_PRESENT_SRC_KHR) continue;
        bool pick = false;
        if (g_storeop_mode == 1) pick = ds[i];
        else for (auto& p : g_storeop_pick) if (p.second == i && sh.compare(0, p.first.size(), p.first) == 0) pick = true;
        if (!pick) continue;
        if (atts[i].storeOp == VK_ATTACHMENT_STORE_OP_STORE) {
            atts[i].storeOp = VK_ATTACHMENT_STORE_OP_DONT_CARE;
            hits.push_back({idx, i, "storeOp", "STORE", "DONT_CARE", 0});
        }
        if (atts[i].stencilStoreOp == VK_ATTACHMENT_STORE_OP_STORE) {
            atts[i].stencilStoreOp = VK_ATTACHMENT_STORE_OP_DONT_CARE;
            hits.push_back({idx, i, "stencilStoreOp", "STORE", "DONT_CARE", 0});
        }
    }
}
// 被改写过的 VkRenderPass 句柄 -> 它对应的**全部** g_effective 下标。
// 一个 pass 可能有多个 attachment 被改, 只记最后一个会让其余条目永远 begins=0,
// harness 会把它们误读成"没生效"。
static std::map<VkRenderPass, std::vector<size_t>> g_rewritten;
// effective 不设上限会在长跑的真游戏里无限涨 (每 300 帧还要整份序列化+fsync)。
static const size_t RK_MAX_EFFECTIVE = 256;
static uint64_t g_effective_dropped = 0;

// ── 观测档: 导出 render pass 形状表 (为"低分辨率 -> 放大到屏幕"那一步定位) ──
//
// 目标是回答四件事: 每个 pass 的**尺寸**(framebuffer 宽高)、**格式**、**在一帧里的次序**、
// 以及**谁是最后一个屏幕分辨率的 pass**(UI 通常画在它里面或它之后)。
// 尺寸不在 VkRenderPass 上, 在 VkFramebuffer 上, 所以两边都要记, 靠 BeginRenderPass 关联。
static bool g_dump_passes = false;

struct RpInfo { uint32_t n_att = 0; std::vector<uint32_t> fmt; std::vector<uint32_t> loadop;
                std::vector<uint32_t> final_layout; };   // copyprobe 注 barrier 要知道源图 pass 结束后的布局
struct FbInfo { uint32_t w = 0, h = 0; VkRenderPass rp = VK_NULL_HANDLE; };
struct PassStat {                      // 按 (renderpass, 宽, 高) 聚合
    uint32_t w = 0, h = 0, n_att = 0;
    std::vector<uint32_t> fmt;
    uint64_t begins = 0, draws = 0;
    // 这个 pass 在多少个**帧**里被真正提交执行过 (QueueSubmit 路径统计)。
    // begins 数的是录制, 预录命令缓冲只录一次却每帧都跑 —— 过滤"每帧都在跑的 pass"
    // 必须用 frames_seen, 用 begins 会把最想看的预录 pass 滤掉 (评审 #4)。
    uint64_t frames_seen = 0;
};
static std::map<VkRenderPass, RpInfo> g_rp_info;
static std::map<VkFramebuffer, FbInfo> g_fb_info;
static std::map<uint64_t, PassStat> g_pass_stat;          // key = rp_seq<<32 | (w<<16|h) 的稳定散列
static std::map<VkCommandBuffer, uint64_t> g_cb_cur;      // 命令缓冲当前在哪个 pass 里
static std::map<VkCommandBuffer, std::vector<uint64_t>> g_cb_passes;  // 这条 cb 录制过哪些 pass (有序)
static std::set<uint64_t> g_frame_seen;                 // 本帧被提交执行的 pass key 集合
static std::map<VkRenderPass, uint64_t> g_rp_seq;         // VkRenderPass -> 创建序号(稳定可读)
static std::vector<std::string> g_frame_seq;              // 采样帧内的有序 pass 序列
static bool g_sampling = false;
static bool g_seq_done = false;
static uint32_t g_swap_w = 0, g_swap_h = 0, g_swap_fmt = 0;

struct CopyProbe {
    VkDevice dev = VK_NULL_HANDLE;
    VkShaderModule sm = VK_NULL_HANDLE;
    VkDescriptorSetLayout dsl = VK_NULL_HANDLE;
    VkPipelineLayout pl = VK_NULL_HANDLE;
    VkPipeline pipe = VK_NULL_HANDLE;
    VkDescriptorPool pool = VK_NULL_HANDLE;
    VkDescriptorSet cset = VK_NULL_HANDLE;     // compute 自己的 set: binding0=src, binding1=dst
    VkImage dst = VK_NULL_HANDLE;
    VkDeviceMemory mem = VK_NULL_HANDLE;
    VkImageView dst_view = VK_NULL_HANDLE;
    uint32_t w = 0, h = 0, fmt = 0;
    VkImage src_img = VK_NULL_HANDLE;          // 本帧拷贝的源 (pass 49 的颜色附件)
    uint64_t copies = 0, subs = 0;
    bool disabled = false;
    // 游戏 set -> (克隆集, 克隆时的写代数)。游戏每更新一次原 set, 代数 +1, 下次克隆重建。
    std::map<VkDescriptorSet, std::pair<VkDescriptorSet, uint64_t>> clones;
};
static CopyProbe g_cp;
static std::map<VkDescriptorSet, uint64_t> g_set_gen;   // UpdateDescriptorSets 里递增
static std::map<VkCommandBuffer, VkRenderPass> g_cb_prev_rp;  // src 图 finalLayout 的出处

static void copyprobe_dispatch(VkCommandBuffer, const DevDisp&, VkRenderPass, const std::vector<VkImageView>&);
static VkDescriptorSet copyprobe_clone(VkCommandBuffer, const DevDisp&, VkDescriptorSet);

// ── 描述符溯源: 回答"送显那一笔 draw 到底采的是哪张图" ──
// 纯观测, 不改任何渲染。链路是:
//   VkImage(尺寸/格式/usage) <- VkImageView <- {framebuffer 附件, 描述符里绑的采样图}
// 在送显分辨率 pass 的**第一笔 draw** 上, 把当时绑着的描述符里的采样图全列出来,
// 看有没有哪张正好是上一个(渲染分辨率)pass 的颜色附件。
struct ImgInfo { uint32_t w = 0, h = 0, fmt = 0, usage = 0;
                 uint32_t id = 0, d = 1, samples = 1, mips = 1, layers = 1; bool swap = false; };
static std::map<VkImage, ImgInfo> g_img;
static std::map<VkImageView, VkImage> g_view;
static std::map<VkFramebuffer, std::vector<VkImageView>> g_fb_att;
// copyprobe 克隆描述符集用: 需要 layout 的完整 binding 清单 (逐个 vkCopyDescriptorSet),
// 以及 image 写当时的 sampler (替换写要带上原采样器, 采样行为才不变)。
struct LayoutBinding { uint32_t binding = 0, count = 0, type = 0; };
static std::map<VkDescriptorSetLayout, std::vector<LayoutBinding>> g_layout_bindings;
static std::map<VkDescriptorSet, VkDescriptorSetLayout> g_set_layout;
static std::map<VkDescriptorPool, std::vector<VkDescriptorSet>> g_pool_sets;   // DestroyPool 时摘 set 表
static std::map<VkDescriptorSet, std::map<uint32_t, VkSampler>> g_set_samplers;  // 同 g_set_views 的 key
static std::map<VkDescriptorSet, std::map<uint32_t, VkImageView>> g_set_views;
static std::map<VkCommandBuffer, std::vector<VkDescriptorSet>> g_cb_sets;
static std::map<VkCommandBuffer, int> g_cb_draw_idx;          // 本 pass 内第几笔 draw
static std::map<VkCommandBuffer, std::vector<VkImageView>> g_cb_prev_att;  // 上一个非送显 pass 的附件
static std::string g_composite_report;                        // 只填一次
static std::map<VkCommandBuffer, bool> g_cb_cur_is_swap;

// ══ frameaudit: 一帧步骤清单 (只读) ═════════════════════════════════════════════
//
// 回答「这一帧 GPU 到底做了哪些步骤」: 按**提交顺序**列出每个 render pass (附件是哪张图的
// 哪个 mip/layer、尺寸、格式、采样数、loadOp/storeOp、draw 数、描述符里引用了哪些图) 以及
// pass 之外的 dispatch / 拷贝 / 清屏 / blit / resolve / UNDEFINED 丢弃。层只记录事实,
// 「哪一步多余」由主机侧 `phonefarm frame-audit` 离线分析。
//
// 触发: 游戏跑起来后主机 `setprop debug.knobs.frameaudit_go <新编号>`; 层每 30 帧看一眼,
// 编号变了就武装: 之后 FA_CAP_DELAY 帧开始连抓 FA_CAP_N 帧, 第 FA_READ_DELAY 帧回读时间戳并
// 落盘 knobs_frameaudit.json。用属性触发而不是固定帧号, 是因为原神从冷启到大世界要过登录页,
// 固定帧号会抓到登录页。
//
// 时间戳 (frameaudit_ts=1): 武装后 FA_TS_FRAMES 帧内**开始录制**的命令缓冲, 在每个 pass
// 开始前/结束后各写一个 BOTTOM_OF_PIPE 时间戳 (写前先在同一条 cb 里 reset 该查询, 所以
// cb 被重复提交也合法)。相邻时间戳之差 = 这一段 GPU 完成所用的时间, 各段加起来就是一帧。
// 预录很久之前的 cb 不会有时间戳, 如实报缺。

struct ViewSub { uint32_t mip = 0, nmip = 1, layer = 0, nlayer = 1; };
struct SubRes { uint32_t img = 0, mip = 0, nmip = 1, layer = 0, nlayer = 1; };
static std::atomic<uint32_t> g_img_seq{0};
static std::map<uint32_t, ImgInfo> g_img_by_id;                     // id -> 形状 (销毁即摘)
static std::map<VkImageView, ViewSub> g_view_sub;
static std::map<VkDescriptorSet, std::set<uint32_t>> g_set_storage; // 哪些 binding 是 storage image
static std::map<VkSwapchainKHR, std::vector<VkImage>> g_swap_imgs;

struct FaAtt {
    SubRes r;
    uint32_t fmt = 0, samples = 1, load = 0, store = 0, sload = 0, sstore = 0, init = 0, fin = 0;
    uint32_t role = 4;   // 0 color / 1 depth-stencil / 2 resolve / 3 只当 input / 4 没被任何 subpass 引用
    bool input = false;  // 有 subpass 把它当 input attachment 读 (同 pass 内 tile 上读, 不走显存)
};
struct FaRp { std::vector<FaAtt> att; uint32_t nsub = 1; };
static std::map<VkRenderPass, FaRp> g_rp_fa;

enum FaKind : uint8_t {
    FA_PASS = 1, FA_DISPATCH, FA_COPY_IMG, FA_BUF2IMG, FA_IMG2BUF, FA_COPY_BUF, FA_BLIT,
    FA_RESOLVE, FA_CLEAR_COLOR, FA_CLEAR_DS, FA_DISCARD, FA_FILL_BUF,
};
struct FaEv {
    uint8_t kind = 0;
    bool dyn = false, sec = false, indirect = false;
    uint64_t rp = 0; uint32_t w = 0, h = 0;
    uint64_t sig = 0;                         // pass 特征码（改写前描述），storeop 指定规则用
    std::vector<FaAtt> att;
    uint32_t draws = 0, draws_idx = 0, draws_ind = 0, clear_att = 0, clear_att_full = 0, subpasses = 1, binds = 0;
    uint64_t prims = 0;                       // 直接 draw 的 (顶点或索引数 × 实例数) 之和
    std::set<uint64_t> reads, writes;         // 描述符引用的图 (writes = storage image, 可读可写)
    SubRes src, dst; uint32_t ex[3] = {0, 0, 0}, sex[2] = {0, 0}; uint32_t filter = 0, regions = 0;
    uint64_t texels = 0, bytes = 0;
    uint32_t gx = 0, gy = 0, gz = 0;
    uint32_t barriers = 0;                    // 与上一事件之间的 barrier 调用数
    int32_t ts0 = -1, ts1 = -1;
    // pass 内开的查询: 遮挡 (OCCLUSION) 与其它类型分开数。有遮挡查询的 pass 即使不写回
    // 任何附件也有产出 (查询结果), 不能当「整个 pass 白画」。
    uint32_t q_occ = 0, q_other = 0;
};
struct FaCb {
    std::vector<FaEv> ev;
    int open = -1;
    VkDescriptorSet gfx[8] = {}, comp[8] = {};
    // push 描述符压进来的图 (view, 是否 storage): 没有 set 句柄, 直接按绑定点挂在 cb 上
    std::vector<std::pair<VkImageView, bool>> push_gfx, push_comp;
    uint32_t barriers = 0;
    bool ts = false, overflow = false;
};
static std::map<VkCommandBuffer, FaCb> g_facb;
static const size_t FA_MAX_EV = 4096;
static const uint64_t FA_CAP_DELAY = 6, FA_CAP_N = 3, FA_TS_FRAMES = 8, FA_READ_DELAY = 90;
static const uint32_t FA_TS_CAP = 16384;

struct FaState {
    long go = 0;                       // 最近一次武装的触发编号
    uint64_t arm_frame = 0;            // 0 = 没武装
    bool ts_armed = false, capturing = false;
    uint32_t cap_done = 0;
    std::vector<std::string> frames;   // 已抓完的帧 (JSON)
    std::string cur;                   // 正在抓的帧的 submits
    uint32_t cur_present = 0;
    std::map<uint32_t, ImgInfo> imgs;  // 抓到的帧里引用过的图 (抓时快照)
    VkQueryPool pool = VK_NULL_HANDLE; VkDevice pool_dev = VK_NULL_HANDLE;
    uint32_t ts_next = 0, ts_lo = 0;   // 本次武装用的时间戳槽 [ts_lo, ts_next)
    float period = 0.f;
    std::map<void*, uint32_t> qid;     // 队列 -> 小编号
    uint64_t cb_unseen = 0, ev_overflow = 0, v2_copies = 0, submit2 = 0, dyn_passes = 0, sec_merged = 0;
    uint64_t ts_skipped = 0;
    uint64_t q_outside = 0, q_copies = 0;
    uint64_t tmpl_updates = 0, push_writes = 0;   // 走模板 / push 的描述符更新次数 (诊断「读看不见」用)   // pass 外开的查询 / 查询结果拷进 buffer (GPU 侧消费, 如条件渲染)
};
static std::map<VkQueryPool, uint32_t> g_qpool_type;   // 应用建的查询池 -> VkQueryType (本层自己的时间戳池不走钩子)
static FaState g_fa;

static uint64_t fa_pack(const SubRes& r) {
    auto c = [](uint32_t v) { return (uint64_t)(v > 255 ? 255 : v); };
    return ((uint64_t)r.img << 32) | (c(r.mip) << 24) | (c(r.nmip) << 16) | (c(r.layer) << 8) | c(r.nlayer);
}
static SubRes fa_view_res(VkImageView v) {
    SubRes r;
    auto vi = g_view.find(v);
    if (vi == g_view.end()) return r;
    auto ii = g_img.find(vi->second);
    if (ii != g_img.end()) r.img = ii->second.id;
    auto si = g_view_sub.find(v);
    if (si != g_view_sub.end()) { r.mip = si->second.mip; r.nmip = si->second.nmip; r.layer = si->second.layer; r.nlayer = si->second.nlayer; }
    return r;
}
static SubRes fa_img_res(VkImage img, uint32_t mip, uint32_t nmip, uint32_t layer, uint32_t nlayer) {
    SubRes r; auto ii = g_img.find(img);
    if (ii != g_img.end()) r.img = ii->second.id;
    r.mip = mip; r.nmip = nmip; r.layer = layer; r.nlayer = nlayer;
    return r;
}
static void fa_add_set(FaEv& e, VkDescriptorSet s) {
    auto sv = g_set_views.find(s);
    if (sv == g_set_views.end()) return;
    auto st = g_set_storage.find(s);
    for (auto& bv : sv->second) {
        SubRes r = fa_view_res(bv.second);
        if (!r.img) continue;
        bool storage = st != g_set_storage.end() && st->second.count(bv.first);
        (storage ? e.writes : e.reads).insert(fa_pack(r));
    }
}
static void fa_add_view(FaEv& e, VkImageView v, bool storage) {
    SubRes r = fa_view_res(v);
    if (r.img) (storage ? e.writes : e.reads).insert(fa_pack(r));
}
// 调用方持 g_mtx。返回新事件 (满了返回 nullptr)
static FaEv* fa_push(VkCommandBuffer cb, FaEv&& e) {
    FaCb& c = g_facb[cb];
    if (c.ev.size() >= FA_MAX_EV) { if (!c.overflow) g_fa.ev_overflow++; c.overflow = true; return nullptr; }
    e.barriers = c.barriers; c.barriers = 0;
    c.ev.push_back(std::move(e));
    return &c.ev.back();
}
static FaEv* fa_open(VkCommandBuffer cb) {
    auto it = g_facb.find(cb);
    if (it == g_facb.end() || it->second.open < 0 || it->second.open >= (int)it->second.ev.size()) return nullptr;
    return &it->second.ev[it->second.open];
}
// pass 外写一个时间戳 (先 reset 该槽)。调用方持 g_mtx。
static int32_t fa_ts(VkCommandBuffer cb, const DevDisp& d) {
    auto it = g_facb.find(cb);
    if (it == g_facb.end() || !it->second.ts) return -1;
    if (g_fa.pool == VK_NULL_HANDLE || g_fa.ts_next >= FA_TS_CAP || !d.CmdResetQueryPool || !d.CmdWriteTimestamp) {
        g_fa.ts_skipped++; return -1;
    }
    uint32_t q = g_fa.ts_next++;
    d.CmdResetQueryPool(cb, g_fa.pool, q, 1);
    d.CmdWriteTimestamp(cb, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, g_fa.pool, q);
    return (int32_t)q;
}
static void fa_record_rp(VkRenderPass rp, const VkRenderPassCreateInfo* ci) {
    if (!g_frameaudit || !ci) return;
    FaRp f; f.nsub = ci->subpassCount ? ci->subpassCount : 1;
    for (uint32_t i = 0; i < ci->attachmentCount; i++) {
        const auto& a = ci->pAttachments[i];
        FaAtt x; x.fmt = a.format; x.samples = a.samples; x.load = a.loadOp; x.store = a.storeOp;
        x.sload = a.stencilLoadOp; x.sstore = a.stencilStoreOp; x.init = a.initialLayout; x.fin = a.finalLayout;
        f.att.push_back(x);
    }
    auto mark = [&](uint32_t idx, uint32_t role) {
        if (idx == VK_ATTACHMENT_UNUSED || idx >= f.att.size()) return;
        if (role == 3) { f.att[idx].input = true; if (f.att[idx].role == 4) f.att[idx].role = 3; return; }
        if (f.att[idx].role == 4 || f.att[idx].role == 3) f.att[idx].role = role;
    };
    for (uint32_t s = 0; s < ci->subpassCount; s++) {
        const auto& sp = ci->pSubpasses[s];
        for (uint32_t j = 0; j < sp.colorAttachmentCount; j++) {
            mark(sp.pColorAttachments[j].attachment, 0);
            if (sp.pResolveAttachments) mark(sp.pResolveAttachments[j].attachment, 2);
        }
        if (sp.pDepthStencilAttachment) mark(sp.pDepthStencilAttachment->attachment, 1);
        for (uint32_t j = 0; j < sp.inputAttachmentCount; j++) mark(sp.pInputAttachments[j].attachment, 3);
    }
    std::lock_guard<std::mutex> lk(g_mtx);
    g_rp_fa[rp] = std::move(f);
}
static void fa_record_rp2(VkRenderPass rp, const VkRenderPassCreateInfo2* ci) {
    if (!g_frameaudit || !ci) return;
    FaRp f; f.nsub = ci->subpassCount ? ci->subpassCount : 1;
    for (uint32_t i = 0; i < ci->attachmentCount; i++) {
        const auto& a = ci->pAttachments[i];
        FaAtt x; x.fmt = a.format; x.samples = a.samples; x.load = a.loadOp; x.store = a.storeOp;
        x.sload = a.stencilLoadOp; x.sstore = a.stencilStoreOp; x.init = a.initialLayout; x.fin = a.finalLayout;
        f.att.push_back(x);
    }
    auto mark = [&](uint32_t idx, uint32_t role) {
        if (idx == VK_ATTACHMENT_UNUSED || idx >= f.att.size()) return;
        if (role == 3) { f.att[idx].input = true; if (f.att[idx].role == 4) f.att[idx].role = 3; return; }
        if (f.att[idx].role == 4 || f.att[idx].role == 3) f.att[idx].role = role;
    };
    for (uint32_t s = 0; s < ci->subpassCount; s++) {
        const auto& sp = ci->pSubpasses[s];
        for (uint32_t j = 0; j < sp.colorAttachmentCount; j++) {
            mark(sp.pColorAttachments[j].attachment, 0);
            if (sp.pResolveAttachments) mark(sp.pResolveAttachments[j].attachment, 2);
        }
        if (sp.pDepthStencilAttachment) mark(sp.pDepthStencilAttachment->attachment, 1);
        for (uint32_t j = 0; j < sp.inputAttachmentCount; j++) mark(sp.pInputAttachments[j].attachment, 3);
    }
    std::lock_guard<std::mutex> lk(g_mtx);
    g_rp_fa[rp] = std::move(f);
}

static uint64_t pass_key(VkRenderPass rp, uint32_t w, uint32_t h) {
    uint64_t seq = 0;
    auto it = g_rp_seq.find(rp);
    if (it != g_rp_seq.end()) seq = it->second;
    return (seq << 34) | ((uint64_t)(w & 0xFFFF) << 17) | (h & 0x1FFFF);
}

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
            char one[200];
            snprintf(one, sizeof one,
                     "%s{\"pass\":%llu,\"attachment\":%u,\"field\":\"%s\","
                     "\"from\":\"%s\",\"to\":\"%s\",\"begins\":%llu}",
                     eff.empty() ? "" : ",", (unsigned long long)e.pass, e.attachment,
                     e.field, e.from, e.to, (unsigned long long)e.begins);
            eff += one;
            if (e.begins) used = true;
        }
    }
    // harness 据此判断该轮算不算数。两种"没生效"要分开报:
    //   没找到 LOAD 可改 vs 改了但应用根本没用那个 pass 对象
    const char* unavail = "null";
    const bool rewriting = g_rewrite_loadop || g_storeop_mode;
    if (rewriting && eff.empty())
        unavail = g_storeop_mode ? "\"no matching STORE attachment seen\"" : "\"no LOAD attachment seen\"";
    else if (rewriting && !used)              unavail = "\"rewritten pass never bound\"";
    std::string knob = g_rewrite_loadop ? "gray_loadop_dontcare" : "";
    if (g_storeop_mode) knob += std::string(knob.empty() ? "" : "+") + (g_storeop_mode == 1 ? "gray_storeop_ds" : "gray_storeop_pick");
    if (knob.empty()) knob = "gray_readonly_probe";

    std::string json =
        std::string("{\"knob\":\"") + knob
        + "\",\"layer_loaded\":true,\"pkg\":\"" + pkg
        + "\",\"effective\":[" + eff + "],\"failed\":[],\"unavailable_reason\":" + unavail
        + ",\"readonly_stats\":{\"frames\":" + std::to_string(g_frames.load())
        + ",\"render_pass_begins\":" + std::to_string(g_rp.load())
        + ",\"render_passes_created\":" + std::to_string(g_rp_created.load()) + "}";

    if (g_dump_passes) {
        std::string tbl, seq, swap, comp;
        {
            std::lock_guard<std::mutex> lk(g_mtx);
            for (auto& kv : g_pass_stat) {
                const PassStat& p = kv.second;
                // 只留"被执行过"的: frames_seen 按提交数, 预录命令缓冲也不会被误滤 (评审 #4)
                if (p.frames_seen < 30 && p.begins < 30) continue;
                char one[256];
                snprintf(one, sizeof one,
                         "%s{\"w\":%u,\"h\":%u,\"att\":%u,\"fmt0\":%u,\"begins\":%llu,\"frames\":%llu,\"draws\":%llu}",
                         tbl.empty() ? "" : ",", p.w, p.h, p.n_att,
                         p.fmt.empty() ? 0u : p.fmt[0],
                         (unsigned long long)p.begins, (unsigned long long)p.frames_seen,
                         (unsigned long long)p.draws);
                tbl += one;
            }
            for (auto& e : g_frame_seq) { if (!seq.empty()) seq += ","; seq += e; }
            // g_swap_* 与 g_composite_report 也在锁内读 (评审 #11: CreateSwapchainKHR 在别的线程写)
            swap = ",\"swapchain\":{\"w\":" + std::to_string(g_swap_w)
                 + ",\"h\":" + std::to_string(g_swap_h)
                 + ",\"fmt\":" + std::to_string(g_swap_fmt) + "}";
            comp = g_composite_report;
        }
        json += swap;
        json += ",\"composite_draw\":" + (comp.empty() ? std::string("null") : comp);
        json += ",\"pass_table\":[" + tbl + "]";
        // frame_seq 可能还没采 (采样窗在稳态后的某一帧): 显式标出来,
        // 免得"空序列"和"还没采"在 harness 眼里长得一样 (评审 #5 的根因)
        json += std::string(",\"frame_seq_ready\":") + (g_seq_done ? "true" : "false");
        json += ",\"frame_seq\":[" + seq + "]";
        if (g_copyprobe || g_upop) {
            std::lock_guard<std::mutex> lk(g_mtx);
            char b[256];
            snprintf(b, sizeof b, ",\"copyprobe\":{\"mode\":\"%s\",\"copies\":%llu,\"subs\":%llu,\"dst\":[%u,%u,%u],\"disabled\":%s}",
                     g_upop ? "upop" : "copy",
                     (unsigned long long)g_cp.copies, (unsigned long long)g_cp.subs,
                     g_cp.w, g_cp.h, g_cp.fmt, g_cp.disabled ? "true" : "false");
            json += b;
        }
    }
    json += "}";

    // 通道 1: logcat (root 可读, 不受目标应用存储沙箱影响)。
    // 但 logcat 单条上限约 4KB, passdump 模式下光 frame_seq 就有约 20KB, 必被截断 (评审 #6):
    // passdump 模式 logcat 只打摘要, 全文只走文件通道。
    if (g_dump_passes) {
        RK_LOG("marker 已写文件通道 (passdump 全文 %zu 字节超出 logcat 单条上限, 此处只打摘要): "
               "frames=%llu rp_begins=%llu", json.size(),
               (unsigned long long)g_frames.load(), (unsigned long long)g_rp.load());
    } else {
        RK_LOG("%s", json.c_str());
    }

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

// 记一个 render pass 的形状 (附件数 / 格式 / loadOp / finalLayout) 与它的创建序号
template <typename FmtFn, typename LoadFn, typename FinFn>
static void record_rp(VkRenderPass rp, uint64_t seq, uint32_t n, FmtFn fmt, LoadFn load, FinFn fin) {
    RpInfo ri; ri.n_att = n;
    for (uint32_t i = 0; i < n; i++) {
        ri.fmt.push_back(fmt(i)); ri.loadop.push_back(load(i));
        ri.final_layout.push_back(fin(i));
    }
    std::lock_guard<std::mutex> lk(g_mtx);
    g_rp_info[rp] = std::move(ri);
    g_rp_seq[rp] = seq;
}

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
    {
        char v[PROP_VALUE_MAX] = {0};
        __system_property_get("debug.knobs.storeop", v);
        parse_storeop_prop(v);
        if (g_storeop_mode) RK_LOG("storeop 改写开: mode=%d 指定 %zu 条", g_storeop_mode, g_storeop_pick.size());
        g_storeop_live = g_storeop_mode && read_bool_prop("debug.knobs.storeop_live");
        g_storeop_on = g_storeop_live && read_bool_prop("debug.knobs.storeop_on");
        if (g_storeop_live) RK_LOG("storeop 现场切换模式: 当前 %s", g_storeop_on.load() ? "开" : "关");
    }
    g_dump_passes = read_bool_prop("debug.knobs.passdump");
    g_copyprobe = read_bool_prop("debug.knobs.copyprobe");
    g_upop = read_bool_prop("debug.knobs.upop");
    g_frameaudit = read_bool_prop("debug.knobs.frameaudit");
    g_frameaudit_ts = g_frameaudit && read_bool_prop("debug.knobs.frameaudit_ts");
    if (g_copyprobe || g_upop || g_frameaudit) g_dump_passes = true;   // 探针/算子/步骤清单全部建立在溯源表上
    RK_LOG("rk_CreateInstance entered (pkg=%s) rewrite_loadop=%d passdump=%d copyprobe=%d upop=%d frameaudit=%d ts=%d",
           self_pkg().c_str(), (int)g_rewrite_loadop, (int)g_dump_passes, (int)g_copyprobe, (int)g_upop,
           (int)g_frameaudit, (int)g_frameaudit_ts);
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
    d.CreateFramebuffer = (PFN_vkCreateFramebuffer)gdpa(*pDev, "vkCreateFramebuffer");
    d.CreateSwapchainKHR = (PFN_vkCreateSwapchainKHR)gdpa(*pDev, "vkCreateSwapchainKHR");
    d.CmdDraw = (PFN_vkCmdDraw)gdpa(*pDev, "vkCmdDraw");
    d.CmdDrawIndexed = (PFN_vkCmdDrawIndexed)gdpa(*pDev, "vkCmdDrawIndexed");
    d.CmdBlitImage = (PFN_vkCmdBlitImage)gdpa(*pDev, "vkCmdBlitImage");
    d.CreateImage = (PFN_vkCreateImage)gdpa(*pDev, "vkCreateImage");
    d.CreateImageView = (PFN_vkCreateImageView)gdpa(*pDev, "vkCreateImageView");
    d.UpdateDescriptorSets = (PFN_vkUpdateDescriptorSets)gdpa(*pDev, "vkUpdateDescriptorSets");
    d.CmdBindDescriptorSets = (PFN_vkCmdBindDescriptorSets)gdpa(*pDev, "vkCmdBindDescriptorSets");
    d.BeginCommandBuffer = (PFN_vkBeginCommandBuffer)gdpa(*pDev, "vkBeginCommandBuffer");
    d.CmdExecuteCommands = (PFN_vkCmdExecuteCommands)gdpa(*pDev, "vkCmdExecuteCommands");
    d.QueueSubmit = (PFN_vkQueueSubmit)gdpa(*pDev, "vkQueueSubmit");
    d.FreeCommandBuffers = (PFN_vkFreeCommandBuffers)gdpa(*pDev, "vkFreeCommandBuffers");
    d.DestroyFramebuffer = (PFN_vkDestroyFramebuffer)gdpa(*pDev, "vkDestroyFramebuffer");
    d.DestroyImage = (PFN_vkDestroyImage)gdpa(*pDev, "vkDestroyImage");
    d.DestroyImageView = (PFN_vkDestroyImageView)gdpa(*pDev, "vkDestroyImageView");
    d.dev = *pDev;
    d.phys = phys;
    d.CreateShaderModule = (PFN_vkCreateShaderModule)gdpa(*pDev, "vkCreateShaderModule");
    d.DestroyShaderModule = (PFN_vkDestroyShaderModule)gdpa(*pDev, "vkDestroyShaderModule");
    d.CreatePipelineLayout = (PFN_vkCreatePipelineLayout)gdpa(*pDev, "vkCreatePipelineLayout");
    d.DestroyPipelineLayout = (PFN_vkDestroyPipelineLayout)gdpa(*pDev, "vkDestroyPipelineLayout");
    d.CreateComputePipelines = (PFN_vkCreateComputePipelines)gdpa(*pDev, "vkCreateComputePipelines");
    d.DestroyPipeline = (PFN_vkDestroyPipeline)gdpa(*pDev, "vkDestroyPipeline");
    d.CreateDescriptorSetLayout = (PFN_vkCreateDescriptorSetLayout)gdpa(*pDev, "vkCreateDescriptorSetLayout");
    d.DestroyDescriptorSetLayout = (PFN_vkDestroyDescriptorSetLayout)gdpa(*pDev, "vkDestroyDescriptorSetLayout");
    d.CreateDescriptorPool = (PFN_vkCreateDescriptorPool)gdpa(*pDev, "vkCreateDescriptorPool");
    d.DestroyDescriptorPool = (PFN_vkDestroyDescriptorPool)gdpa(*pDev, "vkDestroyDescriptorPool");
    d.AllocateDescriptorSets = (PFN_vkAllocateDescriptorSets)gdpa(*pDev, "vkAllocateDescriptorSets");
    d.FreeDescriptorSets = (PFN_vkFreeDescriptorSets)gdpa(*pDev, "vkFreeDescriptorSets");
    d.AllocateMemory = (PFN_vkAllocateMemory)gdpa(*pDev, "vkAllocateMemory");
    d.FreeMemory = (PFN_vkFreeMemory)gdpa(*pDev, "vkFreeMemory");
    d.BindImageMemory = (PFN_vkBindImageMemory)gdpa(*pDev, "vkBindImageMemory");
    d.CmdPipelineBarrier = (PFN_vkCmdPipelineBarrier)gdpa(*pDev, "vkCmdPipelineBarrier");
    d.CmdBindPipeline = (PFN_vkCmdBindPipeline)gdpa(*pDev, "vkCmdBindPipeline");
    d.CmdDispatch = (PFN_vkCmdDispatch)gdpa(*pDev, "vkCmdDispatch");
    if (inst) d.GetPhysMemProps = (PFN_vkGetPhysicalDeviceMemoryProperties)gipa(inst, "vkGetPhysicalDeviceMemoryProperties");
#define RK_GDPA(field, T, name) d.field = (T)gdpa(*pDev, name)
    RK_GDPA(CmdEndRenderPass, PFN_vkCmdEndRenderPass, "vkCmdEndRenderPass");
    RK_GDPA(CmdEndRenderPass2, PFN_vkCmdEndRenderPass2, "vkCmdEndRenderPass2");
    if (!d.CmdEndRenderPass2) RK_GDPA(CmdEndRenderPass2, PFN_vkCmdEndRenderPass2, "vkCmdEndRenderPass2KHR");
    RK_GDPA(CmdNextSubpass, PFN_vkCmdNextSubpass, "vkCmdNextSubpass");
    RK_GDPA(CmdNextSubpass2, PFN_vkCmdNextSubpass2, "vkCmdNextSubpass2");
    if (!d.CmdNextSubpass2) RK_GDPA(CmdNextSubpass2, PFN_vkCmdNextSubpass2, "vkCmdNextSubpass2KHR");
    RK_GDPA(CmdBeginRendering, PFN_vkCmdBeginRendering, "vkCmdBeginRendering");
    if (!d.CmdBeginRendering) RK_GDPA(CmdBeginRendering, PFN_vkCmdBeginRendering, "vkCmdBeginRenderingKHR");
    RK_GDPA(CmdEndRendering, PFN_vkCmdEndRendering, "vkCmdEndRendering");
    if (!d.CmdEndRendering) RK_GDPA(CmdEndRendering, PFN_vkCmdEndRendering, "vkCmdEndRenderingKHR");
    RK_GDPA(CmdDrawIndirect, PFN_vkCmdDrawIndirect, "vkCmdDrawIndirect");
    RK_GDPA(CmdDrawIndexedIndirect, PFN_vkCmdDrawIndexedIndirect, "vkCmdDrawIndexedIndirect");
    RK_GDPA(CmdDispatchIndirect, PFN_vkCmdDispatchIndirect, "vkCmdDispatchIndirect");
    RK_GDPA(CmdCopyImage, PFN_vkCmdCopyImage, "vkCmdCopyImage");
    RK_GDPA(CmdCopyBuffer, PFN_vkCmdCopyBuffer, "vkCmdCopyBuffer");
    RK_GDPA(CmdCopyBufferToImage, PFN_vkCmdCopyBufferToImage, "vkCmdCopyBufferToImage");
    RK_GDPA(CmdCopyImageToBuffer, PFN_vkCmdCopyImageToBuffer, "vkCmdCopyImageToBuffer");
    RK_GDPA(CmdResolveImage, PFN_vkCmdResolveImage, "vkCmdResolveImage");
    RK_GDPA(CmdClearColorImage, PFN_vkCmdClearColorImage, "vkCmdClearColorImage");
    RK_GDPA(CmdClearDepthStencilImage, PFN_vkCmdClearDepthStencilImage, "vkCmdClearDepthStencilImage");
    RK_GDPA(CmdClearAttachments, PFN_vkCmdClearAttachments, "vkCmdClearAttachments");
    RK_GDPA(CmdFillBuffer, PFN_vkCmdFillBuffer, "vkCmdFillBuffer");
    RK_GDPA(CmdUpdateBuffer, PFN_vkCmdUpdateBuffer, "vkCmdUpdateBuffer");
    RK_GDPA(CmdPipelineBarrier2, PFN_vkCmdPipelineBarrier2, "vkCmdPipelineBarrier2");
    if (!d.CmdPipelineBarrier2) RK_GDPA(CmdPipelineBarrier2, PFN_vkCmdPipelineBarrier2, "vkCmdPipelineBarrier2KHR");
    RK_GDPA(QueueSubmit2, PFN_vkQueueSubmit2, "vkQueueSubmit2");
    if (!d.QueueSubmit2) RK_GDPA(QueueSubmit2, PFN_vkQueueSubmit2, "vkQueueSubmit2KHR");
    RK_GDPA(GetSwapchainImagesKHR, PFN_vkGetSwapchainImagesKHR, "vkGetSwapchainImagesKHR");
    RK_GDPA(CreateQueryPool, PFN_vkCreateQueryPool, "vkCreateQueryPool");
    RK_GDPA(DestroyQueryPool, PFN_vkDestroyQueryPool, "vkDestroyQueryPool");
    RK_GDPA(CmdResetQueryPool, PFN_vkCmdResetQueryPool, "vkCmdResetQueryPool");
    RK_GDPA(CmdWriteTimestamp, PFN_vkCmdWriteTimestamp, "vkCmdWriteTimestamp");
    RK_GDPA(GetQueryPoolResults, PFN_vkGetQueryPoolResults, "vkGetQueryPoolResults");
    RK_GDPA(CmdBeginQuery, PFN_vkCmdBeginQuery, "vkCmdBeginQuery");
    RK_GDPA(CmdEndQuery, PFN_vkCmdEndQuery, "vkCmdEndQuery");
    RK_GDPA(CmdCopyQueryPoolResults, PFN_vkCmdCopyQueryPoolResults, "vkCmdCopyQueryPoolResults");
    RK_GDPA(CreateDescriptorUpdateTemplate, PFN_vkCreateDescriptorUpdateTemplate, "vkCreateDescriptorUpdateTemplate");
    if (!d.CreateDescriptorUpdateTemplate) RK_GDPA(CreateDescriptorUpdateTemplate, PFN_vkCreateDescriptorUpdateTemplate, "vkCreateDescriptorUpdateTemplateKHR");
    RK_GDPA(DestroyDescriptorUpdateTemplate, PFN_vkDestroyDescriptorUpdateTemplate, "vkDestroyDescriptorUpdateTemplate");
    if (!d.DestroyDescriptorUpdateTemplate) RK_GDPA(DestroyDescriptorUpdateTemplate, PFN_vkDestroyDescriptorUpdateTemplate, "vkDestroyDescriptorUpdateTemplateKHR");
    RK_GDPA(UpdateDescriptorSetWithTemplate, PFN_vkUpdateDescriptorSetWithTemplate, "vkUpdateDescriptorSetWithTemplate");
    if (!d.UpdateDescriptorSetWithTemplate) RK_GDPA(UpdateDescriptorSetWithTemplate, PFN_vkUpdateDescriptorSetWithTemplate, "vkUpdateDescriptorSetWithTemplateKHR");
    RK_GDPA(CmdPushDescriptorSetKHR, PFN_vkCmdPushDescriptorSetKHR, "vkCmdPushDescriptorSetKHR");
    RK_GDPA(CmdPushDescriptorSetWithTemplateKHR, PFN_vkCmdPushDescriptorSetWithTemplateKHR, "vkCmdPushDescriptorSetWithTemplateKHR");
#undef RK_GDPA
    if (inst) {
        auto gpp = (PFN_vkGetPhysicalDeviceProperties)gipa(inst, "vkGetPhysicalDeviceProperties");
        if (gpp) { VkPhysicalDeviceProperties pp{}; gpp(phys, &pp); d.ts_period_ns = pp.limits.timestampPeriod; }
    }
    { std::lock_guard<std::mutex> lk(g_mtx); g_dev[key(*pDev)] = d; }
    return r;
}

static VKAPI_ATTR void VKAPI_CALL rk_DestroyDevice(VkDevice dev, const VkAllocationCallbacks* a) {
    DevDisp d;
    { std::lock_guard<std::mutex> lk(g_mtx); auto it = g_dev.find(key(dev)); if (it == g_dev.end()) return; d = it->second; g_dev.erase(it); }
    write_marker();  // 收尾再落一次最终计数
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        if (g_fa.pool != VK_NULL_HANDLE && g_fa.pool_dev == dev && d.DestroyQueryPool) {
            d.DestroyQueryPool(dev, g_fa.pool, nullptr); g_fa.pool = VK_NULL_HANDLE;
        }
    }
    if (g_cp.dev == dev) {   // copyprobe 自建对象随 device 一起收
        if (g_cp.dst) { d.DestroyImageView(dev, g_cp.dst_view, nullptr); d.DestroyImage(dev, g_cp.dst, nullptr); d.FreeMemory(dev, g_cp.mem, nullptr); }
        if (g_cp.pipe) { d.DestroyPipeline(dev, g_cp.pipe, nullptr); d.DestroyPipelineLayout(dev, g_cp.pl, nullptr);
                         d.DestroyDescriptorPool(dev, g_cp.pool, nullptr); d.DestroyDescriptorSetLayout(dev, g_cp.dsl, nullptr);
                         d.DestroyShaderModule(dev, g_cp.sm, nullptr); }
        g_cp = CopyProbe{};
    }
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
// 现场切换: 登记孪生 pass。effective 记在孪生句柄上 —— 只有开关开着、真换上了孪生, begins 才涨。
static void register_twin(VkRenderPass orig, VkRenderPass twin, uint64_t sig, const std::vector<Effective>& hits) {
    std::lock_guard<std::mutex> lk(g_mtx);
    g_twin[orig] = twin;
    g_rp_sig[twin] = sig;
    auto& idxs = g_rewritten[twin];
    for (auto& h : hits) {
        if (g_effective.size() >= RK_MAX_EFFECTIVE) { g_effective_dropped++; continue; }
        idxs.push_back(g_effective.size());
        g_effective.push_back(h);
    }
}
// 开关开着就把 begin info 里的 pass 换成孪生的 (sub 由调用方提供存储)
static const VkRenderPassBeginInfo* twin_begin(const VkRenderPassBeginInfo* bi, VkRenderPassBeginInfo* sub) {
    if (!g_storeop_live || !bi || !g_storeop_on.load()) return bi;
    std::lock_guard<std::mutex> lk(g_mtx);
    auto it = g_twin.find(bi->renderPass);
    if (it == g_twin.end()) return bi;
    *sub = *bi;
    sub->renderPass = it->second;
    return sub;
}

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
    const uint64_t sig = ci ? rp_sig(ci->attachmentCount, ci->pAttachments, ci->subpassCount) : 0;
    struct SigRec { VkRenderPass* o; uint64_t s; ~SigRec() { if (o && *o) { std::lock_guard<std::mutex> lk(g_mtx); g_rp_sig[*o] = s; } } } sig_rec{out, sig};
    if ((!g_rewrite_loadop && !g_storeop_mode) || !ci || ci->attachmentCount == 0) {
        VkResult r0 = d.CreateRenderPass(dev, ci, a, out);
        if (r0 == VK_SUCCESS) fa_record_rp(*out, ci);
        if (r0 == VK_SUCCESS && g_dump_passes && ci) record_rp(*out, idx, ci->attachmentCount,
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].format; },
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].loadOp; },
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].finalLayout; });
        return r0;
    }

    // pAttachments 是 const, 必须整份拷出来再改
    std::vector<VkAttachmentDescription> atts(ci->pAttachments, ci->pAttachments + ci->attachmentCount);
    std::vector<Effective> hits;
    for (uint32_t i = 0; g_rewrite_loadop && i < atts.size(); i++) {
        if (atts[i].loadOp != VK_ATTACHMENT_LOAD_OP_LOAD) continue;
        atts[i].loadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE;
        hits.push_back({idx, i, "loadOp", "LOAD", "DONT_CARE", 0});
    }
    apply_storeop(ci, atts.data(), idx, sig, hits);
    if (hits.empty()) {
        VkResult r0 = d.CreateRenderPass(dev, ci, a, out);
        if (r0 == VK_SUCCESS) fa_record_rp(*out, ci);
        if (r0 == VK_SUCCESS && g_dump_passes) record_rp(*out, idx, ci->attachmentCount,
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].format; },
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].loadOp; },
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].finalLayout; });
        return r0;
    }

    VkRenderPassCreateInfo mod = *ci;
    mod.pAttachments = atts.data();
    if (g_storeop_live && !g_rewrite_loadop) {
        // 应用拿原样的; 孪生的另建, 开关开时在 BeginRenderPass 里替换
        VkResult r0 = d.CreateRenderPass(dev, ci, a, out);
        if (r0 != VK_SUCCESS) return r0;
        fa_record_rp(*out, ci);
        if (g_dump_passes) record_rp(*out, idx, ci->attachmentCount,
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].format; },
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].loadOp; },
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].finalLayout; });
        VkRenderPass twin = VK_NULL_HANDLE;
        VkResult r1 = d.CreateRenderPass(dev, &mod, a, &twin);
        if (r1 != VK_SUCCESS) { RK_LOG("孪生 pass 建不出 (%d), pass=%llu 这条开关作废", (int)r1, (unsigned long long)idx); return r0; }
        fa_record_rp(twin, &mod);
        if (g_dump_passes) record_rp(twin, idx, ci->attachmentCount,
            [&](uint32_t i){ return (uint32_t)atts[i].format; },
            [&](uint32_t i){ return (uint32_t)atts[i].loadOp; },
            [&](uint32_t i){ return (uint32_t)atts[i].finalLayout; });
        register_twin(*out, twin, sig, hits);
        return r0;
    }
    VkResult r = d.CreateRenderPass(dev, &mod, a, out);
    if (r != VK_SUCCESS) {   // 改写导致创建失败就如实报, 不偷偷回退成原样
        RK_LOG("CreateRenderPass(改写后) 失败: %d, pass=%llu", (int)r, (unsigned long long)idx);
        return r;
    }
    fa_record_rp(*out, &mod);
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto& idxs = g_rewritten[*out];
        for (auto& h : hits) {
            if (g_effective.size() >= RK_MAX_EFFECTIVE) { g_effective_dropped++; continue; }
            idxs.push_back(g_effective.size());
            g_effective.push_back(h);
        }
    }
    if (g_dump_passes) record_rp(*out, idx, ci->attachmentCount,
        [&](uint32_t i){ return (uint32_t)atts[i].format; },
        [&](uint32_t i){ return (uint32_t)atts[i].loadOp; },
        [&](uint32_t i){ return (uint32_t)atts[i].finalLayout; });
    RK_LOG("改写 pass=%llu: %zu 处 load/store -> DONT_CARE",
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
    const uint64_t sig = ci ? rp_sig(ci->attachmentCount, ci->pAttachments, ci->subpassCount) : 0;
    struct SigRec { VkRenderPass* o; uint64_t s; ~SigRec() { if (o && *o) { std::lock_guard<std::mutex> lk(g_mtx); g_rp_sig[*o] = s; } } } sig_rec{out, sig};
    if ((!g_rewrite_loadop && !g_storeop_mode) || !ci || ci->attachmentCount == 0) {
        VkResult r0 = d.CreateRenderPass2(dev, ci, a, out);
        if (r0 == VK_SUCCESS) fa_record_rp2(*out, ci);
        if (r0 == VK_SUCCESS && g_dump_passes && ci) record_rp(*out, idx, ci->attachmentCount,
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].format; },
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].loadOp; },
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].finalLayout; });
        return r0;
    }

    std::vector<VkAttachmentDescription2> atts(ci->pAttachments, ci->pAttachments + ci->attachmentCount);
    std::vector<Effective> hits;
    for (uint32_t i = 0; g_rewrite_loadop && i < atts.size(); i++) {
        if (atts[i].loadOp != VK_ATTACHMENT_LOAD_OP_LOAD) continue;
        atts[i].loadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE;
        hits.push_back({idx, i, "loadOp", "LOAD", "DONT_CARE", 0});
    }
    apply_storeop(ci, atts.data(), idx, sig, hits);
    if (hits.empty()) {
        VkResult r0 = d.CreateRenderPass2(dev, ci, a, out);
        if (r0 == VK_SUCCESS) fa_record_rp2(*out, ci);
        if (r0 == VK_SUCCESS && g_dump_passes) record_rp(*out, idx, ci->attachmentCount,
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].format; },
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].loadOp; },
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].finalLayout; });
        return r0;
    }

    VkRenderPassCreateInfo2 mod = *ci;
    mod.pAttachments = atts.data();
    if (g_storeop_live && !g_rewrite_loadop) {
        VkResult r0 = d.CreateRenderPass2(dev, ci, a, out);
        if (r0 != VK_SUCCESS) return r0;
        fa_record_rp2(*out, ci);
        if (g_dump_passes) record_rp(*out, idx, ci->attachmentCount,
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].format; },
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].loadOp; },
            [&](uint32_t i){ return (uint32_t)ci->pAttachments[i].finalLayout; });
        VkRenderPass twin = VK_NULL_HANDLE;
        VkResult r1 = d.CreateRenderPass2(dev, &mod, a, &twin);
        if (r1 != VK_SUCCESS) { RK_LOG("孪生 pass2 建不出 (%d), pass=%llu 这条开关作废", (int)r1, (unsigned long long)idx); return r0; }
        fa_record_rp2(twin, &mod);
        if (g_dump_passes) record_rp(twin, idx, ci->attachmentCount,
            [&](uint32_t i){ return (uint32_t)atts[i].format; },
            [&](uint32_t i){ return (uint32_t)atts[i].loadOp; },
            [&](uint32_t i){ return (uint32_t)atts[i].finalLayout; });
        register_twin(*out, twin, sig, hits);
        return r0;
    }
    VkResult r = d.CreateRenderPass2(dev, &mod, a, out);
    if (r != VK_SUCCESS) {
        RK_LOG("CreateRenderPass2(改写后) 失败: %d, pass=%llu", (int)r, (unsigned long long)idx);
        return r;
    }
    fa_record_rp2(*out, &mod);
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto& idxs = g_rewritten[*out];
        for (auto& h : hits) {
            if (g_effective.size() >= RK_MAX_EFFECTIVE) { g_effective_dropped++; continue; }
            idxs.push_back(g_effective.size());
            g_effective.push_back(h);
        }
    }
    if (g_dump_passes) record_rp(*out, idx, ci->attachmentCount,
        [&](uint32_t i){ return (uint32_t)atts[i].format; },
        [&](uint32_t i){ return (uint32_t)atts[i].loadOp; },
        [&](uint32_t i){ return (uint32_t)atts[i].finalLayout; });
    RK_LOG("改写2 pass=%llu: %zu 处 load/store -> DONT_CARE",
           (unsigned long long)idx, hits.size());
    return r;
}

// 记一次"被改写的 render pass 真的被用来开 pass 了"。
// 没有这个计数, effective 只能说明"我改过这个对象", 说不了"应用真的用了它" ——
// 实测 refbench 会把 LOAD 和 DONT_CARE 两个 pass 对象都建出来、只绑其中一个,
// 于是不加区分的 effective 会把一次空改写报成生效。
static void note_begin(VkRenderPass rp) {
    // 只读档没有任何改写, 这里不该占 CmdBeginRenderPass 的热路径去抢全局锁
    // storeop 也是改写 (#91 漏了这一条: 只开 storeop 时 begins 恒为 0, 「真被用上」无从判断)
    if ((!g_rewrite_loadop && !g_storeop_mode) || rp == VK_NULL_HANDLE) return;
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
    VkRenderPass twin = VK_NULL_HANDLE;
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        g_rewritten.erase(rp);
        // 句柄会复用: 这三张表不一起擦, 新建的 pass 拿到旧句柄值就继承死者的 seq/形状 (评审 #8)
        g_rp_seq.erase(rp);
        g_rp_info.erase(rp);
        g_rp_fa.erase(rp);
        g_rp_sig.erase(rp);
        // 孪生跟原样同生共死: 规范保证应用销毁原样 pass 时已没有在飞的命令用它, 换上的孪生同理
        auto tw = g_twin.find(rp);
        if (tw != g_twin.end()) {
            twin = tw->second;
            g_twin.erase(tw);
            g_rewritten.erase(twin); g_rp_seq.erase(twin); g_rp_info.erase(twin); g_rp_fa.erase(twin); g_rp_sig.erase(twin);
        }
    }
    if (twin != VK_NULL_HANDLE) d.DestroyRenderPass(dev, twin, a);
    d.DestroyRenderPass(dev, rp, a);
}

// ── 溯源用的四个只读钩子 ──
static VKAPI_ATTR VkResult VKAPI_CALL rk_CreateImage(
    VkDevice dev, const VkImageCreateInfo* ci, const VkAllocationCallbacks* a, VkImage* out) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.CreateImage) { RK_LOG_ONCE("CreateImage: 无下层"); return VK_ERROR_INITIALIZATION_FAILED; }
    VkResult r = d.CreateImage(dev, ci, a, out);
    if (r == VK_SUCCESS && ci && g_dump_passes) {
        std::lock_guard<std::mutex> lk(g_mtx);
        ImgInfo ii{ci->extent.width, ci->extent.height, (uint32_t)ci->format, (uint32_t)ci->usage};
        ii.id = ++g_img_seq; ii.d = ci->extent.depth; ii.samples = (uint32_t)ci->samples;
        ii.mips = ci->mipLevels; ii.layers = ci->arrayLayers;
        g_img[*out] = ii;
        if (g_frameaudit) g_img_by_id[ii.id] = ii;
    }
    return r;
}
static VKAPI_ATTR VkResult VKAPI_CALL rk_CreateImageView(
    VkDevice dev, const VkImageViewCreateInfo* ci, const VkAllocationCallbacks* a, VkImageView* out) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.CreateImageView) { RK_LOG_ONCE("CreateImageView: 无下层"); return VK_ERROR_INITIALIZATION_FAILED; }
    VkResult r = d.CreateImageView(dev, ci, a, out);
    if (r == VK_SUCCESS && ci && g_dump_passes) {
        std::lock_guard<std::mutex> lk(g_mtx);
        g_view[*out] = ci->image;
        if (g_frameaudit) g_view_sub[*out] = ViewSub{ci->subresourceRange.baseMipLevel, ci->subresourceRange.levelCount,
                                                   ci->subresourceRange.baseArrayLayer, ci->subresourceRange.layerCount};
    }
    return r;
}
static bool is_image_desc(uint32_t t) {
    return t == VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER || t == VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE ||
           t == VK_DESCRIPTOR_TYPE_STORAGE_IMAGE || t == VK_DESCRIPTOR_TYPE_INPUT_ATTACHMENT;
}
// 记一条 image 描述符写 (调用方持 g_mtx)
static void note_image_desc(VkDescriptorSet set, uint32_t binding, uint32_t elem, uint32_t type,
                            VkImageView view, VkSampler samp) {
    if (view == VK_NULL_HANDLE) return;
    uint32_t kk = elem + (binding << 8);
    g_set_views[set][kk] = view;
    g_set_samplers[set][kk] = samp;
    if (g_frameaudit) {
        auto& st = g_set_storage[set];
        if (type == VK_DESCRIPTOR_TYPE_STORAGE_IMAGE) st.insert(kk); else st.erase(kk);
    }
}
// 描述符里绑了哪些采样图 —— 只记 image 类的 binding, buffer 一概不碰
static VKAPI_ATTR void VKAPI_CALL rk_UpdateDescriptorSets(
    VkDevice dev, uint32_t nw, const VkWriteDescriptorSet* w, uint32_t nc, const VkCopyDescriptorSet* c) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.UpdateDescriptorSets) { RK_LOG_ONCE("UpdateDescriptorSets: 无下层"); return; }
    if (g_dump_passes && w) {
        std::lock_guard<std::mutex> lk(g_mtx);
        for (uint32_t i = 0; i < nw; i++) {
            const VkWriteDescriptorSet& ws = w[i];
            if (ws.dstSet) g_set_gen[ws.dstSet]++;
            if (!ws.pImageInfo) continue;
            if (ws.descriptorType != VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER &&
                ws.descriptorType != VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE &&
                ws.descriptorType != VK_DESCRIPTOR_TYPE_STORAGE_IMAGE &&
                ws.descriptorType != VK_DESCRIPTOR_TYPE_INPUT_ATTACHMENT) continue;
            for (uint32_t j = 0; j < ws.descriptorCount; j++)
                note_image_desc(ws.dstSet, ws.dstBinding, ws.dstArrayElement + j, ws.descriptorType,
                                ws.pImageInfo[j].imageView, ws.pImageInfo[j].sampler);
        }
    }
    d.UpdateDescriptorSets(dev, nw, w, nc, c);
}
// copyprobe 克隆集需要原 layout 的 binding 清单
static VKAPI_ATTR VkResult VKAPI_CALL rk_CreateDescriptorSetLayout(
    VkDevice dev, const VkDescriptorSetLayoutCreateInfo* ci, const VkAllocationCallbacks* a, VkDescriptorSetLayout* out) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.CreateDescriptorSetLayout) {
        RK_LOG_ONCE("CreateDescriptorSetLayout: 无下层"); return VK_ERROR_INITIALIZATION_FAILED;
    }
    VkResult r = d.CreateDescriptorSetLayout(dev, ci, a, out);
    if (r == VK_SUCCESS && ci && g_dump_passes) {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto& v = g_layout_bindings[*out];
        for (uint32_t i = 0; i < ci->bindingCount; i++) {
            const VkDescriptorSetLayoutBinding& b = ci->pBindings[i];
            v.push_back({b.binding, b.descriptorCount, (uint32_t)b.descriptorType});
        }
    }
    return r;
}
static VKAPI_ATTR VkResult VKAPI_CALL rk_AllocateDescriptorSets(
    VkDevice dev, const VkDescriptorSetAllocateInfo* ai, VkDescriptorSet* out) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.AllocateDescriptorSets) {
        RK_LOG_ONCE("AllocateDescriptorSets: 无下层"); return VK_ERROR_INITIALIZATION_FAILED;
    }
    VkResult r = d.AllocateDescriptorSets(dev, ai, out);
    if (r == VK_SUCCESS && ai && g_dump_passes) {
        std::lock_guard<std::mutex> lk(g_mtx);
        for (uint32_t i = 0; i < ai->descriptorSetCount; i++) {
            g_set_layout[out[i]] = ai->pSetLayouts[i];
            g_pool_sets[ai->descriptorPool].push_back(out[i]);
            if (g_pool_sets[ai->descriptorPool].size() > 4096)   // 兜底封顶, 防爆
                g_pool_sets[ai->descriptorPool].erase(g_pool_sets[ai->descriptorPool].begin());
        }
    }
    return r;
}
// 池子销毁时其中的 set 全死, 摘表防句柄复用后克隆错对象
static VKAPI_ATTR void VKAPI_CALL rk_DestroyDescriptorPool(
    VkDevice dev, VkDescriptorPool pool, const VkAllocationCallbacks* a) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.DestroyDescriptorPool) { RK_LOG_ONCE("DestroyDescriptorPool: 无下层"); return; }
    if (g_dump_passes) {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto it = g_pool_sets.find(pool);
        if (it != g_pool_sets.end()) {
            for (auto set : it->second) {
                g_set_views.erase(set); g_set_samplers.erase(set); g_set_layout.erase(set); g_set_storage.erase(set);
            }
            g_pool_sets.erase(it);
        }
    }
    d.DestroyDescriptorPool(dev, pool, a);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdBindDescriptorSets(
    VkCommandBuffer cb, VkPipelineBindPoint bp, VkPipelineLayout pl, uint32_t first,
    uint32_t n, const VkDescriptorSet* sets, uint32_t ndyn, const uint32_t* dyn) {
    DevDisp d;
    if (!dev_of(key(cb), &d) || !d.CmdBindDescriptorSets) { RK_LOG_ONCE("CmdBindDescriptorSets: 无下层"); return; }
    if (g_dump_passes && sets && bp == VK_PIPELINE_BIND_POINT_GRAPHICS) {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto& v = g_cb_sets[cb];
        for (uint32_t i = 0; i < n; i++) v.push_back(sets[i]);
        if (v.size() > 32) v.erase(v.begin(), v.end() - 32);
    }
    if (g_frameaudit && sets) {
        std::lock_guard<std::mutex> lk(g_mtx);
        FaCb& c = g_facb[cb];
        VkDescriptorSet* slots = bp == VK_PIPELINE_BIND_POINT_COMPUTE ? c.comp
                               : bp == VK_PIPELINE_BIND_POINT_GRAPHICS ? c.gfx : nullptr;
        if (slots) for (uint32_t i = 0; i < n && first + i < 8; i++) slots[first + i] = sets[i];
        if (bp == VK_PIPELINE_BIND_POINT_GRAPHICS) {
            if (FaEv* e = fa_open(cb)) { e->binds++; for (uint32_t i = 0; i < n; i++) fa_add_set(*e, sets[i]); }
        }
    }
    // copyprobe: 只在送显 pass 内替换 (UI draw 在别的 set 上, 不受影响)
    if ((g_copyprobe || g_upop) && !g_cp.disabled && sets && n > 0 && bp == VK_PIPELINE_BIND_POINT_GRAPHICS) {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto ci = g_cb_cur_is_swap.find(cb);
        if (ci != g_cb_cur_is_swap.end() && ci->second) {
            std::vector<VkDescriptorSet> repl(sets, sets + n);
            bool any = false;
            for (uint32_t i = 0; i < n; i++) {
                VkDescriptorSet c = copyprobe_clone(cb, d, repl[i]);
                if (c != repl[i]) { repl[i] = c; any = true; }
            }
            if (any) { d.CmdBindDescriptorSets(cb, bp, pl, first, n, repl.data(), ndyn, dyn); return; }
        }
    }
    d.CmdBindDescriptorSets(cb, bp, pl, first, n, sets, ndyn, dyn);
}

// framebuffer 才有宽高 —— render pass 上没有。两边都记, BeginRenderPass 时关联起来。
static VKAPI_ATTR VkResult VKAPI_CALL rk_CreateFramebuffer(
    VkDevice dev, const VkFramebufferCreateInfo* ci, const VkAllocationCallbacks* a, VkFramebuffer* out) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.CreateFramebuffer) {
        RK_LOG_ONCE("CreateFramebuffer: 查不到下层函数"); return VK_ERROR_INITIALIZATION_FAILED;
    }
    VkResult r = d.CreateFramebuffer(dev, ci, a, out);
    if (r == VK_SUCCESS && ci && g_dump_passes) {
        std::lock_guard<std::mutex> lk(g_mtx);
        g_fb_info[*out] = FbInfo{ci->width, ci->height, ci->renderPass};
        std::vector<VkImageView> att;
        for (uint32_t i = 0; i < ci->attachmentCount; i++) att.push_back(ci->pAttachments[i]);
        g_fb_att[*out] = std::move(att);
    }
    return r;
}

// 送显分辨率与格式的真值来源
static VKAPI_ATTR VkResult VKAPI_CALL rk_CreateSwapchainKHR(
    VkDevice dev, const VkSwapchainCreateInfoKHR* ci, const VkAllocationCallbacks* a, VkSwapchainKHR* out) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.CreateSwapchainKHR) {
        RK_LOG_ONCE("CreateSwapchainKHR: 查不到下层函数"); return VK_ERROR_INITIALIZATION_FAILED;
    }
    VkResult r = d.CreateSwapchainKHR(dev, ci, a, out);
    if (r == VK_SUCCESS && ci) {
        {
            // 读方 (write_marker / note_pass) 都持 g_mtx, 写方也必须持 (评审 #11: 否则是 UB
            // 数据竞争, swapchain 重建期间还会读到新旧混合的尺寸, is_swap 判错)
            std::lock_guard<std::mutex> lk(g_mtx);
            g_swap_w = ci->imageExtent.width; g_swap_h = ci->imageExtent.height;
            g_swap_fmt = (uint32_t)ci->imageFormat;
        }
        RK_LOG("swapchain %ux%u fmt=%u", ci->imageExtent.width, ci->imageExtent.height, (uint32_t)ci->imageFormat);
    }
    return r;
}

// kind: 0 = vkCmdDraw, 1 = vkCmdDrawIndexed, 2 = 间接 draw; prims = 顶点或索引数 × 实例数 (间接为 0)
static void note_draw(VkCommandBuffer cb, uint64_t n, int kind = 0, uint64_t prims = 0) {
    if (!g_dump_passes) return;
    std::lock_guard<std::mutex> lk(g_mtx);
    if (g_frameaudit) {
        if (FaEv* e = fa_open(cb)) {
            if (kind == 0) e->draws++; else if (kind == 1) e->draws_idx++; else e->draws_ind++;
            e->prims += prims;
        }
    }
    auto it = g_cb_cur.find(cb);
    if (it != g_cb_cur.end()) g_pass_stat[it->second].draws += n;

    int idx = g_cb_draw_idx[cb]++;
    // 只在"送显分辨率 pass 的第一笔 draw"上溯源, 且只做一次 (等稳态, 避开加载期)
    if (idx != 0 || !g_cb_cur_is_swap[cb] || !g_composite_report.empty()) return;
    if (g_frames.load() < 1800) return;

    // 上一个渲染分辨率 pass 的颜色附件 -> image (这就是"pass 49 的输出")
    std::vector<VkImage> prev;
    auto pi = g_cb_prev_att.find(cb);
    if (pi != g_cb_prev_att.end())
        for (auto v : pi->second) { auto vi = g_view.find(v); if (vi != g_view.end()) prev.push_back(vi->second); }

    // 这一笔 draw 当时绑着的描述符里, 所有采样图
    std::string sampled; bool hit = false; std::string hit_desc;
    auto si = g_cb_sets.find(cb);
    if (si != g_cb_sets.end()) {
        int printed = 0;
        for (auto set : si->second) {
            auto sv = g_set_views.find(set);
            if (sv == g_set_views.end()) continue;
            for (auto& bv : sv->second) {
                uint32_t binding = bv.first >> 8;   // key = arrayElem + (binding << 8)
                VkImageView view = bv.second;
                auto vi = g_view.find(view);
                if (vi == g_view.end()) continue;
                auto ii = g_img.find(vi->second);
                if (ii == g_img.end()) continue;
                const ImgInfo& im = ii->second;
                bool is_prev = false;
                for (auto p : prev) if (p == vi->second) is_prev = true;
                if (is_prev) {
                    hit = true;
                    char b[220];
                    snprintf(b, sizeof b, "{\"binding\":%u,\"w\":%u,\"h\":%u,\"fmt\":%u,\"usage\":%u}",
                             binding, im.w, im.h, im.fmt, im.usage);
                    hit_desc = b;
                }
                if (printed < 12) {
                    char b[240];
                    snprintf(b, sizeof b, "%s{\"binding\":%u,\"w\":%u,\"h\":%u,\"fmt\":%u,\"usage\":%u,\"is_prev_pass_output\":%s}",
                             sampled.empty() ? "" : ",", binding, im.w, im.h, im.fmt, im.usage, is_prev ? "true" : "false");
                    sampled += b; printed++;
                }
            }
        }
    }
    // 上一个渲染分辨率 pass 的附件本身也记一份 (含 usage), 供判断我们能不能读
    std::string prevs;
    for (auto p : prev) {
        auto ii = g_img.find(p); if (ii == g_img.end()) continue;
        char b[200];
        snprintf(b, sizeof b, "%s{\"w\":%u,\"h\":%u,\"fmt\":%u,\"usage\":%u}",
                 prevs.empty() ? "" : ",", ii->second.w, ii->second.h, ii->second.fmt, ii->second.usage);
        prevs += b;
    }
    size_t nsets = (si != g_cb_sets.end()) ? si->second.size() : 0;
    g_composite_report = std::string("{\"sets_bound_in_pass\":") + std::to_string(nsets)
        + ",\"confirmed\":" + (hit ? "true" : "false")
        + ",\"hit\":" + (hit_desc.empty() ? "null" : hit_desc)
        + ",\"sampled_by_first_draw\":[" + sampled + "]"
        + ",\"prev_pass_attachments\":[" + prevs + "]}";
    RK_LOG("composite draw 溯源: %s", g_composite_report.c_str());
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdDraw(VkCommandBuffer cb, uint32_t vc, uint32_t ic,
                                             uint32_t fv, uint32_t fi) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdDraw) { RK_LOG_ONCE("CmdDraw: 无下层"); return; }
    note_draw(cb, 1, 0, (uint64_t)vc * ic); d.CmdDraw(cb, vc, ic, fv, fi);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdDrawIndexed(VkCommandBuffer cb, uint32_t ic, uint32_t inst,
                                                    uint32_t fi, int32_t vo, uint32_t fin) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdDrawIndexed) { RK_LOG_ONCE("CmdDrawIndexed: 无下层"); return; }
    note_draw(cb, 1, 1, (uint64_t)ic * inst); d.CmdDrawIndexed(cb, ic, inst, fi, vo, fin);
}
// 放大也可能不是全屏三角形而是 vkCmdBlitImage, 所以单独记一笔
static VKAPI_ATTR void VKAPI_CALL rk_CmdBlitImage(
    VkCommandBuffer cb, VkImage si, VkImageLayout sl, VkImage di, VkImageLayout dl,
    uint32_t n, const VkImageBlit* r, VkFilter f) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdBlitImage) { RK_LOG_ONCE("CmdBlitImage: 无下层"); return; }
    if (g_dump_passes && n > 0 && r) {
        char buf[192];
        snprintf(buf, sizeof buf, "{\"op\":\"blit\",\"src\":[%d,%d],\"dst\":[%d,%d],\"filter\":%d}",
                 r[0].srcOffsets[1].x - r[0].srcOffsets[0].x, r[0].srcOffsets[1].y - r[0].srcOffsets[0].y,
                 r[0].dstOffsets[1].x - r[0].dstOffsets[0].x, r[0].dstOffsets[1].y - r[0].dstOffsets[0].y,
                 (int)f);
        std::lock_guard<std::mutex> lk(g_mtx);
        if (g_sampling && g_frame_seq.size() < 400) g_frame_seq.push_back(buf);
        if (g_frameaudit) {
            FaEv e; e.kind = FA_BLIT; e.regions = n; e.filter = (uint32_t)f;
            e.src = fa_img_res(si, r[0].srcSubresource.mipLevel, 1, r[0].srcSubresource.baseArrayLayer, r[0].srcSubresource.layerCount);
            e.dst = fa_img_res(di, r[0].dstSubresource.mipLevel, 1, r[0].dstSubresource.baseArrayLayer, r[0].dstSubresource.layerCount);
            e.sex[0] = (uint32_t)std::abs(r[0].srcOffsets[1].x - r[0].srcOffsets[0].x);
            e.sex[1] = (uint32_t)std::abs(r[0].srcOffsets[1].y - r[0].srcOffsets[0].y);
            e.ex[0] = (uint32_t)std::abs(r[0].dstOffsets[1].x - r[0].dstOffsets[0].x);
            e.ex[1] = (uint32_t)std::abs(r[0].dstOffsets[1].y - r[0].dstOffsets[0].y); e.ex[2] = 1;
            fa_push(cb, std::move(e));
        }
    }
    d.CmdBlitImage(cb, si, sl, di, dl, n, r, f);
}

// ══ copyprobe: 1:1 拷贝探针 (debug.knobs.copyprobe=1, 隐含打开溯源) ══
//
// 目的不是改画面, 是把"超分要用的全部机制"以最小形态走一遍, 探反作弊与驱动:
//   建 compute pipeline + 建 image + 注入 dispatch + 克隆并替换那一次描述符绑定,
//   但算子是 texelFetch 逐纹素 1:1 拷贝 —— 游戏的放大 shader 拿到的输入逐位相同,
//   最终画面应逐像素不变。任何一步失败都记日志并整体停用探针, 不静默回退。
//
// 时序: 合成 pass 的 vkCmdBeginRenderPass 里 (转发之前) 往同一条命令缓冲注入
//   barrier(src 可采样) -> dispatch(1:1 拷贝) -> barrier(dst 可采样),
// 然后 vkCmdBindDescriptorSets 钩子里把引用了 src 的那个绑定换成 dst 的克隆集。
// 源图 = 上一个渲染分辨率 pass 的颜色附件 (note_pass 里记的 g_cb_prev_att)。

static void cp_fail(const char* what, VkResult r) {
    RK_LOG("copyprobe 停用: %s -> %d", what, (int)r);
    g_cp.disabled = true;
}

// 建齐 compute 那套 (幂等; 尺寸变了重建 dst)。调用方必须持 g_mtx。
static bool copyprobe_ensure(VkDevice dev, const DevDisp& d, uint32_t w, uint32_t h, uint32_t fmt) {
    if (g_cp.disabled) return false;
    if (fmt != VK_FORMAT_R8G8B8A8_UNORM) {   // 算子写死 rgba8, 格式不符不猜
        RK_LOG_ONCE("copyprobe 停用: src fmt=%u 不是 R8G8B8A8_UNORM, 算子不适用", fmt);
        g_cp.disabled = true; return false;
    }
    const unsigned char* spv = g_upop ? kUpopSpv : kCopySpv;
    unsigned int spv_len = g_upop ? kUpopSpvLen : kCopySpvLen;
    if (spv_len == 0) { RK_LOG_ONCE("copyprobe 停用: 内嵌 SPV 为空 (构建没编 shader?)"); g_cp.disabled = true; return false; }
    VkResult r;
    if (g_cp.pipe == VK_NULL_HANDLE) {
        VkShaderModuleCreateInfo smci{VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO};
        smci.codeSize = spv_len; smci.pCode = (const uint32_t*)spv;
        r = d.CreateShaderModule(dev, &smci, nullptr, &g_cp.sm);
        if (r != VK_SUCCESS) { cp_fail("CreateShaderModule", r); return false; }

        VkDescriptorSetLayoutBinding lb[2]{};
        lb[0].binding = 0; lb[0].descriptorType = VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER;
        lb[0].descriptorCount = 1; lb[0].stageFlags = VK_SHADER_STAGE_COMPUTE_BIT;
        lb[1].binding = 1; lb[1].descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_IMAGE;
        lb[1].descriptorCount = 1; lb[1].stageFlags = VK_SHADER_STAGE_COMPUTE_BIT;
        VkDescriptorSetLayoutCreateInfo dlci{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO};
        dlci.bindingCount = 2; dlci.pBindings = lb;
        r = d.CreateDescriptorSetLayout(dev, &dlci, nullptr, &g_cp.dsl);
        if (r != VK_SUCCESS) { cp_fail("CreateDescriptorSetLayout", r); return false; }

        VkPipelineLayoutCreateInfo plci{VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO};
        plci.setLayoutCount = 1; plci.pSetLayouts = &g_cp.dsl;
        r = d.CreatePipelineLayout(dev, &plci, nullptr, &g_cp.pl);
        if (r != VK_SUCCESS) { cp_fail("CreatePipelineLayout", r); return false; }

        VkComputePipelineCreateInfo cpci{VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO};
        cpci.stage.sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO;
        cpci.stage.stage = VK_SHADER_STAGE_COMPUTE_BIT;
        cpci.stage.module = g_cp.sm; cpci.stage.pName = "main";
        cpci.layout = g_cp.pl;
        r = d.CreateComputePipelines(dev, VK_NULL_HANDLE, 1, &cpci, nullptr, &g_cp.pipe);
        if (r != VK_SUCCESS) { cp_fail("CreateComputePipelines", r); return false; }

        // 池子要同时装: compute set + 每帧克隆的游戏 set。克隆集里可能有游戏的任何
        // 描述符类型, 全类型留余量; FREE_BIT 让换下来的旧克隆能还回池子 (游戏每帧
        // 重写原 set 时代数就涨, 不还的话 60fps 下十几秒就把 maxSets 耗尽)。
        VkDescriptorPoolSize ps[8]{};
        ps[0].type = VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER; ps[0].descriptorCount = 2048;
        ps[1].type = VK_DESCRIPTOR_TYPE_STORAGE_IMAGE;          ps[1].descriptorCount = 256;
        ps[2].type = VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER;         ps[2].descriptorCount = 2048;
        ps[3].type = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER;         ps[3].descriptorCount = 512;
        ps[4].type = VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE;          ps[4].descriptorCount = 1024;
        ps[5].type = VK_DESCRIPTOR_TYPE_SAMPLER;                ps[5].descriptorCount = 1024;
        ps[6].type = VK_DESCRIPTOR_TYPE_INPUT_ATTACHMENT;       ps[6].descriptorCount = 256;
        ps[7].type = VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC; ps[7].descriptorCount = 256;
        VkDescriptorPoolCreateInfo dpci{VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO};
        dpci.flags = VK_DESCRIPTOR_POOL_CREATE_FREE_DESCRIPTOR_SET_BIT;
        dpci.maxSets = 1024; dpci.poolSizeCount = 8; dpci.pPoolSizes = ps;
        r = d.CreateDescriptorPool(dev, &dpci, nullptr, &g_cp.pool);
        if (r != VK_SUCCESS) { cp_fail("CreateDescriptorPool", r); return false; }

        VkDescriptorSetAllocateInfo dsai{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO};
        dsai.descriptorPool = g_cp.pool; dsai.descriptorSetCount = 1; dsai.pSetLayouts = &g_cp.dsl;
        r = d.AllocateDescriptorSets(dev, &dsai, &g_cp.cset);
        if (r != VK_SUCCESS) { cp_fail("AllocateDescriptorSets(compute)", r); return false; }
        g_cp.dev = dev;
        RK_LOG("copyprobe: compute pipeline 建好了");
    }
    if (g_cp.w != w || g_cp.h != h) {   // dst 与 src 同尺寸同格式: 逐纹素拷贝, 不是放大
        if (g_cp.dst != VK_NULL_HANDLE) {
            d.DestroyImageView(dev, g_cp.dst_view, nullptr);
            d.DestroyImage(dev, g_cp.dst, nullptr);
            d.FreeMemory(dev, g_cp.mem, nullptr);
            g_cp.dst = VK_NULL_HANDLE;
        }
        VkImageCreateInfo ici{VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO};
        ici.imageType = VK_IMAGE_TYPE_2D; ici.format = (VkFormat)fmt;
        ici.extent = {w, h, 1}; ici.mipLevels = 1; ici.arrayLayers = 1;
        ici.samples = VK_SAMPLE_COUNT_1_BIT; ici.tiling = VK_IMAGE_TILING_OPTIMAL;
        ici.usage = VK_IMAGE_USAGE_STORAGE_BIT | VK_IMAGE_USAGE_SAMPLED_BIT;
        ici.sharingMode = VK_SHARING_MODE_EXCLUSIVE; ici.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
        r = d.CreateImage(dev, &ici, nullptr, &g_cp.dst);
        if (r != VK_SUCCESS) { cp_fail("CreateImage(dst)", r); return false; }
        VkMemoryRequirements mr{}; {   // vkGetImageMemoryRequirements 还没挂, 走 gdpa 现取
            auto fn = (PFN_vkGetImageMemoryRequirements)d.gdpa(dev, "vkGetImageMemoryRequirements");
            if (!fn) { cp_fail("vkGetImageMemoryRequirements 拿不到", VK_ERROR_INITIALIZATION_FAILED); return false; }
            fn(dev, g_cp.dst, &mr);
        }
        VkPhysicalDeviceMemoryProperties mp{};
        d.GetPhysMemProps(d.phys, &mp);
        uint32_t mt = UINT32_MAX;
        for (uint32_t i = 0; i < mp.memoryTypeCount; i++)
            if ((mr.memoryTypeBits & (1u << i)) &&
                (mp.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)) { mt = i; break; }
        if (mt == UINT32_MAX) { cp_fail("没有 DEVICE_LOCAL 显存类型", VK_ERROR_INITIALIZATION_FAILED); return false; }
        VkMemoryAllocateInfo mai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};
        mai.allocationSize = mr.size; mai.memoryTypeIndex = mt;
        r = d.AllocateMemory(dev, &mai, nullptr, &g_cp.mem);
        if (r != VK_SUCCESS) { cp_fail("AllocateMemory", r); return false; }
        r = d.BindImageMemory(dev, g_cp.dst, g_cp.mem, 0);
        if (r != VK_SUCCESS) { cp_fail("BindImageMemory", r); return false; }
        VkImageViewCreateInfo vci{VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO};
        vci.image = g_cp.dst; vci.viewType = VK_IMAGE_VIEW_TYPE_2D; vci.format = (VkFormat)fmt;
        vci.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
        r = d.CreateImageView(dev, &vci, nullptr, &g_cp.dst_view);
        if (r != VK_SUCCESS) { cp_fail("CreateImageView(dst)", r); return false; }
        g_cp.w = w; g_cp.h = h; g_cp.fmt = fmt;
        RK_LOG("copyprobe: dst image %ux%u fmt=%u 建好了", w, h, fmt);
    }
    return true;
}

// 合成 pass 开始时 (转发 BeginRenderPass 之前) 注入拷贝。调用方必须持 g_mtx。
static void copyprobe_dispatch(VkCommandBuffer cb, const DevDisp& d, VkRenderPass prev_rp,
                               const std::vector<VkImageView>& prev_att) {
    (void)prev_rp;   // 布局判断改走"游戏自己已 transition"论证, 见下; 参数留着备用
    if (g_cp.disabled || prev_att.empty()) return;
    // 源图 = 上一个渲染分辨率 pass 的颜色附件
    auto vi = g_view.find(prev_att[0]);
    if (vi == g_view.end()) return;
    VkImage src = vi->second;
    auto ii = g_img.find(src);
    if (ii == g_img.end()) return;
    const ImgInfo& im = ii->second;
    if (!(im.usage & VK_IMAGE_USAGE_SAMPLED_BIT)) { RK_LOG_ONCE("copyprobe 停用: src 不可采样"); g_cp.disabled = true; return; }
    // 布局: 游戏在 pass 50 里用普通采样描述符读 src, 而 src 又不是 pass 50 framebuffer
    // 的附件 (fb 2141x969 vs src 2140x968 尺寸不符, 挂不上) —— 所以游戏必定在 pass 49
    // 结束之后、pass 50 开始之前自己插了 transition barrier 把它转成可采样布局,
    // 否则游戏自己就是非法采样。我们注入的位置在 pass 50 begin 处, 排在游戏那道
    // barrier 之后, src 已经是 SHADER_READ_ONLY_OPTIMAL, 我们只做执行依赖不动布局。
    // (上一版拿 pass 49 的 finalLayout=COLOR_ATTACHMENT_OPTIMAL 当注入时布局, 错了:
    //  那是渲染 pass 结束时的布局, 不是合成 pass 开始时的布局。2026-09-23 实测。)
    uint32_t src_layout = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
    // copy 探针: dst 与 src 同尺寸 (逐纹素拷贝)。upop: dst 是送显分辨率 (放大就是算子干的活)。
    uint32_t dw = im.w, dh = im.h;
    if (g_upop) {
        if (!g_swap_w || !g_swap_h) return;   // swapchain 还没建, 首几帧跳过
        dw = g_swap_w; dh = g_swap_h;
    }
    if (!copyprobe_ensure(d.dev, d, dw, dh, im.fmt)) return;

    // compute set 绑上本帧的 src: view 直接用 pass 49 颜色附件的 view, sampler 从
    // 游戏对 src 的既有描述符写里拿 (combined image sampler 必须带 sampler)。
    VkSampler src_samp = VK_NULL_HANDLE; VkImageView src_view = prev_att[0];
    for (auto& kv : g_set_views) {
        for (auto& bv : kv.second)
            if (bv.second == src_view) { auto si = g_set_samplers[kv.first].find(bv.first);
                if (si != g_set_samplers[kv.first].end()) src_samp = si->second; }
        if (src_samp) break;
    }
    if (!src_samp) return;   // 游戏还没写过引用 src 的描述符, 本帧跳过 (首帧常见)

    VkDescriptorImageInfo dii[2]{};
    dii[0] = {src_samp, src_view, (VkImageLayout)src_layout};
    dii[1] = {VK_NULL_HANDLE, g_cp.dst_view, VK_IMAGE_LAYOUT_GENERAL};
    VkWriteDescriptorSet wr[2]{};
    for (int i = 0; i < 2; i++) {
        wr[i].sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET; wr[i].dstSet = g_cp.cset;
        wr[i].dstBinding = (uint32_t)i; wr[i].descriptorCount = 1;
        wr[i].descriptorType = i == 0 ? VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER
                                      : VK_DESCRIPTOR_TYPE_STORAGE_IMAGE;
        wr[i].pImageInfo = &dii[i];
    }
    d.UpdateDescriptorSets(d.dev, 2, wr, 0, nullptr);

    // 注入: 三条命令全走下层直调, 不再回本层钩子 (避免递归/重复计数)
    VkImageMemoryBarrier b[2]{};
    b[0].sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER;
    b[0].srcAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT | VK_ACCESS_SHADER_READ_BIT;
    b[0].dstAccessMask = VK_ACCESS_SHADER_READ_BIT;
    b[0].oldLayout = (VkImageLayout)src_layout; b[0].newLayout = (VkImageLayout)src_layout;
    b[0].srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED; b[0].dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
    b[0].image = src; b[0].subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    b[1].sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER;
    b[1].srcAccessMask = 0; b[1].dstAccessMask = VK_ACCESS_SHADER_WRITE_BIT;
    b[1].oldLayout = VK_IMAGE_LAYOUT_UNDEFINED; b[1].newLayout = VK_IMAGE_LAYOUT_GENERAL;
    b[1].srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED; b[1].dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
    b[1].image = g_cp.dst; b[1].subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    d.CmdPipelineBarrier(cb, VK_PIPELINE_STAGE_ALL_GRAPHICS_BIT,
                         VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT, 0, 0, nullptr, 0, nullptr, 2, b);
    d.CmdBindPipeline(cb, VK_PIPELINE_BIND_POINT_COMPUTE, g_cp.pipe);
    d.CmdBindDescriptorSets(cb, VK_PIPELINE_BIND_POINT_COMPUTE, g_cp.pl, 0, 1, &g_cp.cset, 0, nullptr);
    d.CmdDispatch(cb, (g_cp.w + 15) / 16, (g_cp.h + 15) / 16, 1);
    VkImageMemoryBarrier b2{};
    b2.sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER;
    b2.srcAccessMask = VK_ACCESS_SHADER_WRITE_BIT; b2.dstAccessMask = VK_ACCESS_SHADER_READ_BIT;
    b2.oldLayout = VK_IMAGE_LAYOUT_GENERAL; b2.newLayout = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
    b2.srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED; b2.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
    b2.image = g_cp.dst; b2.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    d.CmdPipelineBarrier(cb, VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,
                         VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, 0, 0, nullptr, 0, nullptr, 1, &b2);
    g_cp.src_img = src;
    g_cp.copies++;
}

// 游戏在合成 pass 里绑描述符时: 哪个 set 引用了本帧的 src, 就换成它的克隆
// (逐 binding 拷贝, 只把 src 那一个绑定改指 dst)。调用方必须持 g_mtx。
static VkDescriptorSet copyprobe_clone(VkCommandBuffer, const DevDisp& d, VkDescriptorSet game_set) {
    if (g_cp.disabled || g_cp.src_img == VK_NULL_HANDLE) return game_set;
    // 这个 set 里哪个绑定引用了 src?
    auto sv = g_set_views.find(game_set);
    if (sv == g_set_views.end()) return game_set;
    uint32_t target_key = UINT32_MAX;
    for (auto& bv : sv->second) {
        auto vi = g_view.find(bv.second);
        if (vi != g_view.end() && vi->second == g_cp.src_img) { target_key = bv.first; break; }
    }
    if (target_key == UINT32_MAX) return game_set;
    auto li = g_set_layout.find(game_set);
    if (li == g_set_layout.end()) return game_set;
    VkDescriptorSetLayout layout = li->second;
    auto bi = g_layout_bindings.find(layout);
    if (bi == g_layout_bindings.end()) return game_set;
    uint64_t gen = g_set_gen[game_set];
    auto ci = g_cp.clones.find(game_set);
    if (ci == g_cp.clones.end() || ci->second.second != gen) {
        VkDescriptorSet clone = VK_NULL_HANDLE;
        VkDescriptorSetAllocateInfo ai{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO};
        ai.descriptorPool = g_cp.pool; ai.descriptorSetCount = 1; ai.pSetLayouts = &layout;
        VkResult r = d.AllocateDescriptorSets(d.dev, &ai, &clone);
        if (r != VK_SUCCESS) { cp_fail("AllocateDescriptorSets(clone)", r); return game_set; }
        // 逐 binding 全量拷贝原 set, 再单独覆写 src 那一个
        std::vector<VkCopyDescriptorSet> copies;
        for (auto& lb : bi->second) {
            if ((lb.binding << 8) == (target_key & ~0xFFu)) continue;   // 目标 binding 跳过
            VkCopyDescriptorSet c{};
            c.sType = VK_STRUCTURE_TYPE_COPY_DESCRIPTOR_SET;
            c.srcSet = game_set; c.srcBinding = lb.binding; c.srcArrayElement = 0;
            c.dstSet = clone;    c.dstBinding = lb.binding; c.dstArrayElement = 0;
            c.descriptorCount = lb.count;
            copies.push_back(c);
        }
        if (!copies.empty()) d.UpdateDescriptorSets(d.dev, 0, nullptr, (uint32_t)copies.size(), copies.data());
        // 覆写: 同 binding 的同一 arrayElem, 换 view 不换 sampler
        VkSampler samp = VK_NULL_HANDLE;
        auto smi = g_set_samplers.find(game_set);
        if (smi != g_set_samplers.end()) { auto it = smi->second.find(target_key); if (it != smi->second.end()) samp = it->second; }
        if (!samp) return game_set;
        VkDescriptorImageInfo dii{samp, g_cp.dst_view, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
        VkWriteDescriptorSet w{};
        w.sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET; w.dstSet = clone;
        w.dstBinding = target_key >> 8; w.dstArrayElement = target_key & 0xFF;
        w.descriptorCount = 1; w.descriptorType = VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER;
        w.pImageInfo = &dii;
        d.UpdateDescriptorSets(d.dev, 1, &w, 0, nullptr);
        if (ci != g_cp.clones.end()) {
            d.FreeDescriptorSets(d.dev, g_cp.pool, 1, &ci->second.first);
            ci->second = {clone, gen};
        } else g_cp.clones[game_set] = {clone, gen};
        if (g_cp.clones.size() > 512) { RK_LOG_ONCE("copyprobe: 克隆表超 512, 停用防爆"); g_cp.disabled = true; return game_set; }
        ci = g_cp.clones.find(game_set);
    }
    g_cp.subs++;
    return ci->second.first;
}

// 把 (render pass 形状) 与 (framebuffer 宽高) 关联起来, 按 (pass, 宽, 高) 聚合计数,
// 并在采样帧里记下有序序列 —— "第几个 pass、多大、什么格式" 就是从这儿读出来的。
static void note_pass(VkCommandBuffer cb, const VkRenderPassBeginInfo* bi, const DevDisp& d) {
    if (!g_dump_passes || !bi) return;
    std::lock_guard<std::mutex> lk(g_mtx);
    uint32_t w = bi->renderArea.extent.width, h = bi->renderArea.extent.height;
    auto fi = g_fb_info.find(bi->framebuffer);
    if (fi != g_fb_info.end()) { if (fi->second.w) w = fi->second.w; if (fi->second.h) h = fi->second.h; }
    uint64_t k = pass_key(bi->renderPass, w, h);
    auto& st = g_pass_stat[k];
    if (!st.begins) {
        st.w = w; st.h = h;
        auto ri = g_rp_info.find(bi->renderPass);
        if (ri != g_rp_info.end()) { st.n_att = ri->second.n_att; st.fmt = ri->second.fmt; }
    }
    st.begins++;
    g_cb_cur[cb] = k;
    { auto& v = g_cb_passes[cb]; if (v.size() < 256) v.push_back(k); }   // 录制清单, 提交时折算成执行
    g_cb_draw_idx[cb] = 0;                       // 进新 pass, draw 序号归零
    g_cb_sets[cb].clear();                       // 只统计"在本 pass 内绑定"的描述符, 避免把上一个 pass 的算进来
    bool is_swap = (w == g_swap_w && h == g_swap_h);
    auto ai = g_fb_att.find(bi->framebuffer);
    if (!is_swap && ai != g_fb_att.end()) {
        g_cb_prev_att[cb] = ai->second;   // 记住上一个渲染分辨率 pass 的附件
        g_cb_prev_rp[cb] = bi->renderPass;
    }
    if (is_swap) {
        g_cb_cur_is_swap[cb] = true;
        if (g_copyprobe || g_upop) copyprobe_dispatch(cb, d, g_cb_prev_rp.count(cb) ? g_cb_prev_rp[cb] : VK_NULL_HANDLE,
                                            g_cb_prev_att.count(cb) ? g_cb_prev_att[cb] : std::vector<VkImageView>{});
    } else g_cb_cur_is_swap[cb] = false;
    if (g_sampling && g_frame_seq.size() < 400) {
        uint64_t seq = 0; auto si = g_rp_seq.find(bi->renderPass);
        if (si != g_rp_seq.end()) seq = si->second;
        char buf[192];
        snprintf(buf, sizeof buf, "{\"pass\":%llu,\"w\":%u,\"h\":%u,\"att\":%u,\"fmt0\":%u}",
                 (unsigned long long)seq, w, h, st.n_att, st.fmt.empty() ? 0u : st.fmt[0]);
        g_frame_seq.push_back(buf);
    }
    if (g_frameaudit) {
        FaEv e; e.kind = FA_PASS; e.w = w; e.h = h;
        auto si = g_rp_seq.find(bi->renderPass); if (si != g_rp_seq.end()) e.rp = si->second;
        { auto gi = g_rp_sig.find(bi->renderPass); if (gi != g_rp_sig.end()) e.sig = gi->second; }
        std::vector<VkImageView> views;
        if (ai != g_fb_att.end()) views = ai->second;
        for (auto* p = reinterpret_cast<const VkBaseInStructure*>(bi->pNext); p; p = p->pNext)
            if (p->sType == VK_STRUCTURE_TYPE_RENDER_PASS_ATTACHMENT_BEGIN_INFO) {   // imageless framebuffer
                auto* ab = reinterpret_cast<const VkRenderPassAttachmentBeginInfo*>(p);
                views.assign(ab->pAttachments, ab->pAttachments + ab->attachmentCount);
            }
        auto ri = g_rp_fa.find(bi->renderPass);
        size_t na = ri != g_rp_fa.end() ? ri->second.att.size() : views.size();
        for (size_t i = 0; i < na; i++) {
            FaAtt a = ri != g_rp_fa.end() ? ri->second.att[i] : FaAtt{};
            if (i < views.size()) a.r = fa_view_res(views[i]);
            e.att.push_back(a);
        }
        if (ri != g_rp_fa.end()) e.subpasses = 1;   // NextSubpass 再加
        FaCb& c = g_facb[cb];
        for (auto s : c.gfx) if (s) fa_add_set(e, s);   // pass 前已绑的 set 在 pass 内照样生效
        for (auto& pv : c.push_gfx) fa_add_view(e, pv.first, pv.second);
        e.ts0 = fa_ts(cb, d);
        if (fa_push(cb, std::move(e))) c.open = (int)c.ev.size() - 1;
    }
}

static VKAPI_ATTR void VKAPI_CALL rk_CmdBeginRenderPass(
    VkCommandBuffer cb, const VkRenderPassBeginInfo* bi, VkSubpassContents c) {
    DevDisp d;
    if (!dev_of(key(cb), &d) || !d.CmdBeginRenderPass) {
        RK_LOG_ONCE("CmdBeginRenderPass: 查不到下层函数, 本次不计数也不转发");
        return;
    }
    VkRenderPassBeginInfo sub;
    bi = twin_begin(bi, &sub);
    g_rp++;
    note_begin(bi ? bi->renderPass : VK_NULL_HANDLE);
    note_pass(cb, bi, d);
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
    VkRenderPassBeginInfo sub;
    bi = twin_begin(bi, &sub);
    g_rp++;
    note_begin(bi ? bi->renderPass : VK_NULL_HANDLE);
    note_pass(cb, bi, d);       // 与 CmdBeginRenderPass 一致, 漏了它 RenderPass2 开的 pass 就不进表
    d.CmdBeginRenderPass2(cb, bi, si);
}
// 二级命令缓冲里的 draw 没有自己的 BeginRenderPass —— 当前 pass 从继承信息里拿 (评审 #9:
// 不然录进 secondary 的 draw 在 g_cb_cur 里匹配不上, draws 系统性偏低, 按 draw 数找
// "放大那一步"会选错 pass)。顺手在每次重录开始时清掉这条 cb 的旧状态 (评审 #10 的兜底:
// cb 反复重录时这些 per-cb 表才不会只增不减)。
static VKAPI_ATTR VkResult VKAPI_CALL rk_BeginCommandBuffer(VkCommandBuffer cb, const VkCommandBufferBeginInfo* bi) {
    DevDisp d;
    if (!dev_of(key(cb), &d) || !d.BeginCommandBuffer) {
        RK_LOG_ONCE("BeginCommandBuffer: 无下层"); return VK_ERROR_INITIALIZATION_FAILED;
    }
    if (g_dump_passes) {
        std::lock_guard<std::mutex> lk(g_mtx);
        g_cb_cur.erase(cb); g_cb_sets.erase(cb); g_cb_draw_idx.erase(cb);
        g_cb_prev_att.erase(cb); g_cb_cur_is_swap.erase(cb); g_cb_passes.erase(cb);
        if (g_frameaudit) {
            FaCb& c = g_facb[cb];
            c = FaCb{};
            c.ts = g_frameaudit_ts && g_fa.ts_armed;
            if (bi && (bi->flags & VK_COMMAND_BUFFER_USAGE_RENDER_PASS_CONTINUE_BIT) && bi->pInheritanceInfo) {
                // 二级缓冲: 一个「继承来的」伪 pass 收 draw / 描述符引用, 执行时并进主缓冲当前 pass
                FaEv e; e.kind = FA_PASS; e.sec = true; c.ts = false;
                auto si = g_rp_seq.find(bi->pInheritanceInfo->renderPass); if (si != g_rp_seq.end()) e.rp = si->second;
                c.ev.push_back(std::move(e)); c.open = 0;
            }
        }
        if (bi && (bi->flags & VK_COMMAND_BUFFER_USAGE_RENDER_PASS_CONTINUE_BIT) && bi->pInheritanceInfo) {
            const VkCommandBufferInheritanceInfo* inh = bi->pInheritanceInfo;
            uint32_t w = 0, h = 0;   // inheritance 的 framebuffer 允许是 VK_NULL_HANDLE, 那就只有 0x0
            auto fi = g_fb_info.find(inh->framebuffer);
            if (fi != g_fb_info.end()) { w = fi->second.w; h = fi->second.h; }
            uint64_t k = pass_key(inh->renderPass, w, h);
            auto& st = g_pass_stat[k];
            if (!st.begins) {
                st.w = w; st.h = h;
                auto ri = g_rp_info.find(inh->renderPass);
                if (ri != g_rp_info.end()) { st.n_att = ri->second.n_att; st.fmt = ri->second.fmt; }
            }
            st.begins++;
            g_cb_cur[cb] = k;
            g_cb_draw_idx[cb] = 0;
            g_cb_cur_is_swap[cb] = false;
            { auto& v = g_cb_passes[cb]; if (v.size() < 256) v.push_back(k); }
        }
    }
    return d.BeginCommandBuffer(cb, bi);
}

// 二级缓冲被执行时, 它录的 pass 并入主缓冲的录制清单 —— 提交主缓冲时才算执行 (评审 #4/#9)
static VKAPI_ATTR void VKAPI_CALL rk_CmdExecuteCommands(VkCommandBuffer cb, uint32_t n, const VkCommandBuffer* cbs) {
    DevDisp d;
    if (!dev_of(key(cb), &d) || !d.CmdExecuteCommands) { RK_LOG_ONCE("CmdExecuteCommands: 无下层"); return; }
    if (g_dump_passes && cbs) {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto& v = g_cb_passes[cb];
        for (uint32_t i = 0; i < n; i++) {
            auto it = g_cb_passes.find(cbs[i]);
            if (it == g_cb_passes.end()) continue;
            for (uint64_t k : it->second) { if (v.size() >= 256) break; v.push_back(k); }
        }
        if (g_frameaudit) {
            for (uint32_t i = 0; i < n; i++) {
                auto sit = g_facb.find(cbs[i]);
                if (sit == g_facb.end()) continue;
                std::vector<FaEv> sev = sit->second.ev;   // 拷一份: 下面 g_facb[cb] 可能让迭代器所在的树重排
                FaEv* open = fa_open(cb);
                for (auto& e : sev) {
                    if (e.sec && open) {
                        open->draws += e.draws; open->draws_idx += e.draws_idx; open->draws_ind += e.draws_ind;
                        open->prims += e.prims; open->binds += e.binds; open->clear_att += e.clear_att;
                        open->clear_att_full += e.clear_att_full;
                        open->q_occ += e.q_occ; open->q_other += e.q_other;
                        open->reads.insert(e.reads.begin(), e.reads.end());
                        open->writes.insert(e.writes.begin(), e.writes.end());
                        g_fa.sec_merged++;
                    } else if (!e.sec) {
                        fa_push(cb, FaEv(e));
                        open = fa_open(cb);
                    }
                }
            }
        }
    }
    d.CmdExecuteCommands(cb, n, cbs);
}

// ══ frameaudit 钩子与落盘 (全部只读转发; 唯一插入的命令是 frameaudit_ts 时的查询 reset/写时间戳) ══

static void fa_note_img(uint32_t id) {
    if (!id || g_fa.imgs.count(id)) return;
    auto it = g_img_by_id.find(id);
    if (it != g_img_by_id.end()) g_fa.imgs[id] = it->second;
}
static std::string fa_sub_json(const SubRes& r) {
    fa_note_img(r.img);
    char b[64]; snprintf(b, sizeof b, "[%u,%u,%u]", r.img, r.mip, r.layer);
    return b;
}
static std::string fa_refs_json(const std::set<uint64_t>& s) {
    std::string o = "[";
    for (uint64_t p : s) {
        uint32_t id = (uint32_t)(p >> 32);
        fa_note_img(id);
        char b[80];
        snprintf(b, sizeof b, "%s[%u,%u,%u,%u,%u]", o.size() > 1 ? "," : "", id,
                 (uint32_t)((p >> 24) & 255), (uint32_t)((p >> 16) & 255), (uint32_t)((p >> 8) & 255), (uint32_t)(p & 255));
        o += b;
    }
    return o + "]";
}
static const char* fa_kind_name(uint8_t k) {
    switch (k) {
    case FA_PASS: return "pass"; case FA_DISPATCH: return "dispatch"; case FA_COPY_IMG: return "copy_img";
    case FA_BUF2IMG: return "buf2img"; case FA_IMG2BUF: return "img2buf"; case FA_COPY_BUF: return "copy_buf";
    case FA_BLIT: return "blit"; case FA_RESOLVE: return "resolve"; case FA_CLEAR_COLOR: return "clear_color";
    case FA_CLEAR_DS: return "clear_ds"; case FA_DISCARD: return "discard"; case FA_FILL_BUF: return "fill_buf";
    default: return "?";
    }
}
static std::string fa_ev_json(const FaEv& e) {
    char b[512];
    std::string o = std::string("{\"k\":\"") + fa_kind_name(e.kind) + "\"";
    snprintf(b, sizeof b, ",\"br\":%u", e.barriers); o += b;
    if (e.kind == FA_PASS) {
        snprintf(b, sizeof b, ",\"rp\":%llu,\"w\":%u,\"h\":%u,\"dyn\":%s,\"sp\":%u,\"dr\":%u,\"dri\":%u,\"drn\":%u,"
                 "\"prims\":%llu,\"ca\":%u,\"caf\":%u,\"bd\":%u,\"t0\":%d,\"t1\":%d,\"qo\":%u,\"qx\":%u",
                 (unsigned long long)e.rp, e.w, e.h, e.dyn ? "true" : "false", e.subpasses, e.draws, e.draws_idx,
                 e.draws_ind, (unsigned long long)e.prims, e.clear_att, e.clear_att_full, e.binds, e.ts0, e.ts1,
                 e.q_occ, e.q_other);
        o += b;
        if (e.sig) o += ",\"sig\":\"" + sig_hex(e.sig) + "\"";
        o += ",\"att\":[";
        for (size_t i = 0; i < e.att.size(); i++) {
            const FaAtt& a = e.att[i];
            fa_note_img(a.r.img);
            snprintf(b, sizeof b, "%s{\"i\":%u,\"m\":%u,\"l\":%u,\"f\":%u,\"s\":%u,\"lo\":%u,\"st\":%u,\"slo\":%u,\"sst\":%u,"
                     "\"il\":%u,\"fl\":%u,\"ro\":%u,\"in\":%s}",
                     i ? "," : "", a.r.img, a.r.mip, a.r.layer, a.fmt, a.samples, a.load, a.store, a.sload, a.sstore,
                     a.init, a.fin, a.role, a.input ? "true" : "false");
            o += b;
        }
        o += "]";
    } else if (e.kind == FA_DISPATCH) {
        snprintf(b, sizeof b, ",\"g\":[%u,%u,%u],\"ind\":%s", e.gx, e.gy, e.gz, e.indirect ? "true" : "false");
        o += b;
    } else {
        if (e.src.img || e.kind == FA_COPY_IMG || e.kind == FA_IMG2BUF || e.kind == FA_BLIT || e.kind == FA_RESOLVE)
            o += ",\"src\":" + fa_sub_json(e.src);
        if (e.dst.img || e.kind == FA_COPY_IMG || e.kind == FA_BUF2IMG || e.kind == FA_BLIT || e.kind == FA_RESOLVE ||
            e.kind == FA_CLEAR_COLOR || e.kind == FA_CLEAR_DS || e.kind == FA_DISCARD) {
            o += ",\"dst\":" + fa_sub_json(e.dst);
            snprintf(b, sizeof b, ",\"dnm\":%u,\"dnl\":%u", e.dst.nmip, e.dst.nlayer); o += b;
        }
        snprintf(b, sizeof b, ",\"ex\":[%u,%u,%u],\"sex\":[%u,%u],\"n\":%u,\"tx\":%llu,\"bytes\":%llu,\"filter\":%u",
                 e.ex[0], e.ex[1], e.ex[2], e.sex[0], e.sex[1], e.regions, (unsigned long long)e.texels,
                 (unsigned long long)e.bytes, e.filter);
        o += b;
    }
    if (!e.reads.empty()) o += ",\"rd\":" + fa_refs_json(e.reads);
    if (!e.writes.empty()) o += ",\"wr\":" + fa_refs_json(e.writes);
    return o + "}";
}

// 调用方持 g_mtx
static std::map<VkCommandBuffer, uint32_t> g_fa_cbid;
static void fa_capture_submit(void* q, uint32_t n, const VkCommandBuffer* cbs) {
    auto qi = g_fa.qid.find(q);
    uint32_t qn = qi != g_fa.qid.end() ? qi->second : (g_fa.qid[q] = (uint32_t)g_fa.qid.size());
    std::string sub = std::string(g_fa.cur.empty() ? "" : ",") + "{\"q\":" + std::to_string(qn) + ",\"cbs\":[";
    for (uint32_t i = 0; i < n; i++) {
        if (i) sub += ",";
        auto it = g_facb.find(cbs[i]);
        if (it == g_facb.end()) { g_fa.cb_unseen++; sub += "{\"unseen\":true}"; continue; }
        auto ci = g_fa_cbid.find(cbs[i]);
        uint32_t cid = ci != g_fa_cbid.end() ? ci->second : (g_fa_cbid[cbs[i]] = (uint32_t)g_fa_cbid.size());
        sub += "{\"cb\":" + std::to_string(cid) + ",\"ts\":" + (it->second.ts ? "true" : "false")
             + ",\"ov\":" + (it->second.overflow ? "true" : "false") + ",\"ev\":[";
        bool first = true;
        for (const auto& e : it->second.ev) {
            if (e.sec) continue;   // 二级缓冲的伪 pass 已并进主缓冲
            if (!first) sub += ",";
            first = false;
            sub += fa_ev_json(e);
        }
        sub += "]}";
    }
    g_fa.cur += sub + "]}";
}

static void fa_write_file(const DevDisp& d) {   // 调用方持 g_mtx
    std::string ts = "[";
    uint32_t avail = 0;
    if (g_fa.pool != VK_NULL_HANDLE && d.GetQueryPoolResults && g_fa.ts_next > g_fa.ts_lo) {
        uint32_t cnt = g_fa.ts_next - g_fa.ts_lo;
        std::vector<uint64_t> buf((size_t)cnt * 2, 0);
        d.GetQueryPoolResults(g_fa.pool_dev, g_fa.pool, g_fa.ts_lo, cnt, buf.size() * 8, buf.data(), 16,
                              VK_QUERY_RESULT_64_BIT | VK_QUERY_RESULT_WITH_AVAILABILITY_BIT);   // 不等待; 没好的槽 availability=0
        for (uint32_t i = 0; i < cnt; i++) {
            if (!buf[2 * i + 1]) continue;
            char b[64]; snprintf(b, sizeof b, "%s[%u,%llu]", avail ? "," : "", g_fa.ts_lo + i, (unsigned long long)buf[2 * i]);
            ts += b; avail++;
        }
    }
    ts += "]";
    std::string imgs = "[";
    for (auto& kv : g_fa.imgs) {
        const ImgInfo& im = kv.second;
        char b[256];
        snprintf(b, sizeof b, "%s{\"id\":%u,\"w\":%u,\"h\":%u,\"d\":%u,\"fmt\":%u,\"usage\":%u,\"samples\":%u,\"mips\":%u,\"layers\":%u,\"swap\":%s}",
                 imgs.size() > 1 ? "," : "", im.id, im.w, im.h, im.d, im.fmt, im.usage, im.samples, im.mips, im.layers,
                 im.swap ? "true" : "false");
        imgs += b;
    }
    imgs += "]";
    std::string frames = "[";
    for (size_t i = 0; i < g_fa.frames.size(); i++) { if (i) frames += ","; frames += g_fa.frames[i]; }
    frames += "]";
    char hdr[768];
    snprintf(hdr, sizeof hdr,
             "{\"knob\":\"gray_frameaudit\",\"version\":1,\"pkg\":\"%s\",\"go\":%ld,\"arm_frame\":%llu,"
             "\"swapchain\":{\"w\":%u,\"h\":%u,\"fmt\":%u},\"ts_enabled\":%s,\"ts_period_ns\":%.6f,\"ts_slots\":[%u,%u],"
             "\"ts_available\":%u,\"diag\":{\"cb_unseen\":%llu,\"ev_overflow\":%llu,\"submit2\":%llu,\"dyn_passes\":%llu,"
             "\"sec_merged\":%llu,\"ts_skipped\":%llu,\"q_outside\":%llu,\"q_copies\":%llu,\"tmpl_updates\":%llu,\"push_writes\":%llu},",
             self_pkg().c_str(), g_fa.go, (unsigned long long)g_fa.arm_frame, g_swap_w, g_swap_h, g_swap_fmt,
             g_frameaudit_ts ? "true" : "false", (double)g_fa.period, g_fa.ts_lo, g_fa.ts_next, avail,
             (unsigned long long)g_fa.cb_unseen, (unsigned long long)g_fa.ev_overflow, (unsigned long long)g_fa.submit2,
             (unsigned long long)g_fa.dyn_passes, (unsigned long long)g_fa.sec_merged, (unsigned long long)g_fa.ts_skipped,
             (unsigned long long)g_fa.q_outside, (unsigned long long)g_fa.q_copies,
             (unsigned long long)g_fa.tmpl_updates, (unsigned long long)g_fa.push_writes);
    std::string json = std::string(hdr) + "\"images\":" + imgs + ",\"ts\":" + ts + ",\"frames\":" + frames + "}";
    std::string pkg = self_pkg();
    const std::string cands[] = {
        "/storage/emulated/0/Android/data/" + pkg + "/files/knobs_frameaudit.json",
        "/data/data/" + pkg + "/knobs_frameaudit.json",
        "/data/local/tmp/knobs_frameaudit." + pkg + ".json",
    };
    for (const auto& path : cands) {
        FILE* o = fopen(path.c_str(), "w");
        if (!o) continue;
        fwrite(json.data(), 1, json.size(), o);
        fputc('\n', o);
        fflush(o); fsync(fileno(o)); fclose(o);
        RK_LOG("frameaudit go=%ld 落盘 %s (%zu 字节, %zu 帧, 时间戳 %u 个)", g_fa.go, path.c_str(), json.size(),
               g_fa.frames.size(), avail);
        return;
    }
    RK_LOG("frameaudit: 落盘失败 (三个候选路径都写不进)");
}

static void fa_on_present(const DevDisp& d, uint64_t f, const VkPresentInfoKHR* pi) {
    if (!g_fa.arm_frame && f % 30 == 0) {
        char v[PROP_VALUE_MAX] = {0};
        __system_property_get("debug.knobs.frameaudit_go", v);
        long go = atol(v);
        if (go > 0 && go != g_fa.go) {
            std::lock_guard<std::mutex> lk(g_mtx);
            g_fa.go = go; g_fa.arm_frame = f; g_fa.frames.clear(); g_fa.imgs.clear(); g_fa.cur.clear();
            g_fa.cap_done = 0; g_fa.capturing = false;
            g_fa.ts_armed = false;
            if (g_frameaudit_ts) {
                if (g_fa.pool == VK_NULL_HANDLE && d.CreateQueryPool) {
                    VkQueryPoolCreateInfo qci{VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO};
                    qci.queryType = VK_QUERY_TYPE_TIMESTAMP; qci.queryCount = FA_TS_CAP;
                    if (d.CreateQueryPool(d.dev, &qci, nullptr, &g_fa.pool) != VK_SUCCESS) g_fa.pool = VK_NULL_HANDLE;
                    g_fa.pool_dev = d.dev; g_fa.period = d.ts_period_ns;
                }
                g_fa.ts_lo = g_fa.ts_next;
                g_fa.ts_armed = g_fa.pool != VK_NULL_HANDLE && g_fa.ts_next + 4096 <= FA_TS_CAP;
            }
            RK_LOG("frameaudit 武装 go=%ld frame=%llu ts=%d", go, (unsigned long long)f, (int)g_fa.ts_armed);
        }
    }
    if (!g_fa.arm_frame) return;
    uint64_t k = f - g_fa.arm_frame;
    std::lock_guard<std::mutex> lk(g_mtx);
    if (k >= FA_TS_FRAMES) g_fa.ts_armed = false;
    if (g_fa.capturing) {
        // 帧边界: 本帧的提交到此为止
        uint32_t pid = 0;
        if (pi && pi->swapchainCount && pi->pImageIndices) {
            auto si = g_swap_imgs.find(pi->pSwapchains[0]);
            if (si != g_swap_imgs.end() && pi->pImageIndices[0] < si->second.size()) {
                auto ii = g_img.find(si->second[pi->pImageIndices[0]]);
                if (ii != g_img.end()) { pid = ii->second.id; fa_note_img(pid); }
            }
        }
        g_fa.frames.push_back("{\"frame\":" + std::to_string(f) + ",\"present\":" + std::to_string(pid)
                              + ",\"submits\":[" + g_fa.cur + "]}");
        g_fa.cur.clear();
        if (++g_fa.cap_done >= FA_CAP_N) g_fa.capturing = false;
    } else if (k == FA_CAP_DELAY && g_fa.cap_done == 0) {
        g_fa.capturing = true; g_fa.cur.clear();
    }
    if (k >= FA_READ_DELAY) { fa_write_file(d); g_fa.arm_frame = 0; }
}

static VKAPI_ATTR void VKAPI_CALL rk_CmdEndRenderPass(VkCommandBuffer cb) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdEndRenderPass) { RK_LOG_ONCE("CmdEndRenderPass: 无下层"); return; }
    d.CmdEndRenderPass(cb);
    std::lock_guard<std::mutex> lk(g_mtx);
    if (FaEv* e = fa_open(cb)) e->ts1 = fa_ts(cb, d);
    auto it = g_facb.find(cb); if (it != g_facb.end()) it->second.open = -1;
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdEndRenderPass2(VkCommandBuffer cb, const VkSubpassEndInfo* ei) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdEndRenderPass2) { RK_LOG_ONCE("CmdEndRenderPass2: 无下层"); return; }
    d.CmdEndRenderPass2(cb, ei);
    std::lock_guard<std::mutex> lk(g_mtx);
    if (FaEv* e = fa_open(cb)) e->ts1 = fa_ts(cb, d);
    auto it = g_facb.find(cb); if (it != g_facb.end()) it->second.open = -1;
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdNextSubpass(VkCommandBuffer cb, VkSubpassContents c) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdNextSubpass) { RK_LOG_ONCE("CmdNextSubpass: 无下层"); return; }
    { std::lock_guard<std::mutex> lk(g_mtx); if (FaEv* e = fa_open(cb)) e->subpasses++; }
    d.CmdNextSubpass(cb, c);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdNextSubpass2(VkCommandBuffer cb, const VkSubpassBeginInfo* bi, const VkSubpassEndInfo* ei) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdNextSubpass2) { RK_LOG_ONCE("CmdNextSubpass2: 无下层"); return; }
    { std::lock_guard<std::mutex> lk(g_mtx); if (FaEv* e = fa_open(cb)) e->subpasses++; }
    d.CmdNextSubpass2(cb, bi, ei);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdBeginRendering(VkCommandBuffer cb, const VkRenderingInfo* ri) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdBeginRendering) { RK_LOG_ONCE("CmdBeginRendering: 无下层"); return; }
    if (ri) {
        std::lock_guard<std::mutex> lk(g_mtx);
        g_fa.dyn_passes++;
        FaEv e; e.kind = FA_PASS; e.dyn = true; e.w = ri->renderArea.extent.width; e.h = ri->renderArea.extent.height;
        auto add = [&](const VkRenderingAttachmentInfo* a, uint32_t role) {
            if (!a || a->imageView == VK_NULL_HANDLE) return;
            FaAtt x; x.r = fa_view_res(a->imageView); x.load = a->loadOp; x.store = a->storeOp; x.role = role;
            x.init = a->imageLayout; x.fin = a->imageLayout;
            auto ii = g_img_by_id.find(x.r.img); if (ii != g_img_by_id.end()) { x.fmt = ii->second.fmt; x.samples = ii->second.samples; }
            e.att.push_back(x);
            if (a->resolveImageView != VK_NULL_HANDLE) {
                FaAtt y; y.r = fa_view_res(a->resolveImageView); y.load = VK_ATTACHMENT_LOAD_OP_DONT_CARE;
                y.store = VK_ATTACHMENT_STORE_OP_STORE; y.role = 2;
                auto ij = g_img_by_id.find(y.r.img); if (ij != g_img_by_id.end()) { y.fmt = ij->second.fmt; y.samples = ij->second.samples; }
                e.att.push_back(y);
            }
        };
        for (uint32_t i = 0; i < ri->colorAttachmentCount; i++) add(&ri->pColorAttachments[i], 0);
        add(ri->pDepthAttachment, 1);
        if (ri->pStencilAttachment && (!ri->pDepthAttachment || ri->pStencilAttachment->imageView != ri->pDepthAttachment->imageView))
            add(ri->pStencilAttachment, 1);
        else if (ri->pStencilAttachment && !e.att.empty()) {
            e.att.back().sload = ri->pStencilAttachment->loadOp; e.att.back().sstore = ri->pStencilAttachment->storeOp;
        }
        FaCb& c = g_facb[cb];
        for (auto s : c.gfx) if (s) fa_add_set(e, s);
        for (auto& pv : c.push_gfx) fa_add_view(e, pv.first, pv.second);
        e.ts0 = fa_ts(cb, d);
        if (fa_push(cb, std::move(e))) c.open = (int)c.ev.size() - 1;
    }
    d.CmdBeginRendering(cb, ri);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdEndRendering(VkCommandBuffer cb) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdEndRendering) { RK_LOG_ONCE("CmdEndRendering: 无下层"); return; }
    d.CmdEndRendering(cb);
    std::lock_guard<std::mutex> lk(g_mtx);
    if (FaEv* e = fa_open(cb)) e->ts1 = fa_ts(cb, d);
    auto it = g_facb.find(cb); if (it != g_facb.end()) it->second.open = -1;
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdDrawIndirect(VkCommandBuffer cb, VkBuffer b, VkDeviceSize o, uint32_t n, uint32_t st) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdDrawIndirect) { RK_LOG_ONCE("CmdDrawIndirect: 无下层"); return; }
    note_draw(cb, 1, 2, 0); d.CmdDrawIndirect(cb, b, o, n, st);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdDrawIndexedIndirect(VkCommandBuffer cb, VkBuffer b, VkDeviceSize o, uint32_t n, uint32_t st) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdDrawIndexedIndirect) { RK_LOG_ONCE("CmdDrawIndexedIndirect: 无下层"); return; }
    note_draw(cb, 1, 2, 0); d.CmdDrawIndexedIndirect(cb, b, o, n, st);
}
static void fa_dispatch(VkCommandBuffer cb, uint32_t x, uint32_t y, uint32_t z, bool ind) {
    std::lock_guard<std::mutex> lk(g_mtx);
    FaEv e; e.kind = FA_DISPATCH; e.gx = x; e.gy = y; e.gz = z; e.indirect = ind;
    FaCb& c = g_facb[cb];
    for (auto s : c.comp) if (s) fa_add_set(e, s);
    for (auto& pv : c.push_comp) fa_add_view(e, pv.first, pv.second);
    fa_push(cb, std::move(e));
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdDispatch(VkCommandBuffer cb, uint32_t x, uint32_t y, uint32_t z) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdDispatch) { RK_LOG_ONCE("CmdDispatch: 无下层"); return; }
    fa_dispatch(cb, x, y, z, false); d.CmdDispatch(cb, x, y, z);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdDispatchIndirect(VkCommandBuffer cb, VkBuffer b, VkDeviceSize o) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdDispatchIndirect) { RK_LOG_ONCE("CmdDispatchIndirect: 无下层"); return; }
    fa_dispatch(cb, 0, 0, 0, true); d.CmdDispatchIndirect(cb, b, o);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdCopyImage(VkCommandBuffer cb, VkImage si, VkImageLayout sl, VkImage di,
                                                  VkImageLayout dl, uint32_t n, const VkImageCopy* r) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdCopyImage) { RK_LOG_ONCE("CmdCopyImage: 无下层"); return; }
    if (n && r) {
        std::lock_guard<std::mutex> lk(g_mtx);
        FaEv e; e.kind = FA_COPY_IMG; e.regions = n;
        e.src = fa_img_res(si, r[0].srcSubresource.mipLevel, 1, r[0].srcSubresource.baseArrayLayer, r[0].srcSubresource.layerCount);
        e.dst = fa_img_res(di, r[0].dstSubresource.mipLevel, 1, r[0].dstSubresource.baseArrayLayer, r[0].dstSubresource.layerCount);
        e.ex[0] = r[0].extent.width; e.ex[1] = r[0].extent.height; e.ex[2] = r[0].extent.depth;
        for (uint32_t i = 0; i < n; i++) e.texels += (uint64_t)r[i].extent.width * r[i].extent.height * r[i].extent.depth * r[i].srcSubresource.layerCount;
        fa_push(cb, std::move(e));
    }
    d.CmdCopyImage(cb, si, sl, di, dl, n, r);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdCopyBuffer(VkCommandBuffer cb, VkBuffer s, VkBuffer t, uint32_t n, const VkBufferCopy* r) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdCopyBuffer) { RK_LOG_ONCE("CmdCopyBuffer: 无下层"); return; }
    if (n && r) {
        std::lock_guard<std::mutex> lk(g_mtx);
        FaEv e; e.kind = FA_COPY_BUF; e.regions = n;
        for (uint32_t i = 0; i < n; i++) e.bytes += r[i].size;
        fa_push(cb, std::move(e));
    }
    d.CmdCopyBuffer(cb, s, t, n, r);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdCopyBufferToImage(VkCommandBuffer cb, VkBuffer s, VkImage di, VkImageLayout dl,
                                                          uint32_t n, const VkBufferImageCopy* r) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdCopyBufferToImage) { RK_LOG_ONCE("CmdCopyBufferToImage: 无下层"); return; }
    if (n && r) {
        std::lock_guard<std::mutex> lk(g_mtx);
        FaEv e; e.kind = FA_BUF2IMG; e.regions = n;
        e.dst = fa_img_res(di, r[0].imageSubresource.mipLevel, 1, r[0].imageSubresource.baseArrayLayer, r[0].imageSubresource.layerCount);
        e.ex[0] = r[0].imageExtent.width; e.ex[1] = r[0].imageExtent.height; e.ex[2] = r[0].imageExtent.depth;
        for (uint32_t i = 0; i < n; i++) e.texels += (uint64_t)r[i].imageExtent.width * r[i].imageExtent.height * r[i].imageExtent.depth * r[i].imageSubresource.layerCount;
        fa_push(cb, std::move(e));
    }
    d.CmdCopyBufferToImage(cb, s, di, dl, n, r);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdCopyImageToBuffer(VkCommandBuffer cb, VkImage si, VkImageLayout sl, VkBuffer t,
                                                          uint32_t n, const VkBufferImageCopy* r) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdCopyImageToBuffer) { RK_LOG_ONCE("CmdCopyImageToBuffer: 无下层"); return; }
    if (n && r) {
        std::lock_guard<std::mutex> lk(g_mtx);
        FaEv e; e.kind = FA_IMG2BUF; e.regions = n;
        e.src = fa_img_res(si, r[0].imageSubresource.mipLevel, 1, r[0].imageSubresource.baseArrayLayer, r[0].imageSubresource.layerCount);
        e.ex[0] = r[0].imageExtent.width; e.ex[1] = r[0].imageExtent.height; e.ex[2] = r[0].imageExtent.depth;
        for (uint32_t i = 0; i < n; i++) e.texels += (uint64_t)r[i].imageExtent.width * r[i].imageExtent.height * r[i].imageExtent.depth * r[i].imageSubresource.layerCount;
        fa_push(cb, std::move(e));
    }
    d.CmdCopyImageToBuffer(cb, si, sl, t, n, r);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdResolveImage(VkCommandBuffer cb, VkImage si, VkImageLayout sl, VkImage di,
                                                     VkImageLayout dl, uint32_t n, const VkImageResolve* r) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdResolveImage) { RK_LOG_ONCE("CmdResolveImage: 无下层"); return; }
    if (n && r) {
        std::lock_guard<std::mutex> lk(g_mtx);
        FaEv e; e.kind = FA_RESOLVE; e.regions = n;
        e.src = fa_img_res(si, r[0].srcSubresource.mipLevel, 1, r[0].srcSubresource.baseArrayLayer, r[0].srcSubresource.layerCount);
        e.dst = fa_img_res(di, r[0].dstSubresource.mipLevel, 1, r[0].dstSubresource.baseArrayLayer, r[0].dstSubresource.layerCount);
        e.ex[0] = r[0].extent.width; e.ex[1] = r[0].extent.height; e.ex[2] = r[0].extent.depth;
        for (uint32_t i = 0; i < n; i++) e.texels += (uint64_t)r[i].extent.width * r[i].extent.height * r[i].extent.depth;
        fa_push(cb, std::move(e));
    }
    d.CmdResolveImage(cb, si, sl, di, dl, n, r);
}
static void fa_clear_img(VkCommandBuffer cb, uint8_t kind, VkImage img, uint32_t n, const VkImageSubresourceRange* rg) {
    if (!n || !rg) return;
    std::lock_guard<std::mutex> lk(g_mtx);
    FaEv e; e.kind = kind; e.regions = n;
    e.dst = fa_img_res(img, rg[0].baseMipLevel, rg[0].levelCount, rg[0].baseArrayLayer, rg[0].layerCount);
    auto ii = g_img.find(img);
    if (ii != g_img.end()) { e.ex[0] = ii->second.w; e.ex[1] = ii->second.h; e.ex[2] = ii->second.d; }
    fa_push(cb, std::move(e));
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdClearColorImage(VkCommandBuffer cb, VkImage img, VkImageLayout l,
                                                        const VkClearColorValue* c, uint32_t n, const VkImageSubresourceRange* rg) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdClearColorImage) { RK_LOG_ONCE("CmdClearColorImage: 无下层"); return; }
    fa_clear_img(cb, FA_CLEAR_COLOR, img, n, rg);
    d.CmdClearColorImage(cb, img, l, c, n, rg);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdClearDepthStencilImage(VkCommandBuffer cb, VkImage img, VkImageLayout l,
                                                               const VkClearDepthStencilValue* c, uint32_t n, const VkImageSubresourceRange* rg) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdClearDepthStencilImage) { RK_LOG_ONCE("CmdClearDepthStencilImage: 无下层"); return; }
    fa_clear_img(cb, FA_CLEAR_DS, img, n, rg);
    d.CmdClearDepthStencilImage(cb, img, l, c, n, rg);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdClearAttachments(VkCommandBuffer cb, uint32_t na, const VkClearAttachment* a,
                                                         uint32_t nr, const VkClearRect* r) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdClearAttachments) { RK_LOG_ONCE("CmdClearAttachments: 无下层"); return; }
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        if (FaEv* e = fa_open(cb)) {
            e->clear_att += na;
            bool full = false;
            for (uint32_t i = 0; i < nr && r; i++)
                if (r[i].rect.offset.x == 0 && r[i].rect.offset.y == 0 &&
                    (e->w == 0 || (r[i].rect.extent.width >= e->w && r[i].rect.extent.height >= e->h))) full = true;
            if (full) e->clear_att_full += na;
        }
    }
    d.CmdClearAttachments(cb, na, a, nr, r);
}
static void fa_discard(VkCommandBuffer cb, VkImage img, const VkImageSubresourceRange& rg) {   // 持锁
    FaEv e; e.kind = FA_DISCARD;
    e.dst = fa_img_res(img, rg.baseMipLevel, rg.levelCount, rg.baseArrayLayer, rg.layerCount);
    if (e.dst.img) fa_push(cb, std::move(e));
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdPipelineBarrier(VkCommandBuffer cb, VkPipelineStageFlags ss, VkPipelineStageFlags ds,
    VkDependencyFlags df, uint32_t nm, const VkMemoryBarrier* m, uint32_t nb, const VkBufferMemoryBarrier* b,
    uint32_t ni, const VkImageMemoryBarrier* im) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdPipelineBarrier) { RK_LOG_ONCE("CmdPipelineBarrier: 无下层"); return; }
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        FaCb& c = g_facb[cb];
        c.barriers++;
        // oldLayout=UNDEFINED = 应用声明「旧内容不要了」: 之前对它的 STORE 就白存了
        if (c.open < 0 && im)
            for (uint32_t i = 0; i < ni; i++)
                if (im[i].oldLayout == VK_IMAGE_LAYOUT_UNDEFINED) fa_discard(cb, im[i].image, im[i].subresourceRange);
    }
    d.CmdPipelineBarrier(cb, ss, ds, df, nm, m, nb, b, ni, im);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdPipelineBarrier2(VkCommandBuffer cb, const VkDependencyInfo* di) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdPipelineBarrier2) { RK_LOG_ONCE("CmdPipelineBarrier2: 无下层"); return; }
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        FaCb& c = g_facb[cb];
        c.barriers++;
        if (c.open < 0 && di && di->pImageMemoryBarriers)
            for (uint32_t i = 0; i < di->imageMemoryBarrierCount; i++)
                if (di->pImageMemoryBarriers[i].oldLayout == VK_IMAGE_LAYOUT_UNDEFINED)
                    fa_discard(cb, di->pImageMemoryBarriers[i].image, di->pImageMemoryBarriers[i].subresourceRange);
    }
    d.CmdPipelineBarrier2(cb, di);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdFillBuffer(VkCommandBuffer cb, VkBuffer b, VkDeviceSize o, VkDeviceSize sz, uint32_t v) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdFillBuffer) { RK_LOG_ONCE("CmdFillBuffer: 无下层"); return; }
    { std::lock_guard<std::mutex> lk(g_mtx); FaEv e; e.kind = FA_FILL_BUF; e.bytes = sz; fa_push(cb, std::move(e)); }
    d.CmdFillBuffer(cb, b, o, sz, v);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdUpdateBuffer(VkCommandBuffer cb, VkBuffer b, VkDeviceSize o, VkDeviceSize sz, const void* p) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdUpdateBuffer) { RK_LOG_ONCE("CmdUpdateBuffer: 无下层"); return; }
    { std::lock_guard<std::mutex> lk(g_mtx); FaEv e; e.kind = FA_FILL_BUF; e.bytes = sz; fa_push(cb, std::move(e)); }
    d.CmdUpdateBuffer(cb, b, o, sz, p);
}
// ── 描述符模板 / push 描述符 ──
struct TmplEntry { uint32_t binding, elem, count, type; size_t offset, stride; };
static std::map<VkDescriptorUpdateTemplate, std::vector<TmplEntry>> g_tmpl;
static VKAPI_ATTR VkResult VKAPI_CALL rk_CreateDescriptorUpdateTemplate(VkDevice dev, const VkDescriptorUpdateTemplateCreateInfo* ci,
    const VkAllocationCallbacks* a, VkDescriptorUpdateTemplate* out) {
    DevDisp d; if (!dev_of(key(dev), &d) || !d.CreateDescriptorUpdateTemplate) { RK_LOG_ONCE("CreateDescriptorUpdateTemplate: 无下层"); return VK_ERROR_INITIALIZATION_FAILED; }
    VkResult r = d.CreateDescriptorUpdateTemplate(dev, ci, a, out);
    if (r == VK_SUCCESS && ci) {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto& v = g_tmpl[*out];
        for (uint32_t i = 0; i < ci->descriptorUpdateEntryCount; i++) {
            const auto& en = ci->pDescriptorUpdateEntries[i];
            if (is_image_desc(en.descriptorType))
                v.push_back({en.dstBinding, en.dstArrayElement, en.descriptorCount, (uint32_t)en.descriptorType, en.offset, en.stride});
        }
    }
    return r;
}
static VKAPI_ATTR void VKAPI_CALL rk_DestroyDescriptorUpdateTemplate(VkDevice dev, VkDescriptorUpdateTemplate t, const VkAllocationCallbacks* a) {
    DevDisp d; if (!dev_of(key(dev), &d) || !d.DestroyDescriptorUpdateTemplate) { RK_LOG_ONCE("DestroyDescriptorUpdateTemplate: 无下层"); return; }
    { std::lock_guard<std::mutex> lk(g_mtx); g_tmpl.erase(t); }
    d.DestroyDescriptorUpdateTemplate(dev, t, a);
}
static VKAPI_ATTR void VKAPI_CALL rk_UpdateDescriptorSetWithTemplate(VkDevice dev, VkDescriptorSet set, VkDescriptorUpdateTemplate t, const void* data) {
    DevDisp d; if (!dev_of(key(dev), &d) || !d.UpdateDescriptorSetWithTemplate) { RK_LOG_ONCE("UpdateDescriptorSetWithTemplate: 无下层"); return; }
    if (data) {
        std::lock_guard<std::mutex> lk(g_mtx);
        g_set_gen[set]++;
        g_fa.tmpl_updates++;
        auto it = g_tmpl.find(t);
        if (it != g_tmpl.end())
            for (const auto& en : it->second)
                for (uint32_t j = 0; j < en.count; j++) {
                    auto* ii = reinterpret_cast<const VkDescriptorImageInfo*>(static_cast<const char*>(data) + en.offset + j * en.stride);
                    note_image_desc(set, en.binding, en.elem + j, en.type, ii->imageView, ii->sampler);
                }
    }
    d.UpdateDescriptorSetWithTemplate(dev, set, t, data);
}
// push: 图直接挂到 cb 的绑定点上; pass 开着就同时记进当前 pass 的读
static void note_push(VkCommandBuffer cb, VkPipelineBindPoint bp, VkImageView v, uint32_t type) {   // 持锁
    if (v == VK_NULL_HANDLE) return;
    g_fa.push_writes++;
    FaCb& c = g_facb[cb];
    bool st = type == VK_DESCRIPTOR_TYPE_STORAGE_IMAGE;
    auto& lst = bp == VK_PIPELINE_BIND_POINT_COMPUTE ? c.push_comp : c.push_gfx;
    if (lst.size() < 64) lst.push_back({v, st});
    if (bp == VK_PIPELINE_BIND_POINT_GRAPHICS) if (FaEv* e = fa_open(cb)) fa_add_view(*e, v, st);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdPushDescriptorSetKHR(VkCommandBuffer cb, VkPipelineBindPoint bp, VkPipelineLayout pl,
    uint32_t set, uint32_t n, const VkWriteDescriptorSet* w) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdPushDescriptorSetKHR) { RK_LOG_ONCE("CmdPushDescriptorSetKHR: 无下层"); return; }
    if (w) {
        std::lock_guard<std::mutex> lk(g_mtx);
        for (uint32_t i = 0; i < n; i++)
            if (w[i].pImageInfo && is_image_desc(w[i].descriptorType))
                for (uint32_t j = 0; j < w[i].descriptorCount; j++) note_push(cb, bp, w[i].pImageInfo[j].imageView, w[i].descriptorType);
    }
    d.CmdPushDescriptorSetKHR(cb, bp, pl, set, n, w);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdPushDescriptorSetWithTemplateKHR(VkCommandBuffer cb, VkDescriptorUpdateTemplate t,
    VkPipelineLayout pl, uint32_t set, const void* data) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdPushDescriptorSetWithTemplateKHR) { RK_LOG_ONCE("CmdPushDescriptorSetWithTemplateKHR: 无下层"); return; }
    if (data) {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto it = g_tmpl.find(t);
        // 模板没记绑定点 (push 模板的 pipelineBindPoint 在 CreateInfo 里, 这里按图形算: 保守地多算读)
        if (it != g_tmpl.end())
            for (const auto& en : it->second)
                for (uint32_t j = 0; j < en.count; j++) {
                    auto* ii = reinterpret_cast<const VkDescriptorImageInfo*>(static_cast<const char*>(data) + en.offset + j * en.stride);
                    note_push(cb, VK_PIPELINE_BIND_POINT_GRAPHICS, ii->imageView, en.type);
                }
    }
    d.CmdPushDescriptorSetWithTemplateKHR(cb, t, pl, set, data);
}
static VKAPI_ATTR VkResult VKAPI_CALL rk_CreateQueryPool(VkDevice dev, const VkQueryPoolCreateInfo* ci,
                                                         const VkAllocationCallbacks* a, VkQueryPool* out) {
    DevDisp d; if (!dev_of(key(dev), &d) || !d.CreateQueryPool) { RK_LOG_ONCE("CreateQueryPool: 无下层"); return VK_ERROR_INITIALIZATION_FAILED; }
    VkResult r = d.CreateQueryPool(dev, ci, a, out);
    if (r == VK_SUCCESS && ci) { std::lock_guard<std::mutex> lk(g_mtx); g_qpool_type[*out] = (uint32_t)ci->queryType; }
    return r;
}
static VKAPI_ATTR void VKAPI_CALL rk_DestroyQueryPool(VkDevice dev, VkQueryPool p, const VkAllocationCallbacks* a) {
    DevDisp d; if (!dev_of(key(dev), &d) || !d.DestroyQueryPool) { RK_LOG_ONCE("DestroyQueryPool: 无下层"); return; }
    { std::lock_guard<std::mutex> lk(g_mtx); g_qpool_type.erase(p); }
    d.DestroyQueryPool(dev, p, a);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdBeginQuery(VkCommandBuffer cb, VkQueryPool p, uint32_t q, VkQueryControlFlags f) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdBeginQuery) { RK_LOG_ONCE("CmdBeginQuery: 无下层"); return; }
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto it = g_qpool_type.find(p);
        bool occ = it != g_qpool_type.end() && it->second == VK_QUERY_TYPE_OCCLUSION;
        if (FaEv* e = fa_open(cb)) { if (occ) e->q_occ++; else e->q_other++; }
        else g_fa.q_outside++;
    }
    d.CmdBeginQuery(cb, p, q, f);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdEndQuery(VkCommandBuffer cb, VkQueryPool p, uint32_t q) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdEndQuery) { RK_LOG_ONCE("CmdEndQuery: 无下层"); return; }
    d.CmdEndQuery(cb, p, q);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdCopyQueryPoolResults(VkCommandBuffer cb, VkQueryPool p, uint32_t first, uint32_t n,
    VkBuffer b, VkDeviceSize o, VkDeviceSize st, VkQueryResultFlags f) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdCopyQueryPoolResults) { RK_LOG_ONCE("CmdCopyQueryPoolResults: 无下层"); return; }
    { std::lock_guard<std::mutex> lk(g_mtx); g_fa.q_copies++; }
    d.CmdCopyQueryPoolResults(cb, p, first, n, b, o, st, f);
}
static VKAPI_ATTR VkResult VKAPI_CALL rk_QueueSubmit2(VkQueue q, uint32_t n, const VkSubmitInfo2* si, VkFence fence) {
    DevDisp d; if (!dev_of(key(q), &d) || !d.QueueSubmit2) { RK_LOG_ONCE("QueueSubmit2: 无下层"); return VK_ERROR_INITIALIZATION_FAILED; }
    if (si) {
        std::lock_guard<std::mutex> lk(g_mtx);
        g_fa.submit2++;
        if (g_fa.capturing)
            for (uint32_t i = 0; i < n; i++) {
                std::vector<VkCommandBuffer> cbs;
                for (uint32_t j = 0; j < si[i].commandBufferInfoCount; j++) cbs.push_back(si[i].pCommandBufferInfos[j].commandBuffer);
                fa_capture_submit(key(q), (uint32_t)cbs.size(), cbs.data());
            }
    }
    return d.QueueSubmit2(q, n, si, fence);
}
static VKAPI_ATTR VkResult VKAPI_CALL rk_GetSwapchainImagesKHR(VkDevice dev, VkSwapchainKHR sc, uint32_t* pn, VkImage* imgs) {
    DevDisp d; if (!dev_of(key(dev), &d) || !d.GetSwapchainImagesKHR) { RK_LOG_ONCE("GetSwapchainImagesKHR: 无下层"); return VK_ERROR_INITIALIZATION_FAILED; }
    VkResult r = d.GetSwapchainImagesKHR(dev, sc, pn, imgs);
    if ((r == VK_SUCCESS || r == VK_INCOMPLETE) && imgs && pn) {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto& v = g_swap_imgs[sc];
        v.assign(imgs, imgs + *pn);
        for (uint32_t i = 0; i < *pn; i++) {
            if (g_img.count(imgs[i])) continue;
            ImgInfo ii{g_swap_w, g_swap_h, g_swap_fmt, (uint32_t)VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT};
            ii.id = ++g_img_seq; ii.swap = true;
            g_img[imgs[i]] = ii; g_img_by_id[ii.id] = ii;
        }
    }
    return r;
}

// begins 数的是录制, 不是执行; 真正的"这个 pass 每帧都在跑"要看提交 (评审 #4)。
// 提交时把每条 cb 录过的 pass 记进本帧集合, present 时统一折算成 frames_seen。
static VKAPI_ATTR VkResult VKAPI_CALL rk_QueueSubmit(VkQueue q, uint32_t n, const VkSubmitInfo* si, VkFence fence) {
    DevDisp d;
    if (!dev_of(key(q), &d) || !d.QueueSubmit) { RK_LOG_ONCE("QueueSubmit: 无下层"); return VK_ERROR_INITIALIZATION_FAILED; }
    if (g_dump_passes && si) {
        std::lock_guard<std::mutex> lk(g_mtx);
        for (uint32_t i = 0; i < n; i++)
            for (uint32_t j = 0; j < si[i].commandBufferCount; j++) {
                auto it = g_cb_passes.find(si[i].pCommandBuffers[j]);
                if (it != g_cb_passes.end())
                    for (uint64_t k : it->second) g_frame_seen.insert(k);
            }
        if (g_frameaudit && g_fa.capturing)
            for (uint32_t i = 0; i < n; i++) fa_capture_submit(key(q), si[i].commandBufferCount, si[i].pCommandBuffers);
    }
    return d.QueueSubmit(q, n, si, fence);
}

// cb 被释放时摘掉它的全部 per-cb 状态 (评审 #10: 这些 map 跑在游戏进程里, 只增不减会无限涨)
static VKAPI_ATTR void VKAPI_CALL rk_FreeCommandBuffers(VkDevice dev, VkCommandPool pool, uint32_t n, const VkCommandBuffer* cbs) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.FreeCommandBuffers) { RK_LOG_ONCE("FreeCommandBuffers: 无下层"); return; }
    if (g_dump_passes && cbs) {
        std::lock_guard<std::mutex> lk(g_mtx);
        for (uint32_t i = 0; i < n; i++) {
            g_cb_cur.erase(cbs[i]); g_cb_sets.erase(cbs[i]); g_cb_draw_idx.erase(cbs[i]);
            g_cb_prev_att.erase(cbs[i]); g_cb_cur_is_swap.erase(cbs[i]); g_cb_passes.erase(cbs[i]);
            g_facb.erase(cbs[i]);
        }
    }
    d.FreeCommandBuffers(dev, pool, n, cbs);
}

// 同理: image / view / framebuffer 销毁时摘表 (评审 #10)。
// 注意 g_set_views 里可能还指着已销毁的 view —— 不级联清 (view 表先摘, 溯源时 g_view/g_img
// 查不到自然跳过, 不会读到假数据)。
static VKAPI_ATTR void VKAPI_CALL rk_DestroyFramebuffer(VkDevice dev, VkFramebuffer fb, const VkAllocationCallbacks* a) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.DestroyFramebuffer) { RK_LOG_ONCE("DestroyFramebuffer: 无下层"); return; }
    { std::lock_guard<std::mutex> lk(g_mtx); g_fb_info.erase(fb); g_fb_att.erase(fb); }
    d.DestroyFramebuffer(dev, fb, a);
}
static VKAPI_ATTR void VKAPI_CALL rk_DestroyImage(VkDevice dev, VkImage img, const VkAllocationCallbacks* a) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.DestroyImage) { RK_LOG_ONCE("DestroyImage: 无下层"); return; }
    { std::lock_guard<std::mutex> lk(g_mtx);
      auto it = g_img.find(img); if (it != g_img.end()) { g_img_by_id.erase(it->second.id); g_img.erase(it); } }
    d.DestroyImage(dev, img, a);
}
static VKAPI_ATTR void VKAPI_CALL rk_DestroyImageView(VkDevice dev, VkImageView v, const VkAllocationCallbacks* a) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.DestroyImageView) { RK_LOG_ONCE("DestroyImageView: 无下层"); return; }
    { std::lock_guard<std::mutex> lk(g_mtx); g_view.erase(v); g_view_sub.erase(v); }
    d.DestroyImageView(dev, v, a);
}

static VKAPI_ATTR VkResult VKAPI_CALL rk_QueuePresentKHR(VkQueue q, const VkPresentInfoKHR* pi) {
    DevDisp d;
    if (!dev_of(key(q), &d) || !d.QueuePresentKHR) {
        RK_LOG_ONCE("QueuePresentKHR: 查不到下层函数");
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    uint64_t f = ++g_frames;
    if (g_storeop_live && f % 30 == 0) {
        bool on = read_bool_prop("debug.knobs.storeop_on");
        if (on != g_storeop_on.exchange(on)) RK_LOG("storeop 现场切换: %s (frame=%llu)", on ? "开" : "关", (unsigned long long)f);
    }
    if (g_frameaudit) fa_on_present(d, f, pi);
    if (g_dump_passes) {
        {
            // 本帧提交执行了哪些 pass -> frames_seen。录制数 (begins) 代替不了它:
            // 预录命令缓冲录一次跑几千帧 (评审 #4)。
            std::lock_guard<std::mutex> lk(g_mtx);
            for (uint64_t k : g_frame_seen) g_pass_stat[k].frames_seen++;
            g_frame_seen.clear();
        }
        // 挑一帧记有序序列: 等画面进入稳态再采, 避开加载期。
        // 1750 不是 300 的倍数 —— 采样帧与落盘帧撞在一起会让那次落盘的 frame_seq 必为空,
        // 而 marker 看起来正常 (评审 #5: 之前采样窗开在 1800 就踩了这个)。
        if (f == 1750 && !g_seq_done) { std::lock_guard<std::mutex> lk(g_mtx); g_frame_seq.clear(); g_sampling = true; }
        else if (f == 1751 && !g_seq_done) { std::lock_guard<std::mutex> lk(g_mtx); g_sampling = false; g_seq_done = true; }
    }
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
    RK_HOOK("vkCreateSwapchainKHR", rk_CreateSwapchainKHR);
    // 这几个只为 passdump 观测服务。passdump 没开就**不要挂** ——
    // vkCmdDraw 这种每帧上万次的热函数, 挂上等于给 loadop 的 A/B 两臂凭空加一层开销。
    if (g_dump_passes) {
        RK_HOOK("vkCmdDraw", rk_CmdDraw);
        RK_HOOK("vkCmdDrawIndexed", rk_CmdDrawIndexed);
        RK_HOOK("vkCmdBlitImage", rk_CmdBlitImage);
        RK_HOOK("vkCreateImage", rk_CreateImage);
        RK_HOOK("vkCreateImageView", rk_CreateImageView);
        RK_HOOK("vkUpdateDescriptorSets", rk_UpdateDescriptorSets);
        RK_HOOK("vkCmdBindDescriptorSets", rk_CmdBindDescriptorSets);
        RK_HOOK("vkCreateFramebuffer", rk_CreateFramebuffer);
        RK_HOOK("vkBeginCommandBuffer", rk_BeginCommandBuffer);
        RK_HOOK("vkCmdExecuteCommands", rk_CmdExecuteCommands);
        RK_HOOK("vkQueueSubmit", rk_QueueSubmit);
        RK_HOOK("vkFreeCommandBuffers", rk_FreeCommandBuffers);
        RK_HOOK("vkDestroyFramebuffer", rk_DestroyFramebuffer);
        RK_HOOK("vkDestroyImage", rk_DestroyImage);
        RK_HOOK("vkDestroyImageView", rk_DestroyImageView);
        RK_HOOK("vkCreateDescriptorSetLayout", rk_CreateDescriptorSetLayout);
        RK_HOOK("vkAllocateDescriptorSets", rk_AllocateDescriptorSets);
        RK_HOOK("vkDestroyDescriptorPool", rk_DestroyDescriptorPool);
    }
    if (g_frameaudit) {
        RK_HOOK("vkCmdEndRenderPass", rk_CmdEndRenderPass);
        RK_HOOK("vkCmdNextSubpass", rk_CmdNextSubpass);
        RK_HOOK("vkCmdDrawIndirect", rk_CmdDrawIndirect);
        RK_HOOK("vkCmdDrawIndexedIndirect", rk_CmdDrawIndexedIndirect);
        RK_HOOK("vkCmdDispatch", rk_CmdDispatch);
        RK_HOOK("vkCmdDispatchIndirect", rk_CmdDispatchIndirect);
        RK_HOOK("vkCmdCopyImage", rk_CmdCopyImage);
        RK_HOOK("vkCmdCopyBuffer", rk_CmdCopyBuffer);
        RK_HOOK("vkCmdCopyBufferToImage", rk_CmdCopyBufferToImage);
        RK_HOOK("vkCmdCopyImageToBuffer", rk_CmdCopyImageToBuffer);
        RK_HOOK("vkCmdResolveImage", rk_CmdResolveImage);
        RK_HOOK("vkCmdClearColorImage", rk_CmdClearColorImage);
        RK_HOOK("vkCmdClearDepthStencilImage", rk_CmdClearDepthStencilImage);
        RK_HOOK("vkCmdClearAttachments", rk_CmdClearAttachments);
        RK_HOOK("vkCmdPipelineBarrier", rk_CmdPipelineBarrier);
        RK_HOOK("vkCmdFillBuffer", rk_CmdFillBuffer);
        RK_HOOK("vkCmdUpdateBuffer", rk_CmdUpdateBuffer);
        RK_HOOK("vkGetSwapchainImagesKHR", rk_GetSwapchainImagesKHR);
        RK_HOOK("vkCreateQueryPool", rk_CreateQueryPool);
        RK_HOOK("vkDestroyQueryPool", rk_DestroyQueryPool);
        RK_HOOK("vkCmdBeginQuery", rk_CmdBeginQuery);
        RK_HOOK("vkCmdEndQuery", rk_CmdEndQuery);
        RK_HOOK("vkCmdCopyQueryPoolResults", rk_CmdCopyQueryPoolResults);
        RK_HOOK("vkCreateDescriptorUpdateTemplate", rk_CreateDescriptorUpdateTemplate);
        RK_HOOK("vkCreateDescriptorUpdateTemplateKHR", rk_CreateDescriptorUpdateTemplate);
        RK_HOOK("vkDestroyDescriptorUpdateTemplate", rk_DestroyDescriptorUpdateTemplate);
        RK_HOOK("vkDestroyDescriptorUpdateTemplateKHR", rk_DestroyDescriptorUpdateTemplate);
        RK_HOOK("vkUpdateDescriptorSetWithTemplate", rk_UpdateDescriptorSetWithTemplate);
        RK_HOOK("vkUpdateDescriptorSetWithTemplateKHR", rk_UpdateDescriptorSetWithTemplate);
    }
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
    RK_HOOK("vkCreateSwapchainKHR", rk_CreateSwapchainKHR);
    // 这几个只为 passdump 观测服务。passdump 没开就**不要挂** ——
    // vkCmdDraw 这种每帧上万次的热函数, 挂上等于给 loadop 的 A/B 两臂凭空加一层开销。
    if (g_dump_passes) {
        RK_HOOK("vkCmdDraw", rk_CmdDraw);
        RK_HOOK("vkCmdDrawIndexed", rk_CmdDrawIndexed);
        RK_HOOK("vkCmdBlitImage", rk_CmdBlitImage);
        RK_HOOK("vkCreateImage", rk_CreateImage);
        RK_HOOK("vkCreateImageView", rk_CreateImageView);
        RK_HOOK("vkUpdateDescriptorSets", rk_UpdateDescriptorSets);
        RK_HOOK("vkCmdBindDescriptorSets", rk_CmdBindDescriptorSets);
        RK_HOOK("vkCreateFramebuffer", rk_CreateFramebuffer);
        RK_HOOK("vkBeginCommandBuffer", rk_BeginCommandBuffer);
        RK_HOOK("vkCmdExecuteCommands", rk_CmdExecuteCommands);
        RK_HOOK("vkQueueSubmit", rk_QueueSubmit);
        RK_HOOK("vkFreeCommandBuffers", rk_FreeCommandBuffers);
        RK_HOOK("vkDestroyFramebuffer", rk_DestroyFramebuffer);
        RK_HOOK("vkDestroyImage", rk_DestroyImage);
        RK_HOOK("vkDestroyImageView", rk_DestroyImageView);
        RK_HOOK("vkCreateDescriptorSetLayout", rk_CreateDescriptorSetLayout);
        RK_HOOK("vkAllocateDescriptorSets", rk_AllocateDescriptorSets);
        RK_HOOK("vkDestroyDescriptorPool", rk_DestroyDescriptorPool);
    }
    if (g_frameaudit) {
        RK_HOOK("vkCmdEndRenderPass", rk_CmdEndRenderPass);
        RK_HOOK("vkCmdNextSubpass", rk_CmdNextSubpass);
        RK_HOOK("vkCmdDrawIndirect", rk_CmdDrawIndirect);
        RK_HOOK("vkCmdDrawIndexedIndirect", rk_CmdDrawIndexedIndirect);
        RK_HOOK("vkCmdDispatch", rk_CmdDispatch);
        RK_HOOK("vkCmdDispatchIndirect", rk_CmdDispatchIndirect);
        RK_HOOK("vkCmdCopyImage", rk_CmdCopyImage);
        RK_HOOK("vkCmdCopyBuffer", rk_CmdCopyBuffer);
        RK_HOOK("vkCmdCopyBufferToImage", rk_CmdCopyBufferToImage);
        RK_HOOK("vkCmdCopyImageToBuffer", rk_CmdCopyImageToBuffer);
        RK_HOOK("vkCmdResolveImage", rk_CmdResolveImage);
        RK_HOOK("vkCmdClearColorImage", rk_CmdClearColorImage);
        RK_HOOK("vkCmdClearDepthStencilImage", rk_CmdClearDepthStencilImage);
        RK_HOOK("vkCmdClearAttachments", rk_CmdClearAttachments);
        RK_HOOK("vkCmdPipelineBarrier", rk_CmdPipelineBarrier);
        RK_HOOK("vkCmdFillBuffer", rk_CmdFillBuffer);
        RK_HOOK("vkCmdUpdateBuffer", rk_CmdUpdateBuffer);
        RK_HOOK("vkGetSwapchainImagesKHR", rk_GetSwapchainImagesKHR);
        RK_HOOK("vkCreateQueryPool", rk_CreateQueryPool);
        RK_HOOK("vkDestroyQueryPool", rk_DestroyQueryPool);
        RK_HOOK("vkCmdBeginQuery", rk_CmdBeginQuery);
        RK_HOOK("vkCmdEndQuery", rk_CmdEndQuery);
        RK_HOOK("vkCmdCopyQueryPoolResults", rk_CmdCopyQueryPoolResults);
        RK_HOOK("vkCreateDescriptorUpdateTemplate", rk_CreateDescriptorUpdateTemplate);
        RK_HOOK("vkCreateDescriptorUpdateTemplateKHR", rk_CreateDescriptorUpdateTemplate);
        RK_HOOK("vkDestroyDescriptorUpdateTemplate", rk_DestroyDescriptorUpdateTemplate);
        RK_HOOK("vkDestroyDescriptorUpdateTemplateKHR", rk_DestroyDescriptorUpdateTemplate);
        RK_HOOK("vkUpdateDescriptorSetWithTemplate", rk_UpdateDescriptorSetWithTemplate);
        RK_HOOK("vkUpdateDescriptorSetWithTemplateKHR", rk_UpdateDescriptorSetWithTemplate);
    }
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
    if (g_frameaudit) {
        // 可选 (1.2/1.3/扩展) 命令: 底层有才挂, 没有就原样转发 (多半是空指针, 应用自己会判)
#define RK_OPT(n, field, fn) if (!strcmp(name, n)) return d.field ? reinterpret_cast<PFN_vkVoidFunction>(fn) : (d.gdpa ? d.gdpa(dev, name) : nullptr)
        RK_OPT("vkCmdEndRenderPass2", CmdEndRenderPass2, rk_CmdEndRenderPass2);
        RK_OPT("vkCmdEndRenderPass2KHR", CmdEndRenderPass2, rk_CmdEndRenderPass2);
        RK_OPT("vkCmdNextSubpass2", CmdNextSubpass2, rk_CmdNextSubpass2);
        RK_OPT("vkCmdNextSubpass2KHR", CmdNextSubpass2, rk_CmdNextSubpass2);
        RK_OPT("vkCmdBeginRendering", CmdBeginRendering, rk_CmdBeginRendering);
        RK_OPT("vkCmdBeginRenderingKHR", CmdBeginRendering, rk_CmdBeginRendering);
        RK_OPT("vkCmdEndRendering", CmdEndRendering, rk_CmdEndRendering);
        RK_OPT("vkCmdEndRenderingKHR", CmdEndRendering, rk_CmdEndRendering);
        RK_OPT("vkCmdPipelineBarrier2", CmdPipelineBarrier2, rk_CmdPipelineBarrier2);
        RK_OPT("vkCmdPipelineBarrier2KHR", CmdPipelineBarrier2, rk_CmdPipelineBarrier2);
        RK_OPT("vkQueueSubmit2", QueueSubmit2, rk_QueueSubmit2);
        RK_OPT("vkQueueSubmit2KHR", QueueSubmit2, rk_QueueSubmit2);
        RK_OPT("vkCmdPushDescriptorSetKHR", CmdPushDescriptorSetKHR, rk_CmdPushDescriptorSetKHR);
        RK_OPT("vkCmdPushDescriptorSetWithTemplateKHR", CmdPushDescriptorSetWithTemplateKHR, rk_CmdPushDescriptorSetWithTemplateKHR);
#undef RK_OPT
    }
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
