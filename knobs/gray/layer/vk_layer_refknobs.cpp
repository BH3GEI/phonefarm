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
#if defined(RK_LIFETIME_HOST_FIXTURE) && defined(__ANDROID__)
#error "Host lifetime contracts are forbidden in Android artifacts"
#endif
#include <vulkan/vulkan.h>
#include <android/log.h>
#include <atomic>
#include <cstdio>
#include <cstring>
#include <map>
#include <mutex>
#include <set>
#include <string>
#include <vector>
#include <utility>
#include <sys/system_properties.h>
#include <unistd.h>
#include "artifact_stats.h"
#ifdef RK_DYNAMIC_UPOP_BUILD
#include "artifact_metadata.h"
#ifndef RK_DYNAMIC_ARTIFACT_METADATA
#error "Dynamic builds require artifact metadata; no fixed-shader fallback"
#endif
#include "copy_spv.h"
static_assert(kUpopSpvLen > 0 && kUpopSpvLen % 4 == 0, "Dynamic candidate SPIR-V required");
#else
static const char kArtifactId[] = "legacy-fixed-upop";
static const char kCandidateSourceSha256[] = "";
static const char kCandidateSpirvSha256[] = "";
#if __has_include("copy_spv.h")
#include "copy_spv.h"
#else
static const unsigned char kCopySpv[] = {0};
static const unsigned int kCopySpvLen = 0;
static const unsigned char kUpopSpv[] = {0};
static const unsigned int kUpopSpvLen = 0;
#endif
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
    PFN_vkCmdEndRenderPass CmdEndRenderPass = nullptr;
    PFN_vkCmdNextSubpass CmdNextSubpass = nullptr;
    PFN_vkCmdEndRenderPass2 CmdEndRenderPass2 = nullptr;
    PFN_vkCmdNextSubpass2 CmdNextSubpass2 = nullptr;
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
    PFN_vkEndCommandBuffer EndCommandBuffer = nullptr;
    PFN_vkCreateCommandPool CreateCommandPool = nullptr;
    PFN_vkResetDescriptorPool ResetDescriptorPool = nullptr;
    PFN_vkCreateSampler CreateSampler = nullptr;
    PFN_vkDestroySampler DestroySampler = nullptr;
    std::vector<VkQueueFlags> queue_flags;
    PFN_vkCmdExecuteCommands CmdExecuteCommands = nullptr;
    PFN_vkQueueSubmit QueueSubmit = nullptr;
    PFN_vkFreeCommandBuffers FreeCommandBuffers = nullptr;
    PFN_vkAllocateCommandBuffers AllocateCommandBuffers = nullptr;
    PFN_vkResetCommandBuffer ResetCommandBuffer = nullptr;
    PFN_vkResetCommandPool ResetCommandPool = nullptr;
    PFN_vkDestroyCommandPool DestroyCommandPool = nullptr;
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
    VkDevice dev = VK_NULL_HANDLE;
    VkPhysicalDevice phys = VK_NULL_HANDLE;                 // 查显存类型用
    PFN_vkGetPhysicalDeviceMemoryProperties GetPhysMemProps = nullptr;  // instance 级, CreateDevice 时经 gipa 取
};

static std::mutex g_mtx;
static ArtifactStats g_artifact_stats;
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
    bool asset_mode_upop = false;
    uint32_t w = 0, h = 0, fmt = 0; // Last recorded dimensions, marker only.
    uint64_t copies = 0, subs = 0;
    bool disabled = false;
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
struct ImgInfo { uint32_t w = 0, h = 0, fmt = 0, usage = 0; };
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

static void cp_disable(const char *);
#include "dispatch_ownership.h"

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
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        json += std::string(",\"upop_artifact\":{\"artifact_id\":\"") + kArtifactId +
                "\",\"source_sha256\":\"" + kCandidateSourceSha256 + "\",\"spirv_sha256\":\"" +
                kCandidateSpirvSha256 + "\",\"requested\":" + (g_upop ? "true" : "false") +
                ",\"disabled_state\":" + (g_cp.disabled ? "true" : "false") +
                ",\"queue_submission_api\":\"vkQueueSubmit\",\"gpu_completion_proven\":false,"
                "\"counts\":" +
                g_artifact_stats.json() +
                ",\"ownership_scope\":\"recording_generation_per_dispatch\",\"deployable\":false,"
                "\"evaluation_eligible\":false,\"ineligible_reason\":\"" +
                rk_lifetime::refusal + "\"}";
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
static void record_rp(VkDevice dev, VkRenderPass rp, uint64_t seq, uint32_t n, FmtFn fmt,
                      LoadFn load, FinFn fin) {
    RpInfo ri; ri.n_att = n;
    for (uint32_t i = 0; i < n; i++) {
        ri.fmt.push_back(fmt(i)); ri.loadop.push_back(load(i));
        ri.final_layout.push_back(fin(i));
    }
    std::lock_guard<std::mutex> lk(g_mtx);
    g_rp_info[rp] = std::move(ri);
    g_rp_seq[rp] = seq;
    rk_lifetime::remember(rk_lifetime::render_passes, std::make_pair(dev, rp),
                          ++rk_lifetime::next_epoch);
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
    g_dump_passes = read_bool_prop("debug.knobs.passdump");
    g_copyprobe = read_bool_prop("debug.knobs.copyprobe");
    g_upop = read_bool_prop("debug.knobs.upop");
    if (g_copyprobe || g_upop) g_dump_passes = true;   // 探针/算子的 src/dst/替换全部建立在溯源表上
    RK_LOG("rk_CreateInstance entered (pkg=%s) rewrite_loadop=%d passdump=%d copyprobe=%d upop=%d",
           self_pkg().c_str(), (int)g_rewrite_loadop, (int)g_dump_passes, (int)g_copyprobe, (int)g_upop);
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
    d.CmdEndRenderPass = (PFN_vkCmdEndRenderPass)gdpa(*pDev, "vkCmdEndRenderPass");
    d.CmdNextSubpass = (PFN_vkCmdNextSubpass)gdpa(*pDev, "vkCmdNextSubpass");
    d.CmdEndRenderPass2 = (PFN_vkCmdEndRenderPass2)gdpa(*pDev, "vkCmdEndRenderPass2");
    if (!d.CmdEndRenderPass2)
        d.CmdEndRenderPass2 = (PFN_vkCmdEndRenderPass2)gdpa(*pDev, "vkCmdEndRenderPass2KHR");
    d.CmdNextSubpass2 = (PFN_vkCmdNextSubpass2)gdpa(*pDev, "vkCmdNextSubpass2");
    if (!d.CmdNextSubpass2)
        d.CmdNextSubpass2 = (PFN_vkCmdNextSubpass2)gdpa(*pDev, "vkCmdNextSubpass2KHR");
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
    d.AllocateCommandBuffers = (PFN_vkAllocateCommandBuffers)gdpa(*pDev, "vkAllocateCommandBuffers");
    d.ResetCommandBuffer = (PFN_vkResetCommandBuffer)gdpa(*pDev, "vkResetCommandBuffer");
    d.ResetCommandPool = (PFN_vkResetCommandPool)gdpa(*pDev, "vkResetCommandPool");
    d.DestroyCommandPool = (PFN_vkDestroyCommandPool)gdpa(*pDev, "vkDestroyCommandPool");
    d.DestroyFramebuffer = (PFN_vkDestroyFramebuffer)gdpa(*pDev, "vkDestroyFramebuffer");
    d.DestroyImage = (PFN_vkDestroyImage)gdpa(*pDev, "vkDestroyImage");
    d.DestroyImageView = (PFN_vkDestroyImageView)gdpa(*pDev, "vkDestroyImageView");
    d.EndCommandBuffer = (PFN_vkEndCommandBuffer)gdpa(*pDev, "vkEndCommandBuffer");
    d.CreateCommandPool = (PFN_vkCreateCommandPool)gdpa(*pDev, "vkCreateCommandPool");
    d.ResetDescriptorPool = (PFN_vkResetDescriptorPool)gdpa(*pDev, "vkResetDescriptorPool");
    d.CreateSampler = (PFN_vkCreateSampler)gdpa(*pDev, "vkCreateSampler");
    d.DestroySampler = (PFN_vkDestroySampler)gdpa(*pDev, "vkDestroySampler");
    auto families = (PFN_vkGetPhysicalDeviceQueueFamilyProperties)gipa(
        inst, "vkGetPhysicalDeviceQueueFamilyProperties");
    if (families) {
        uint32_t count = 0;
        families(phys, &count, nullptr);
        if (count <= 64) {
            std::vector<VkQueueFamilyProperties> props(count);
            families(phys, &count, props.data());
            for (uint32_t i = 0; i < count && i < props.size(); ++i)
                d.queue_flags.push_back(props[i].queueFlags);
        }
    }
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
    { std::lock_guard<std::mutex> lk(g_mtx); g_dev[key(*pDev)] = d; }
    return r;
}

static VKAPI_ATTR void VKAPI_CALL rk_DestroyDevice(VkDevice dev, const VkAllocationCallbacks* a) {
    DevDisp d;
    { std::lock_guard<std::mutex> lk(g_mtx); auto it = g_dev.find(key(dev)); if (it == g_dev.end()) return; d = it->second; g_dev.erase(it); }
    write_marker();  // 收尾再落一次最终计数
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        rk_lifetime::destroy_device(dev, d);
        if (g_cp.dev == dev) {
            rk_lifetime::destroy_assets(d);
            g_cp = CopyProbe{};
        }
        if (g_dev.empty())
            g_artifact_stats.clear_pending();
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
    if (!g_rewrite_loadop || !ci || ci->attachmentCount == 0) {
        VkResult r0 = d.CreateRenderPass(dev, ci, a, out);
        if (r0 == VK_SUCCESS && g_dump_passes && ci)
            record_rp(
                dev, *out, idx, ci->attachmentCount,
                [&](uint32_t i) { return (uint32_t)ci->pAttachments[i].format; },
                [&](uint32_t i) { return (uint32_t)ci->pAttachments[i].loadOp; },
                [&](uint32_t i) { return (uint32_t)ci->pAttachments[i].finalLayout; });
        return r0;
    }

    // pAttachments 是 const, 必须整份拷出来再改
    std::vector<VkAttachmentDescription> atts(ci->pAttachments, ci->pAttachments + ci->attachmentCount);
    std::vector<Effective> hits;
    for (uint32_t i = 0; i < atts.size(); i++) {
        if (atts[i].loadOp != VK_ATTACHMENT_LOAD_OP_LOAD) continue;
        atts[i].loadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE;
        hits.push_back({idx, i, 0});
    }
    if (hits.empty()) {
        VkResult r0 = d.CreateRenderPass(dev, ci, a, out);
        if (r0 == VK_SUCCESS && g_dump_passes)
            record_rp(
                dev, *out, idx, ci->attachmentCount,
                [&](uint32_t i) { return (uint32_t)ci->pAttachments[i].format; },
                [&](uint32_t i) { return (uint32_t)ci->pAttachments[i].loadOp; },
                [&](uint32_t i) { return (uint32_t)ci->pAttachments[i].finalLayout; });
        return r0;
    }

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
    if (g_dump_passes)
        record_rp(
            dev, *out, idx, ci->attachmentCount,
            [&](uint32_t i) { return (uint32_t)atts[i].format; },
            [&](uint32_t i) { return (uint32_t)atts[i].loadOp; },
            [&](uint32_t i) { return (uint32_t)atts[i].finalLayout; });
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
    if (!g_rewrite_loadop || !ci || ci->attachmentCount == 0) {
        VkResult r0 = d.CreateRenderPass2(dev, ci, a, out);
        if (r0 == VK_SUCCESS && g_dump_passes && ci)
            record_rp(
                dev, *out, idx, ci->attachmentCount,
                [&](uint32_t i) { return (uint32_t)ci->pAttachments[i].format; },
                [&](uint32_t i) { return (uint32_t)ci->pAttachments[i].loadOp; },
                [&](uint32_t i) { return (uint32_t)ci->pAttachments[i].finalLayout; });
        return r0;
    }

    std::vector<VkAttachmentDescription2> atts(ci->pAttachments, ci->pAttachments + ci->attachmentCount);
    std::vector<Effective> hits;
    for (uint32_t i = 0; i < atts.size(); i++) {
        if (atts[i].loadOp != VK_ATTACHMENT_LOAD_OP_LOAD) continue;
        atts[i].loadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE;
        hits.push_back({idx, i, 0});
    }
    if (hits.empty()) {
        VkResult r0 = d.CreateRenderPass2(dev, ci, a, out);
        if (r0 == VK_SUCCESS && g_dump_passes)
            record_rp(
                dev, *out, idx, ci->attachmentCount,
                [&](uint32_t i) { return (uint32_t)ci->pAttachments[i].format; },
                [&](uint32_t i) { return (uint32_t)ci->pAttachments[i].loadOp; },
                [&](uint32_t i) { return (uint32_t)ci->pAttachments[i].finalLayout; });
        return r0;
    }

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
    if (g_dump_passes)
        record_rp(
            dev, *out, idx, ci->attachmentCount,
            [&](uint32_t i) { return (uint32_t)atts[i].format; },
            [&](uint32_t i) { return (uint32_t)atts[i].loadOp; },
            [&](uint32_t i) { return (uint32_t)atts[i].finalLayout; });
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
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        g_rewritten.erase(rp);
        // 句柄会复用: 这三张表不一起擦, 新建的 pass 拿到旧句柄值就继承死者的 seq/形状 (评审 #8)
        g_rp_seq.erase(rp);
        rk_lifetime::render_passes.erase({dev, rp});
        g_rp_info.erase(rp);
    }
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
        rk_lifetime::created_image(dev, *out, *ci);
        g_img[*out] = ImgInfo{ci->extent.width, ci->extent.height, (uint32_t)ci->format, (uint32_t)ci->usage};
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
        rk_lifetime::created_view(dev, *out, *ci);
    }
    return r;
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
            auto& m = g_set_views[ws.dstSet];
            auto& ms = g_set_samplers[ws.dstSet];
            for (uint32_t j = 0; j < ws.descriptorCount; j++)
                if (ws.pImageInfo[j].imageView != VK_NULL_HANDLE) {
                    uint32_t kk = ws.dstArrayElement + j + (ws.dstBinding << 8);
                    m[kk] = ws.pImageInfo[j].imageView;
                    ms[kk] = ws.pImageInfo[j].sampler;
                }
        }
    }
    d.UpdateDescriptorSets(dev, nw, w, nc, c);
    if (g_dump_passes) {
        std::lock_guard<std::mutex> lk(g_mtx);
        rk_lifetime::updated_sets(dev, nw, w, nc, c);
    }
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
        rk_lifetime::created_layout(dev, *out, *ci);
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
        rk_lifetime::allocated_sets(dev, *ai, out);
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
        rk_lifetime::invalidate_pool(dev, pool, true);
        auto it = g_pool_sets.find(pool);
        if (it != g_pool_sets.end()) {
            for (auto set : it->second) {
                g_set_views.erase(set); g_set_samplers.erase(set); g_set_layout.erase(set);
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
    if (bp == VK_PIPELINE_BIND_POINT_COMPUTE) {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto it = rk_lifetime::cbs.find(cb);
        if (it != rk_lifetime::cbs.end())
            ++it->second.compute_mutation;
    }
    if (g_dump_passes && sets && bp == VK_PIPELINE_BIND_POINT_GRAPHICS) {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto& v = g_cb_sets[cb];
        for (uint32_t i = 0; i < n; i++) v.push_back(sets[i]);
        if (v.size() > 32) v.erase(v.begin(), v.end() - 32);
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
            if (any) {
                d.CmdBindDescriptorSets(cb, bp, pl, first, n, repl.data(), ndyn, dyn);
                if (g_upop) for (uint32_t i = 0; i < n; ++i) if (repl[i] != sets[i]) ++g_artifact_stats.substituted;
                return;
            }
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
        auto live_rp = rk_lifetime::render_passes.find({dev, ci->renderPass});
        rk_lifetime::remember(rk_lifetime::framebuffers, std::make_pair(dev, *out),
                              rk_lifetime::Framebuffer{
                                  ++rk_lifetime::next_epoch, ci->renderPass,
                                  live_rp == rk_lifetime::render_passes.end() ? 0 : live_rp->second,
                                  !ci->pNext && !ci->flags && ci->layers == 1});
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

static void note_draw(VkCommandBuffer cb, uint64_t n) {
    if (!g_dump_passes) return;
    std::lock_guard<std::mutex> lk(g_mtx);
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
    note_draw(cb, 1); d.CmdDraw(cb, vc, ic, fv, fi);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdDrawIndexed(VkCommandBuffer cb, uint32_t ic, uint32_t inst,
                                                    uint32_t fi, int32_t vo, uint32_t fin) {
    DevDisp d; if (!dev_of(key(cb), &d) || !d.CmdDrawIndexed) { RK_LOG_ONCE("CmdDrawIndexed: 无下层"); return; }
    note_draw(cb, 1); d.CmdDrawIndexed(cb, ic, inst, fi, vo, fin);
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

static void cp_disable(const char* reason) {
    g_cp.disabled = true;
    if (g_upop) g_artifact_stats.disable(reason);
#ifdef RK_DYNAMIC_UPOP_BUILD
    else g_artifact_stats.disable(reason);
#endif
}
// Same ownership path is used by production and host fixtures. Unknown contracts refuse injection.
static void copyprobe_dispatch(VkCommandBuffer cb, const DevDisp &d, VkRenderPass rp,
                               const std::vector<VkImageView> &views) {
    rk_lifetime::inject(cb, d, rp, views);
}
static VkDescriptorSet copyprobe_clone(VkCommandBuffer cb, const DevDisp &d, VkDescriptorSet set) {
    return rk_lifetime::clone(cb, d, set);
}

// 把 (render pass 形状) 与 (framebuffer 宽高) 关联起来, 按 (pass, 宽, 高) 聚合计数,
// 并在采样帧里记下有序序列 —— "第几个 pass、多大、什么格式" 就是从这儿读出来的。
static void note_pass(VkCommandBuffer cb, const VkRenderPassBeginInfo *bi, const DevDisp &d,
                      bool inline_contents) {
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
    auto owner = rk_lifetime::cbs.find(cb);
    if (owner != rk_lifetime::cbs.end()) {
        owner->second.active = SIZE_MAX;
        owner->second.target_framebuffer = bi->framebuffer;
        owner->second.inline_contents = inline_contents;
        owner->second.inside_pass = true;
    }
    bool is_swap = (w == g_swap_w && h == g_swap_h);
    auto ai = g_fb_att.find(bi->framebuffer);
    if (!is_swap && ai != g_fb_att.end()) {
        g_cb_prev_att[cb] = ai->second;   // 记住上一个渲染分辨率 pass 的附件
        g_cb_prev_rp[cb] = bi->renderPass;
    }
    if (is_swap) {
        g_cb_cur_is_swap[cb] = true;
        if (g_copyprobe || g_upop)
            copyprobe_dispatch(cb, d, bi->renderPass,
                               g_cb_prev_att.count(cb) ? g_cb_prev_att[cb]
                                                       : std::vector<VkImageView>{});
    } else g_cb_cur_is_swap[cb] = false;
    if (g_sampling && g_frame_seq.size() < 400) {
        uint64_t seq = 0; auto si = g_rp_seq.find(bi->renderPass);
        if (si != g_rp_seq.end()) seq = si->second;
        char buf[192];
        snprintf(buf, sizeof buf, "{\"pass\":%llu,\"w\":%u,\"h\":%u,\"att\":%u,\"fmt0\":%u}",
                 (unsigned long long)seq, w, h, st.n_att, st.fmt.empty() ? 0u : st.fmt[0]);
        g_frame_seq.push_back(buf);
    }
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
    note_pass(cb, bi, d, c == VK_SUBPASS_CONTENTS_INLINE);
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
    note_pass(cb, bi, d,
              si &&
                  si->contents == VK_SUBPASS_CONTENTS_INLINE); // 与 CmdBeginRenderPass 一致, 漏了它
                                                               // RenderPass2 开的 pass 就不进表
    d.CmdBeginRenderPass2(cb, bi, si);
}
static void clear_cb_observer(VkCommandBuffer cb) {
    g_cb_cur.erase(cb);
    g_cb_sets.erase(cb);
    g_cb_draw_idx.erase(cb);
    g_cb_prev_att.erase(cb);
    g_cb_prev_rp.erase(cb);
    g_cb_cur_is_swap.erase(cb);
    g_cb_passes.erase(cb);
}

// 二级命令缓冲里的 draw 没有自己的 BeginRenderPass —— 当前 pass 从继承信息里拿 (评审 #9:
// 不然录进 secondary 的 draw 在 g_cb_cur 里匹配不上, draws 系统性偏低, 按 draw 数找
// "放大那一步"会选错 pass)。顺手在每次重录开始时清掉这条 cb 的旧状态 (评审 #10 的兜底:
// cb 反复重录时这些 per-cb 表才不会只增不减)。
static VKAPI_ATTR VkResult VKAPI_CALL rk_BeginCommandBuffer(VkCommandBuffer cb,
                                                            const VkCommandBufferBeginInfo *bi) {
    DevDisp d;
    if (!dev_of(key(cb), &d) || !d.BeginCommandBuffer) {
        RK_LOG_ONCE("BeginCommandBuffer: 无下层");
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    VkResult result = d.BeginCommandBuffer(cb, bi);
    if (result != VK_SUCCESS) {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto it = rk_lifetime::cbs.find(cb);
        if (it != rk_lifetime::cbs.end())
            it->second.uncertain = true;
        if (g_copyprobe || g_upop)
            rk_lifetime::reject("begin_command_buffer_failed");
        return result;
    }
    if (g_dump_passes) {
        std::lock_guard<std::mutex> lk(g_mtx);
        clear_cb_observer(cb);
        if (bi && (bi->flags & VK_COMMAND_BUFFER_USAGE_RENDER_PASS_CONTINUE_BIT) &&
            bi->pInheritanceInfo) {
            const VkCommandBufferInheritanceInfo *inh = bi->pInheritanceInfo;
            uint32_t w = 0, h = 0; // inheritance 的 framebuffer 允许是 VK_NULL_HANDLE, 那就只有 0x0
            auto fi = g_fb_info.find(inh->framebuffer);
            if (fi != g_fb_info.end()) {
                w = fi->second.w;
                h = fi->second.h;
            }
            uint64_t k = pass_key(inh->renderPass, w, h);
            auto &st = g_pass_stat[k];
            if (!st.begins) {
                st.w = w;
                st.h = h;
                auto ri = g_rp_info.find(inh->renderPass);
                if (ri != g_rp_info.end()) {
                    st.n_att = ri->second.n_att;
                    st.fmt = ri->second.fmt;
                }
            }
            st.begins++;
            g_cb_cur[cb] = k;
            g_cb_draw_idx[cb] = 0;
            g_cb_cur_is_swap[cb] = false;
            {
                auto &v = g_cb_passes[cb];
                if (v.size() < 256)
                    v.push_back(k);
            }
        }
    }
    if (g_copyprobe || g_upop) {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto it = rk_lifetime::cbs.find(cb);
        if (it == rk_lifetime::cbs.end())
            rk_lifetime::reject("unknown_command_buffer_identity");
        else {
            rk_lifetime::retire(it->second, d);
            it->second.recording_epoch = ++rk_lifetime::next_epoch;
            it->second.flags = bi ? bi->flags : 0;
            it->second.recording = true;
        }
    }
    if (g_upop) {
        std::lock_guard<std::mutex> lk(g_mtx);
        g_artifact_stats.reset(reinterpret_cast<uintptr_t>(cb));
    }
    return result;
}

// 二级缓冲被执行时, 它录的 pass 并入主缓冲的录制清单 —— 提交主缓冲时才算执行 (评审 #4/#9)
static VKAPI_ATTR void VKAPI_CALL rk_CmdExecuteCommands(VkCommandBuffer cb, uint32_t n,
                                                        const VkCommandBuffer *cbs) {
    DevDisp d;
    if (!dev_of(key(cb), &d) || !d.CmdExecuteCommands) {
        RK_LOG_ONCE("CmdExecuteCommands: 无下层");
        return;
    }
    if (g_dump_passes && cbs) {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto &v = g_cb_passes[cb];
        for (uint32_t i = 0; i < n; i++) {
            if (g_upop)
                g_artifact_stats.inherit(reinterpret_cast<uintptr_t>(cb),
                                         reinterpret_cast<uintptr_t>(cbs[i]));
            auto it = g_cb_passes.find(cbs[i]);
            if (it == g_cb_passes.end())
                continue;
            for (uint64_t k : it->second) {
                if (v.size() >= 256)
                    break;
                v.push_back(k);
            }
        }
    }
    d.CmdExecuteCommands(cb, n, cbs);
}

// begins 数的是录制, 不是执行; 真正的"这个 pass 每帧都在跑"要看提交 (评审 #4)。
// 提交时把每条 cb 录过的 pass 记进本帧集合, present 时统一折算成 frames_seen。
static VKAPI_ATTR VkResult VKAPI_CALL rk_QueueSubmit(VkQueue q, uint32_t n, const VkSubmitInfo *si,
                                                     VkFence fence) {
    DevDisp d;
    if (!dev_of(key(q), &d) || !d.QueueSubmit) {
        RK_LOG_ONCE("QueueSubmit: 无下层");
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    if (g_dump_passes && si) {
        std::lock_guard<std::mutex> lk(g_mtx);
        for (uint32_t i = 0; i < n; i++)
            for (uint32_t j = 0; j < si[i].commandBufferCount; j++) {
                auto it = g_cb_passes.find(si[i].pCommandBuffers[j]);
                if (it != g_cb_passes.end())
                    for (uint64_t k : it->second)
                        g_frame_seen.insert(k);
            }
    }
    uint64_t submitted = 0;
    if (g_upop && si) {
        std::vector<uintptr_t> cbs;
        for (uint32_t i = 0; i < n; ++i)
            if (si[i].pCommandBuffers)
                for (uint32_t j = 0; j < si[i].commandBufferCount; ++j)
                    cbs.push_back(reinterpret_cast<uintptr_t>(si[i].pCommandBuffers[j]));
        std::lock_guard<std::mutex> lk(g_mtx);
        submitted = g_artifact_stats.submitted_count(cbs);
    }
    VkResult result = d.QueueSubmit(q, n, si, fence);
    if (g_upop) {
        std::lock_guard<std::mutex> lk(g_mtx);
        g_artifact_stats.submit_result(submitted, result == VK_SUCCESS);
    }
    return result;
}

// cb 被释放时摘掉它的全部 per-cb 状态 (评审 #10: 这些 map 跑在游戏进程里, 只增不减会无限涨)
static VKAPI_ATTR void VKAPI_CALL rk_FreeCommandBuffers(VkDevice dev, VkCommandPool pool,
                                                        uint32_t n, const VkCommandBuffer *cbs) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.FreeCommandBuffers) {
        RK_LOG_ONCE("FreeCommandBuffers: 无下层");
        return;
    }
    d.FreeCommandBuffers(dev, pool, n, cbs);
    if (g_dump_passes && cbs) {
        std::lock_guard<std::mutex> lk(g_mtx);
        for (uint32_t i = 0; i < n; i++) {
            auto owner = rk_lifetime::cbs.find(cbs[i]);
            if (owner != rk_lifetime::cbs.end()) {
                rk_lifetime::retire(owner->second, d);
                rk_lifetime::cbs.erase(owner);
            }
            if (g_upop)
                g_artifact_stats.free(reinterpret_cast<uintptr_t>(cbs[i]));
            clear_cb_observer(cbs[i]);
        }
    }
}

static VKAPI_ATTR VkResult VKAPI_CALL rk_AllocateCommandBuffers(
    VkDevice dev, const VkCommandBufferAllocateInfo *ci, VkCommandBuffer *cbs) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.AllocateCommandBuffers)
        return VK_ERROR_INITIALIZATION_FAILED;
    VkResult result = d.AllocateCommandBuffers(dev, ci, cbs);
    if (result == VK_SUCCESS && ci && cbs) {
        std::lock_guard<std::mutex> lk(g_mtx);
        for (uint32_t i = 0; i < ci->commandBufferCount; ++i) {
            g_artifact_stats.allocated(reinterpret_cast<uintptr_t>(cbs[i]),
                                       reinterpret_cast<uintptr_t>(ci->commandPool));
            rk_lifetime::CB cb;
            cb.dev = dev;
            cb.pool = ci->commandPool;
            cb.level = ci->level;
            cb.allocation = ++rk_lifetime::next_epoch;
            auto p = rk_lifetime::command_pools.find({dev, ci->commandPool});
            if (p != rk_lifetime::command_pools.end())
                cb.pool_epoch = p->second.epoch;
            rk_lifetime::remember(rk_lifetime::cbs, cbs[i], cb);
        }
    }
    return result;
}
static VKAPI_ATTR VkResult VKAPI_CALL rk_ResetCommandBuffer(VkCommandBuffer cb,
                                                            VkCommandBufferResetFlags flags) {
    DevDisp d;
    if (!dev_of(key(cb), &d) || !d.ResetCommandBuffer)
        return VK_ERROR_INITIALIZATION_FAILED;
    VkResult result = d.ResetCommandBuffer(cb, flags);
    if (result == VK_SUCCESS) {
        std::lock_guard<std::mutex> lk(g_mtx);
        g_artifact_stats.reset(reinterpret_cast<uintptr_t>(cb));
        clear_cb_observer(cb);
        auto it = rk_lifetime::cbs.find(cb);
        if (it != rk_lifetime::cbs.end())
            rk_lifetime::retire(it->second, d);
    } else {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto it = rk_lifetime::cbs.find(cb);
        if (it != rk_lifetime::cbs.end())
            it->second.uncertain = true;
        if (g_copyprobe || g_upop)
            rk_lifetime::reject("reset_command_buffer_failed");
    }
    return result;
}
static VKAPI_ATTR VkResult VKAPI_CALL rk_ResetCommandPool(VkDevice dev, VkCommandPool pool,
                                                          VkCommandPoolResetFlags flags) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.ResetCommandPool)
        return VK_ERROR_INITIALIZATION_FAILED;
    VkResult result = d.ResetCommandPool(dev, pool, flags);
    if (result == VK_SUCCESS) {
        std::lock_guard<std::mutex> lk(g_mtx);
        g_artifact_stats.reset_pool(reinterpret_cast<uintptr_t>(pool));
        for (auto &cb : rk_lifetime::cbs)
            if (cb.second.dev == dev && cb.second.pool == pool) {
                rk_lifetime::retire(cb.second, d);
                clear_cb_observer(cb.first);
            }
    }
    if (result != VK_SUCCESS && (g_copyprobe || g_upop)) {
        std::lock_guard<std::mutex> lk(g_mtx);
        for (auto &cb : rk_lifetime::cbs)
            if (cb.second.dev == dev && cb.second.pool == pool)
                cb.second.uncertain = true;
        rk_lifetime::reject("reset_command_pool_failed");
    }
    return result;
}
static VKAPI_ATTR void VKAPI_CALL rk_DestroyCommandPool(VkDevice dev, VkCommandPool pool,
                                                        const VkAllocationCallbacks *a) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.DestroyCommandPool)
        return;
    d.DestroyCommandPool(dev, pool, a);
    std::lock_guard<std::mutex> lk(g_mtx);
    g_artifact_stats.reset_pool(reinterpret_cast<uintptr_t>(pool), true);
    for (auto it = rk_lifetime::cbs.begin(); it != rk_lifetime::cbs.end();)
        if (it->second.dev == dev && it->second.pool == pool) {
            rk_lifetime::retire(it->second, d);
            clear_cb_observer(it->first);
            it = rk_lifetime::cbs.erase(it);
        } else
            ++it;
    rk_lifetime::command_pools.erase({dev, pool});
}

// 同理: image / view / framebuffer 销毁时摘表 (评审 #10)。
// 注意 g_set_views 里可能还指着已销毁的 view —— 不级联清 (view 表先摘, 溯源时 g_view/g_img
// 查不到自然跳过, 不会读到假数据)。
static VKAPI_ATTR void VKAPI_CALL rk_DestroyFramebuffer(VkDevice dev, VkFramebuffer fb,
                                                        const VkAllocationCallbacks *a) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.DestroyFramebuffer) {
        RK_LOG_ONCE("DestroyFramebuffer: 无下层");
        return;
    }
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        g_fb_info.erase(fb);
        g_fb_att.erase(fb);
        rk_lifetime::framebuffers.erase({dev, fb});
    }
    d.DestroyFramebuffer(dev, fb, a);
}
static VKAPI_ATTR void VKAPI_CALL rk_DestroyImage(VkDevice dev, VkImage img,
                                                  const VkAllocationCallbacks *a) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.DestroyImage) {
        RK_LOG_ONCE("DestroyImage: 无下层");
        return;
    }
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        g_img.erase(img);
        rk_lifetime::images.erase({dev, img});
    }
    d.DestroyImage(dev, img, a);
}
static VKAPI_ATTR void VKAPI_CALL rk_DestroyImageView(VkDevice dev, VkImageView v,
                                                      const VkAllocationCallbacks *a) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.DestroyImageView) {
        RK_LOG_ONCE("DestroyImageView: 无下层");
        return;
    }
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        g_view.erase(v);
        rk_lifetime::views.erase({dev, v});
    }
    d.DestroyImageView(dev, v, a);
}

static VKAPI_ATTR VkResult VKAPI_CALL rk_QueuePresentKHR(VkQueue q, const VkPresentInfoKHR* pi) {
    DevDisp d;
    if (!dev_of(key(q), &d) || !d.QueuePresentKHR) {
        RK_LOG_ONCE("QueuePresentKHR: 查不到下层函数");
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    uint64_t f = ++g_frames;
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

static void end_target_pass(VkCommandBuffer cb, bool next_subpass) {
    std::lock_guard<std::mutex> lk(g_mtx);
    auto it = rk_lifetime::cbs.find(cb);
    if (it != rk_lifetime::cbs.end()) {
        if (next_subpass && it->second.active != SIZE_MAX)
            rk_lifetime::reject("unsupported_target_subpass");
        it->second.active = SIZE_MAX;
        it->second.inside_pass = next_subpass;
    }
    g_cb_cur_is_swap[cb] = false;
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdEndRenderPass(VkCommandBuffer cb) {
    DevDisp d;
    if (!dev_of(key(cb), &d) || !d.CmdEndRenderPass)
        return;
    d.CmdEndRenderPass(cb);
    end_target_pass(cb, false);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdNextSubpass(VkCommandBuffer cb,
                                                    VkSubpassContents contents) {
    DevDisp d;
    if (!dev_of(key(cb), &d) || !d.CmdNextSubpass)
        return;
    d.CmdNextSubpass(cb, contents);
    end_target_pass(cb, true);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdEndRenderPass2(VkCommandBuffer cb,
                                                       const VkSubpassEndInfo *info) {
    DevDisp d;
    if (!dev_of(key(cb), &d) || !d.CmdEndRenderPass2)
        return;
    d.CmdEndRenderPass2(cb, info);
    end_target_pass(cb, false);
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdNextSubpass2(VkCommandBuffer cb,
                                                     const VkSubpassBeginInfo *begin,
                                                     const VkSubpassEndInfo *end) {
    DevDisp d;
    if (!dev_of(key(cb), &d) || !d.CmdNextSubpass2)
        return;
    d.CmdNextSubpass2(cb, begin, end);
    end_target_pass(cb, true);
}

// Ownership metadata callbacks: original API calls always forward before publication/retirement.
static VKAPI_ATTR VkResult VKAPI_CALL rk_CreateCommandPool(VkDevice dev,
                                                           const VkCommandPoolCreateInfo *ci,
                                                           const VkAllocationCallbacks *a,
                                                           VkCommandPool *out) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.CreateCommandPool)
        return VK_ERROR_INITIALIZATION_FAILED;
    VkResult r = d.CreateCommandPool(dev, ci, a, out);
    if (r == VK_SUCCESS && ci && g_dump_passes) {
        std::lock_guard<std::mutex> lk(g_mtx);
        bool family = ci->queueFamilyIndex < d.queue_flags.size() &&
                      (d.queue_flags[ci->queueFamilyIndex] &
                       (VK_QUEUE_GRAPHICS_BIT | VK_QUEUE_COMPUTE_BIT)) ==
                          (VK_QUEUE_GRAPHICS_BIT | VK_QUEUE_COMPUTE_BIT);
        rk_lifetime::remember(
            rk_lifetime::command_pools, std::make_pair(dev, *out),
            rk_lifetime::CommandPool{
                ++rk_lifetime::next_epoch, ci->flags,
                !ci->pNext && family &&
                    !(ci->flags & ~(VK_COMMAND_POOL_CREATE_TRANSIENT_BIT |
                                    VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT))});
    }
    return r;
}
static VKAPI_ATTR VkResult VKAPI_CALL rk_EndCommandBuffer(VkCommandBuffer cb) {
    DevDisp d;
    if (!dev_of(key(cb), &d) || !d.EndCommandBuffer)
        return VK_ERROR_INITIALIZATION_FAILED;
    VkResult r = d.EndCommandBuffer(cb);
    std::lock_guard<std::mutex> lk(g_mtx);
    auto it = rk_lifetime::cbs.find(cb);
    if (it != rk_lifetime::cbs.end()) {
        it->second.recording = false;
        if (r != VK_SUCCESS) {
            it->second.uncertain = true;
            rk_lifetime::reject("end_command_buffer_failed");
        }
    }
    return r;
}
static VKAPI_ATTR void VKAPI_CALL rk_CmdBindPipeline(VkCommandBuffer cb, VkPipelineBindPoint point,
                                                     VkPipeline pipeline) {
    DevDisp d;
    if (!dev_of(key(cb), &d) || !d.CmdBindPipeline)
        return;
    d.CmdBindPipeline(cb, point, pipeline);
    if (point == VK_PIPELINE_BIND_POINT_COMPUTE) {
        std::lock_guard<std::mutex> lk(g_mtx);
        auto it = rk_lifetime::cbs.find(cb);
        if (it != rk_lifetime::cbs.end())
            ++it->second.compute_mutation;
    }
}
static VKAPI_ATTR VkResult VKAPI_CALL rk_CreateDescriptorPool(VkDevice dev,
                                                              const VkDescriptorPoolCreateInfo *ci,
                                                              const VkAllocationCallbacks *a,
                                                              VkDescriptorPool *out) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.CreateDescriptorPool)
        return VK_ERROR_INITIALIZATION_FAILED;
    VkResult r = d.CreateDescriptorPool(dev, ci, a, out);
    if (r == VK_SUCCESS && ci && g_dump_passes) {
        std::lock_guard<std::mutex> lk(g_mtx);
        rk_lifetime::remember(
            rk_lifetime::app_pools, std::make_pair(dev, *out),
            rk_lifetime::AppPool{
                ++rk_lifetime::next_epoch,
                !ci->pNext && !(ci->flags & ~VK_DESCRIPTOR_POOL_CREATE_FREE_DESCRIPTOR_SET_BIT)});
    }
    return r;
}
static void clear_set_observer(VkDescriptorSet set) {
    g_set_views.erase(set);
    g_set_samplers.erase(set);
    g_set_layout.erase(set);
    g_set_gen.erase(set);
}
static VKAPI_ATTR VkResult VKAPI_CALL rk_FreeDescriptorSets(VkDevice dev, VkDescriptorPool pool,
                                                            uint32_t n,
                                                            const VkDescriptorSet *sets) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.FreeDescriptorSets)
        return VK_ERROR_INITIALIZATION_FAILED;
    VkResult r = d.FreeDescriptorSets(dev, pool, n, sets);
    if (r == VK_SUCCESS) {
        std::lock_guard<std::mutex> lk(g_mtx);
        for (uint32_t i = 0; i < n; ++i) {
            rk_lifetime::sets.erase({dev, sets[i]});
            clear_set_observer(sets[i]);
        }
        auto p = g_pool_sets.find(pool);
        if (p != g_pool_sets.end())
            for (uint32_t i = 0; i < n; ++i) {
                auto &v = p->second;
                for (auto it = v.begin(); it != v.end();)
                    if (*it == sets[i])
                        it = v.erase(it);
                    else
                        ++it;
            }
    }
    return r;
}
static VKAPI_ATTR VkResult VKAPI_CALL rk_ResetDescriptorPool(VkDevice dev, VkDescriptorPool pool,
                                                             VkDescriptorPoolResetFlags flags) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.ResetDescriptorPool)
        return VK_ERROR_INITIALIZATION_FAILED;
    VkResult r = d.ResetDescriptorPool(dev, pool, flags);
    if (r == VK_SUCCESS) {
        std::lock_guard<std::mutex> lk(g_mtx);
        rk_lifetime::invalidate_pool(dev, pool, false);
        auto p = g_pool_sets.find(pool);
        if (p != g_pool_sets.end()) {
            for (auto set : p->second)
                clear_set_observer(set);
            g_pool_sets.erase(p);
        }
    }
    return r;
}
static VKAPI_ATTR void VKAPI_CALL rk_DestroyDescriptorSetLayout(VkDevice dev,
                                                                VkDescriptorSetLayout layout,
                                                                const VkAllocationCallbacks *a) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.DestroyDescriptorSetLayout)
        return;
    d.DestroyDescriptorSetLayout(dev, layout, a);
    std::lock_guard<std::mutex> lk(g_mtx);
    rk_lifetime::layouts.erase({dev, layout});
    g_layout_bindings.erase(layout);
}
static VKAPI_ATTR VkResult VKAPI_CALL rk_CreateSampler(VkDevice dev, const VkSamplerCreateInfo *ci,
                                                       const VkAllocationCallbacks *a,
                                                       VkSampler *out) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.CreateSampler)
        return VK_ERROR_INITIALIZATION_FAILED;
    VkResult r = d.CreateSampler(dev, ci, a, out);
    if (r == VK_SUCCESS && g_dump_passes) {
        std::lock_guard<std::mutex> lk(g_mtx);
        rk_lifetime::remember(
            rk_lifetime::samplers, std::make_pair(dev, *out),
            rk_lifetime::Sampler{++rk_lifetime::next_epoch, ci && !ci->pNext && !ci->flags});
    }
    return r;
}
static VKAPI_ATTR void VKAPI_CALL rk_DestroySampler(VkDevice dev, VkSampler sampler,
                                                    const VkAllocationCallbacks *a) {
    DevDisp d;
    if (!dev_of(key(dev), &d) || !d.DestroySampler)
        return;
    d.DestroySampler(dev, sampler, a);
    std::lock_guard<std::mutex> lk(g_mtx);
    rk_lifetime::samplers.erase({dev, sampler});
}

// ── proc addr 分发 ──
#define RK_HOOK(n, fn) if (!strcmp(name, n)) return reinterpret_cast<PFN_vkVoidFunction>(fn)

static bool lower_optional_instance(VkInstance instance,const char* name) {
    if(!instance)return false;
    InstDisp d;{std::lock_guard<std::mutex> lk(g_mtx);auto it=g_inst.find(key(instance));if(it==g_inst.end())return false;d=it->second;}
    return d.gipa && d.gipa(instance,name);
}
static bool lower_optional_device(VkDevice dev,const char* name) {
    if(!dev)return false;
    DevDisp d;if(!dev_of(key(dev),&d))return false;
    return d.gdpa && d.gdpa(dev,name);
}

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
        if (g_upop || g_copyprobe) {
            RK_HOOK("vkCmdEndRenderPass", rk_CmdEndRenderPass);
            RK_HOOK("vkCmdNextSubpass", rk_CmdNextSubpass);
            if(lower_optional_instance(instance,name)) RK_HOOK("vkCmdEndRenderPass2", rk_CmdEndRenderPass2);
            if(lower_optional_instance(instance,name)) RK_HOOK("vkCmdEndRenderPass2KHR", rk_CmdEndRenderPass2);
            if(lower_optional_instance(instance,name)) RK_HOOK("vkCmdNextSubpass2", rk_CmdNextSubpass2);
            if(lower_optional_instance(instance,name)) RK_HOOK("vkCmdNextSubpass2KHR", rk_CmdNextSubpass2);
        }
        RK_HOOK("vkCreateFramebuffer", rk_CreateFramebuffer);
        RK_HOOK("vkBeginCommandBuffer", rk_BeginCommandBuffer);
        RK_HOOK("vkCmdExecuteCommands", rk_CmdExecuteCommands);
        RK_HOOK("vkQueueSubmit", rk_QueueSubmit);
        RK_HOOK("vkFreeCommandBuffers", rk_FreeCommandBuffers);
        if (g_upop || g_copyprobe) {
            RK_HOOK("vkCreateCommandPool", rk_CreateCommandPool);
            RK_HOOK("vkEndCommandBuffer", rk_EndCommandBuffer);
            RK_HOOK("vkCmdBindPipeline", rk_CmdBindPipeline);
            RK_HOOK("vkAllocateCommandBuffers", rk_AllocateCommandBuffers);
            RK_HOOK("vkResetCommandBuffer", rk_ResetCommandBuffer);
            RK_HOOK("vkResetCommandPool", rk_ResetCommandPool);
            RK_HOOK("vkDestroyCommandPool", rk_DestroyCommandPool);
        }
        RK_HOOK("vkDestroyFramebuffer", rk_DestroyFramebuffer);
        RK_HOOK("vkDestroyImage", rk_DestroyImage);
        RK_HOOK("vkDestroyImageView", rk_DestroyImageView);
        RK_HOOK("vkCreateDescriptorSetLayout", rk_CreateDescriptorSetLayout);
        RK_HOOK("vkAllocateDescriptorSets", rk_AllocateDescriptorSets);
        RK_HOOK("vkDestroyDescriptorPool", rk_DestroyDescriptorPool);
        if (g_upop || g_copyprobe) {
            RK_HOOK("vkCreateDescriptorPool", rk_CreateDescriptorPool);
            RK_HOOK("vkFreeDescriptorSets", rk_FreeDescriptorSets);
            RK_HOOK("vkResetDescriptorPool", rk_ResetDescriptorPool);
            RK_HOOK("vkDestroyDescriptorSetLayout", rk_DestroyDescriptorSetLayout);
            RK_HOOK("vkCreateSampler", rk_CreateSampler);
            RK_HOOK("vkDestroySampler", rk_DestroySampler);
        }
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
        if (g_upop || g_copyprobe) {
            RK_HOOK("vkCmdEndRenderPass", rk_CmdEndRenderPass);
            RK_HOOK("vkCmdNextSubpass", rk_CmdNextSubpass);
            if(lower_optional_device(dev,name)) RK_HOOK("vkCmdEndRenderPass2", rk_CmdEndRenderPass2);
            if(lower_optional_device(dev,name)) RK_HOOK("vkCmdEndRenderPass2KHR", rk_CmdEndRenderPass2);
            if(lower_optional_device(dev,name)) RK_HOOK("vkCmdNextSubpass2", rk_CmdNextSubpass2);
            if(lower_optional_device(dev,name)) RK_HOOK("vkCmdNextSubpass2KHR", rk_CmdNextSubpass2);
        }
        RK_HOOK("vkCreateFramebuffer", rk_CreateFramebuffer);
        RK_HOOK("vkBeginCommandBuffer", rk_BeginCommandBuffer);
        RK_HOOK("vkCmdExecuteCommands", rk_CmdExecuteCommands);
        RK_HOOK("vkQueueSubmit", rk_QueueSubmit);
        RK_HOOK("vkFreeCommandBuffers", rk_FreeCommandBuffers);
        if (g_upop || g_copyprobe) {
            RK_HOOK("vkCreateCommandPool", rk_CreateCommandPool);
            RK_HOOK("vkEndCommandBuffer", rk_EndCommandBuffer);
            RK_HOOK("vkCmdBindPipeline", rk_CmdBindPipeline);
            RK_HOOK("vkAllocateCommandBuffers", rk_AllocateCommandBuffers);
            RK_HOOK("vkResetCommandBuffer", rk_ResetCommandBuffer);
            RK_HOOK("vkResetCommandPool", rk_ResetCommandPool);
            RK_HOOK("vkDestroyCommandPool", rk_DestroyCommandPool);
        }
        RK_HOOK("vkDestroyFramebuffer", rk_DestroyFramebuffer);
        RK_HOOK("vkDestroyImage", rk_DestroyImage);
        RK_HOOK("vkDestroyImageView", rk_DestroyImageView);
        RK_HOOK("vkCreateDescriptorSetLayout", rk_CreateDescriptorSetLayout);
        RK_HOOK("vkAllocateDescriptorSets", rk_AllocateDescriptorSets);
        RK_HOOK("vkDestroyDescriptorPool", rk_DestroyDescriptorPool);
        if (g_upop || g_copyprobe) {
            RK_HOOK("vkCreateDescriptorPool", rk_CreateDescriptorPool);
            RK_HOOK("vkFreeDescriptorSets", rk_FreeDescriptorSets);
            RK_HOOK("vkResetDescriptorPool", rk_ResetDescriptorPool);
            RK_HOOK("vkDestroyDescriptorSetLayout", rk_DestroyDescriptorSetLayout);
            RK_HOOK("vkCreateSampler", rk_CreateSampler);
            RK_HOOK("vkDestroySampler", rk_DestroySampler);
        }
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
