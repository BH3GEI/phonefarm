// framecap.h — 按「帧序号」抓帧 + 在画面角落盖帧序号条码 (成对采集用)
//
// 只被 vk_layer_refknobs.cpp include 一次, 用它里面的 DevDisp / g_mtx / RK_LOG / self_pkg。
//
// 为什么要它: Megacity 的相机位姿只由帧序号决定 (u = i/(frames-1)), 同一台手机先后跑
// 「低清+放大」和「原生高清」两遍, 只要按**同一个帧序号**取帧, 两边看到的就是同一个位姿。
// 但手机上没有现成的办法知道某张截图是第几帧 —— adb screencap 拿到的是「那一刻正在显示的」,
// 两遍帧率不同, 按时间对不上。所以在层里做:
//
//   帧序号 = 载体落 started 标记 (预热结束、进被测窗口) 之后的第几次 present。
//   层在每次 present 时 stat 一下标记文件 (应用自己的外部 files 目录, 应用进程里看得见),
//   第一次看见它的那次 present 记为 0 号。
//
//   - 抓帧 (debug.knobs.framecap=1): 对选中的帧序号, 在 present 前把交换链图像拷进
//     主机可见缓冲, 等 GPU 做完, 原样写成 raw (宽×高×4 字节, 交换链格式) —— 无损,
//     是游戏交给系统的最终画面 (系统合成器再放大到屏幕之前)。
//   - 盖条码 (debug.knobs.framecap_stamp=1): 抓完之后在左上角用 copy 盖一排黑白方块,
//     编码帧序号。录屏 (adb screenrecord) 里每一帧都能读出序号, 两遍录屏就能按序号对齐
//     合成并排视频。抓到的 raw 是盖码**之前**拷的, 所以画质数据不受条码影响。
//   - 每次 present 记一行 (帧序号, CLOCK_MONOTONIC ns), 供视频叠加逐帧帧率。
//
// 选帧规则 (属性, 挂层前设, CreateInstance 时读一次):
//   debug.knobs.framecap_every   每隔几帧抓一张 (缺省 60; 0 = 不按间隔抓)
//   debug.knobs.framecap_burst   形如 "600:2": 每 600 帧的整数倍处再抓前后各 2 帧,
//                                用来事后量「两遍之间的对齐误差」(缺省 "600:2"; "0" 关)
//   debug.knobs.framecap_max     帧序号上限, 超过不抓 (缺省 100000)
//   debug.knobs.framecap_marker  标记文件名 (应用外部 files 目录下), 缺省 megacity_started
//
// 代价 (所以抓帧遍只产画面, 不进性能统计): 每次 present 多一次 submit + 一个信号量;
// 抓帧那一帧要等 GPU 做完再写 ~8MB 文件, 那一帧会卡几十毫秒。相机位姿不读 deltaTime,
// 卡顿不改变走过的路径。
//
// 安全: 要往交换链 usage 里加 TRANSFER_SRC|TRANSFER_DST。表面不支持就不加、整个功能停用
// (照常出帧, 自报 disabled 原因), 绝不让游戏的交换链建不出来。
#pragma once
#include <sys/stat.h>
#include <time.h>

typedef VkResult(VKAPI_PTR* RkSetDeviceLoaderData)(VkDevice device, void* object);

struct FcSlot {                       // 每张交换链图像一套 (信号量复用安全: 该图像再次被
    VkCommandBuffer cb = VK_NULL_HANDLE;   // acquire 时, 上一次 present 对它的等待必然已完成)
    VkFence fence = VK_NULL_HANDLE;
    VkSemaphore sem = VK_NULL_HANDLE;
    bool used = false;
};
struct FcSwap {
    uint32_t w = 0, h = 0, fmt = 0, transform = 0;
    bool usage_ok = false;
    std::vector<VkImage> imgs;
    std::vector<FcSlot> slots;
};
struct FcState {
    bool on = false, stamp = false;
    uint32_t every = 60, burst_every = 600, burst_r = 2, max_idx = 100000;
    std::string marker = "megacity_started";
    std::string dir;                  // 输出目录
    int64_t base = -1;                // 0 号帧的 present 序号 (g_frames)
    std::string disabled;             // 非空 = 停用原因
    uint64_t captured = 0, stamped = 0, write_fail = 0;
    VkDevice dev = VK_NULL_HANDLE;
    RkSetDeviceLoaderData set_loader_data = nullptr;
    std::map<VkQueue, uint32_t> queue_family;
    std::map<uint32_t, VkCommandPool> pools;
    std::map<VkSwapchainKHR, FcSwap> swaps;
    // 主机可见回读缓冲 + 盖码用的黑白方块缓冲
    VkBuffer rb = VK_NULL_HANDLE; VkDeviceMemory rb_mem = VK_NULL_HANDLE; void* rb_map = nullptr;
    VkDeviceSize rb_size = 0; bool rb_coherent = true;
    VkBuffer tile = VK_NULL_HANDLE; VkDeviceMemory tile_mem = VK_NULL_HANDLE; uint32_t tile_fmt_kind = 0;
    FILE* log = nullptr;              // 每次 present 一行: idx,mono_ns
    // 下层函数 (按需经 gdpa 取, 不往 DevDisp 里塞)
    PFN_vkCreateCommandPool CreateCommandPool = nullptr;
    PFN_vkAllocateCommandBuffers AllocateCommandBuffers = nullptr;
    PFN_vkBeginCommandBuffer BeginCommandBuffer = nullptr;
    PFN_vkEndCommandBuffer EndCommandBuffer = nullptr;
    PFN_vkResetCommandBuffer ResetCommandBuffer = nullptr;
    PFN_vkCreateFence CreateFence = nullptr;
    PFN_vkWaitForFences WaitForFences = nullptr;
    PFN_vkResetFences ResetFences = nullptr;
    PFN_vkCreateSemaphore CreateSemaphore = nullptr;
    PFN_vkCreateBuffer CreateBuffer = nullptr;
    PFN_vkGetBufferMemoryRequirements GetBufferMemoryRequirements = nullptr;
    PFN_vkBindBufferMemory BindBufferMemory = nullptr;
    PFN_vkMapMemory MapMemory = nullptr;
    PFN_vkInvalidateMappedMemoryRanges InvalidateMappedMemoryRanges = nullptr;
    PFN_vkCmdCopyImageToBuffer CmdCopyImageToBuffer = nullptr;
    PFN_vkCmdCopyBufferToImage CmdCopyBufferToImage = nullptr;
    PFN_vkCmdPipelineBarrier CmdPipelineBarrier = nullptr;
    PFN_vkQueueSubmit QueueSubmit = nullptr;
    PFN_vkGetSwapchainImagesKHR GetSwapchainImagesKHR = nullptr;
    PFN_vkGetDeviceQueue GetDeviceQueue = nullptr;
};
static FcState g_fc;

// 条码几何 (交换链像素坐标): 左上角起, 一排 FC_NT 个 FC_T×FC_T 方块。
//   [白][黑] 起始守卫 + 16 位帧序号 (高位在前, 1=白) + 1 位偶校验 + [白] 结束守卫
// 方块够大 (系统合成器再放大 ~1.3 倍、录屏 H.264 压缩后中心依然分得清)。
static const uint32_t FC_T = 24, FC_X0 = 16, FC_Y0 = 16, FC_BITS = 16, FC_NT = 2 + FC_BITS + 1 + 1;

static uint32_t fc_prop_u32(const char* name, uint32_t def) {
    char v[PROP_VALUE_MAX] = {0};
    if (__system_property_get(name, v) <= 0 || !v[0]) return def;
    return (uint32_t)strtoul(v, nullptr, 10);
}

static void fc_read_props() {
    g_fc.on = read_bool_prop("debug.knobs.framecap");
    g_fc.stamp = read_bool_prop("debug.knobs.framecap_stamp");
    if (!g_fc.on && !g_fc.stamp) return;
    g_fc.every = fc_prop_u32("debug.knobs.framecap_every", 60);
    g_fc.max_idx = fc_prop_u32("debug.knobs.framecap_max", 100000);
    char v[PROP_VALUE_MAX] = {0};
    if (__system_property_get("debug.knobs.framecap_burst", v) > 0 && v[0]) {
        unsigned a = 0, b = 0;
        if (sscanf(v, "%u:%u", &a, &b) == 2) { g_fc.burst_every = a; g_fc.burst_r = b; }
        else { g_fc.burst_every = 0; g_fc.burst_r = 0; }
    }
    char m[PROP_VALUE_MAX] = {0};
    if (__system_property_get("debug.knobs.framecap_marker", m) > 0 && m[0]) g_fc.marker = m;
    RK_LOG("framecap: cap=%d stamp=%d every=%u burst=%u:%u max=%u marker=%s", (int)g_fc.on, (int)g_fc.stamp,
           g_fc.every, g_fc.burst_every, g_fc.burst_r, g_fc.max_idx, g_fc.marker.c_str());
}

static bool fc_want(uint64_t idx) {
    if (!g_fc.on || idx > g_fc.max_idx) return false;
    if (g_fc.every && idx % g_fc.every == 0) return true;
    if (g_fc.burst_every) {   // 每个 burst_every 整倍数的前后 burst_r 帧
        uint64_t m = idx % g_fc.burst_every;
        if (m <= g_fc.burst_r || m + g_fc.burst_r >= g_fc.burst_every) return true;
    }
    return false;
}

static std::string fc_dir() {
    return "/storage/emulated/0/Android/data/" + self_pkg() + "/files/framecap";
}

// 自报 (落进 knobs_layer_out.json 的 "framecap" 字段)
static std::string fc_report() {
    if (!g_fc.on && !g_fc.stamp) return "";
    std::lock_guard<std::mutex> lk(g_mtx);
    uint32_t w = 0, h = 0, fmt = 0, tr = 0; bool uok = false;
    if (!g_fc.swaps.empty()) { auto& s = g_fc.swaps.rbegin()->second; w = s.w; h = s.h; fmt = s.fmt; tr = s.transform; uok = s.usage_ok; }
    char b[640];
    snprintf(b, sizeof b,
             ",\"framecap\":{\"cap\":%s,\"stamp\":%s,\"every\":%u,\"burst\":\"%u:%u\",\"base_present\":%lld,"
             "\"captured\":%llu,\"stamped\":%llu,\"write_fail\":%llu,\"swap\":[%u,%u,%u],\"pre_transform\":%u,"
             "\"usage_ok\":%s,\"disabled\":%s%s%s,\"tile\":[%u,%u,%u,%u]}",
             g_fc.on ? "true" : "false", g_fc.stamp ? "true" : "false", g_fc.every, g_fc.burst_every, g_fc.burst_r,
             (long long)g_fc.base, (unsigned long long)g_fc.captured, (unsigned long long)g_fc.stamped,
             (unsigned long long)g_fc.write_fail, w, h, fmt, tr, uok ? "true" : "false",
             g_fc.disabled.empty() ? "" : "\"", g_fc.disabled.empty() ? "null" : g_fc.disabled.c_str(),
             g_fc.disabled.empty() ? "" : "\"", FC_X0, FC_Y0, FC_T, FC_NT);
    return b;
}

static void fc_disable(const std::string& why) {
    if (g_fc.disabled.empty()) { g_fc.disabled = why; RK_LOG("framecap 停用: %s", why.c_str()); }
}

// CreateDevice 时调: 记下 loader 的 SetDeviceLoaderData 回调 (自建命令缓冲是可分发对象, 要它补 loader 表)
static void fc_on_create_device(const VkDeviceCreateInfo* ci, VkDevice dev, PFN_vkGetDeviceProcAddr gdpa) {
    if (!g_fc.on && !g_fc.stamp) return;
    for (auto* p = reinterpret_cast<const RkDevCreateInfo*>(ci->pNext); p;
         p = reinterpret_cast<const RkDevCreateInfo*>(p->pNext)) {
        if (p->sType == VK_STRUCTURE_TYPE_LOADER_DEVICE_CREATE_INFO && (int)p->function == 1) {
            memcpy(&g_fc.set_loader_data, &p->u, sizeof(g_fc.set_loader_data));
        }
    }
    // 只有一个队列族时直接记下 (GetDeviceQueue 钩子再补)
    if (ci->queueCreateInfoCount == 1) g_fc.pools[ci->pQueueCreateInfos[0].queueFamilyIndex] = VK_NULL_HANDLE;
    g_fc.dev = dev;
#define FC_GET(f, T, n) g_fc.f = (T)gdpa(dev, n)
    FC_GET(CreateCommandPool, PFN_vkCreateCommandPool, "vkCreateCommandPool");
    FC_GET(AllocateCommandBuffers, PFN_vkAllocateCommandBuffers, "vkAllocateCommandBuffers");
    FC_GET(BeginCommandBuffer, PFN_vkBeginCommandBuffer, "vkBeginCommandBuffer");
    FC_GET(EndCommandBuffer, PFN_vkEndCommandBuffer, "vkEndCommandBuffer");
    FC_GET(ResetCommandBuffer, PFN_vkResetCommandBuffer, "vkResetCommandBuffer");
    FC_GET(CreateFence, PFN_vkCreateFence, "vkCreateFence");
    FC_GET(WaitForFences, PFN_vkWaitForFences, "vkWaitForFences");
    FC_GET(ResetFences, PFN_vkResetFences, "vkResetFences");
    FC_GET(CreateSemaphore, PFN_vkCreateSemaphore, "vkCreateSemaphore");
    FC_GET(CreateBuffer, PFN_vkCreateBuffer, "vkCreateBuffer");
    FC_GET(GetBufferMemoryRequirements, PFN_vkGetBufferMemoryRequirements, "vkGetBufferMemoryRequirements");
    FC_GET(BindBufferMemory, PFN_vkBindBufferMemory, "vkBindBufferMemory");
    FC_GET(MapMemory, PFN_vkMapMemory, "vkMapMemory");
    FC_GET(InvalidateMappedMemoryRanges, PFN_vkInvalidateMappedMemoryRanges, "vkInvalidateMappedMemoryRanges");
    FC_GET(CmdCopyImageToBuffer, PFN_vkCmdCopyImageToBuffer, "vkCmdCopyImageToBuffer");
    FC_GET(CmdCopyBufferToImage, PFN_vkCmdCopyBufferToImage, "vkCmdCopyBufferToImage");
    FC_GET(CmdPipelineBarrier, PFN_vkCmdPipelineBarrier, "vkCmdPipelineBarrier");
    FC_GET(QueueSubmit, PFN_vkQueueSubmit, "vkQueueSubmit");
    FC_GET(GetSwapchainImagesKHR, PFN_vkGetSwapchainImagesKHR, "vkGetSwapchainImagesKHR");
    FC_GET(GetDeviceQueue, PFN_vkGetDeviceQueue, "vkGetDeviceQueue");
#undef FC_GET
    if (!g_fc.CreateCommandPool || !g_fc.AllocateCommandBuffers || !g_fc.QueueSubmit || !g_fc.CmdCopyBufferToImage ||
        !g_fc.CmdCopyImageToBuffer || !g_fc.CmdPipelineBarrier || !g_fc.GetSwapchainImagesKHR || !g_fc.CreateSemaphore)
        fc_disable("下层函数取不全");
}

static VKAPI_ATTR void VKAPI_CALL rk_GetDeviceQueue(VkDevice dev, uint32_t fam, uint32_t idx, VkQueue* q) {
    if (!g_fc.GetDeviceQueue) { DevDisp d; if (dev_of(key(dev), &d) && d.gdpa) g_fc.GetDeviceQueue = (PFN_vkGetDeviceQueue)d.gdpa(dev, "vkGetDeviceQueue"); }
    if (!g_fc.GetDeviceQueue) return;
    g_fc.GetDeviceQueue(dev, fam, idx, q);
    if (q && *q) { std::lock_guard<std::mutex> lk(g_mtx); g_fc.queue_family[*q] = fam; }
}

// 建交换链前: 表面支持才加 TRANSFER_SRC|DST。返回值写进 FcSwap.usage_ok。
static bool fc_patch_swapchain_usage(VkDevice dev, VkSwapchainCreateInfoKHR* ci) {
    if (!g_fc.on && !g_fc.stamp) return false;
    InstDisp id; VkPhysicalDevice phys = VK_NULL_HANDLE;
    {
        std::lock_guard<std::mutex> lk(g_mtx);
        if (g_inst.empty()) return false;
        id = g_inst.begin()->second;
        auto it = g_dev.find(key(dev));
        if (it == g_dev.end() && g_dev.size() == 1) it = g_dev.begin();
        if (it != g_dev.end()) phys = it->second.phys;
    }
    auto caps_fn = (PFN_vkGetPhysicalDeviceSurfaceCapabilitiesKHR)id.gipa(id.instance, "vkGetPhysicalDeviceSurfaceCapabilitiesKHR");
    if (!caps_fn || !phys) { fc_disable("查不到表面能力"); return false; }
    VkSurfaceCapabilitiesKHR caps{};
    if (caps_fn(phys, ci->surface, &caps) != VK_SUCCESS) { fc_disable("查表面能力失败"); return false; }
    const VkImageUsageFlags need = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT;
    if ((caps.supportedUsageFlags & need) != need) { fc_disable("表面不支持 TRANSFER_SRC|DST usage"); return false; }
    ci->imageUsage |= need;
    return true;
}

static void fc_on_swapchain(VkSwapchainKHR sc, const VkSwapchainCreateInfoKHR* ci, bool usage_ok) {
    if (!g_fc.on && !g_fc.stamp) return;
    std::lock_guard<std::mutex> lk(g_mtx);
    FcSwap s; s.w = ci->imageExtent.width; s.h = ci->imageExtent.height; s.fmt = (uint32_t)ci->imageFormat;
    s.transform = (uint32_t)ci->preTransform; s.usage_ok = usage_ok;
    g_fc.swaps[sc] = s;
    RK_LOG("framecap: swapchain %ux%u fmt=%u preTransform=%u usage_ok=%d", s.w, s.h, s.fmt, s.transform, (int)usage_ok);
}

static int fc_mem_type(const DevDisp& d, uint32_t bits, VkMemoryPropertyFlags want, VkMemoryPropertyFlags* got) {
    VkPhysicalDeviceMemoryProperties mp{};
    d.GetPhysMemProps(d.phys, &mp);
    for (uint32_t i = 0; i < mp.memoryTypeCount; i++)
        if ((bits & (1u << i)) && (mp.memoryTypes[i].propertyFlags & want) == want) { if (got) *got = mp.memoryTypes[i].propertyFlags; return (int)i; }
    return -1;
}

static bool fc_make_buffer(const DevDisp& d, VkDeviceSize size, VkBufferUsageFlags usage, bool prefer_cached,
                           VkBuffer* buf, VkDeviceMemory* mem, void** map, bool* coherent) {
    VkBufferCreateInfo bci{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO};
    bci.size = size; bci.usage = usage; bci.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
    if (g_fc.CreateBuffer(d.dev, &bci, nullptr, buf) != VK_SUCCESS) return false;
    VkMemoryRequirements mr{};
    g_fc.GetBufferMemoryRequirements(d.dev, *buf, &mr);
    VkMemoryPropertyFlags got = 0;
    int mt = -1;
    if (prefer_cached) mt = fc_mem_type(d, mr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_CACHED_BIT, &got);
    if (mt < 0) mt = fc_mem_type(d, mr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT, &got);
    if (mt < 0) return false;
    VkMemoryAllocateInfo mai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};
    mai.allocationSize = mr.size; mai.memoryTypeIndex = (uint32_t)mt;
    if (d.AllocateMemory(d.dev, &mai, nullptr, mem) != VK_SUCCESS) return false;
    if (g_fc.BindBufferMemory(d.dev, *buf, *mem, 0) != VK_SUCCESS) return false;
    if (g_fc.MapMemory(d.dev, *mem, 0, VK_WHOLE_SIZE, 0, map) != VK_SUCCESS) return false;
    if (coherent) *coherent = (got & VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) != 0;
    return true;
}

// 黑白方块的像素值: 只支持 4 字节格式。8 位 RGBA/BGRA: 白=FFFFFFFF, 黑=FF000000 (小端: A 在第 4 字节,
// 两种通道序一样); A2B10G10R10/A2R10G10B10: 白=FFFFFFFF, 黑=C0000000。其它格式不盖码。
static uint32_t fc_fmt_kind(uint32_t fmt) {
    if ((fmt >= 37 && fmt <= 50)) return 1;      // R8G8B8A8_* / B8G8R8A8_*
    if (fmt >= 58 && fmt <= 69) return 2;        // A2R10G10B10_* / A2B10G10R10_*
    if (fmt >= 51 && fmt <= 57) return 1;        // A8B8G8R8_*_PACK32
    return 0;
}

static bool fc_ensure(const DevDisp& d, VkQueue q, VkSwapchainKHR sc, FcSwap& s, uint32_t img_idx, uint32_t* fam_out) {
    if (!g_fc.disabled.empty()) return false;
    if (!s.usage_ok) return false;
    uint32_t fam = UINT32_MAX;
    auto qf = g_fc.queue_family.find(q);
    if (qf != g_fc.queue_family.end()) fam = qf->second;
    else if (g_fc.pools.size() == 1) fam = g_fc.pools.begin()->first;
    if (fam == UINT32_MAX) { fc_disable("不知道 present 队列属于哪个队列族"); return false; }
    *fam_out = fam;
    VkCommandPool& pool = g_fc.pools[fam];
    if (pool == VK_NULL_HANDLE) {
        VkCommandPoolCreateInfo pci{VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO};
        pci.flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT; pci.queueFamilyIndex = fam;
        if (g_fc.CreateCommandPool(d.dev, &pci, nullptr, &pool) != VK_SUCCESS) { fc_disable("建命令池失败"); return false; }
    }
    if (s.imgs.empty()) {
        uint32_t n = 0;
        g_fc.GetSwapchainImagesKHR(d.dev, sc, &n, nullptr);
        s.imgs.resize(n);
        g_fc.GetSwapchainImagesKHR(d.dev, sc, &n, s.imgs.data());
        s.slots.resize(n);
    }
    if (img_idx >= s.slots.size()) return false;
    FcSlot& sl = s.slots[img_idx];
    if (sl.cb == VK_NULL_HANDLE) {
        VkCommandBufferAllocateInfo ai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO};
        ai.commandPool = pool; ai.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY; ai.commandBufferCount = 1;
        if (g_fc.AllocateCommandBuffers(d.dev, &ai, &sl.cb) != VK_SUCCESS) { fc_disable("分配命令缓冲失败"); return false; }
        // 可分发对象: 补 loader 分发表 (没有回调就照 loader 约定抄 device 的)
        if (g_fc.set_loader_data) g_fc.set_loader_data(d.dev, sl.cb);
        else *reinterpret_cast<void**>(sl.cb) = *reinterpret_cast<void**>(d.dev);
        VkFenceCreateInfo fci{VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; fci.flags = VK_FENCE_CREATE_SIGNALED_BIT;
        VkSemaphoreCreateInfo sci{VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO};
        if (g_fc.CreateFence(d.dev, &fci, nullptr, &sl.fence) != VK_SUCCESS ||
            g_fc.CreateSemaphore(d.dev, &sci, nullptr, &sl.sem) != VK_SUCCESS) { fc_disable("建 fence/信号量失败"); return false; }
    }
    if (g_fc.stamp && g_fc.tile == VK_NULL_HANDLE) {
        uint32_t kind = fc_fmt_kind(s.fmt);
        if (!kind) { fc_disable("交换链格式不认识, 不盖码 fmt=" + std::to_string(s.fmt)); return false; }
        void* map = nullptr; bool coh = true;
        VkDeviceSize sz = (VkDeviceSize)FC_T * FC_T * 4 * 2;
        if (!fc_make_buffer(d, sz, VK_BUFFER_USAGE_TRANSFER_SRC_BIT, false, &g_fc.tile, &g_fc.tile_mem, &map, &coh) || !coh) {
            fc_disable("建条码缓冲失败"); return false;
        }
        uint32_t* px = (uint32_t*)map;
        uint32_t white = 0xFFFFFFFFu, black = kind == 2 ? 0xC0000000u : 0xFF000000u;
        for (uint32_t i = 0; i < FC_T * FC_T; i++) { px[i] = white; px[FC_T * FC_T + i] = black; }
        g_fc.tile_fmt_kind = kind;
    }
    if (g_fc.on) {
        VkDeviceSize need = (VkDeviceSize)s.w * s.h * 4;
        if (fc_fmt_kind(s.fmt) == 0) { fc_disable("交换链格式不是 4 字节, 不抓 fmt=" + std::to_string(s.fmt)); return false; }
        if (g_fc.rb == VK_NULL_HANDLE || g_fc.rb_size < need) {
            if (g_fc.rb != VK_NULL_HANDLE) { fc_disable("交换链尺寸变大, 不重建回读缓冲"); return false; }
            if (!fc_make_buffer(d, need, VK_BUFFER_USAGE_TRANSFER_DST_BIT, true, &g_fc.rb, &g_fc.rb_mem, &g_fc.rb_map, &g_fc.rb_coherent)) {
                fc_disable("建回读缓冲失败"); return false;
            }
            g_fc.rb_size = need;
        }
    }
    return true;
}

static void fc_barrier(VkCommandBuffer cb, VkImage img, VkImageLayout from, VkImageLayout to,
                       VkAccessFlags sa, VkAccessFlags da, VkPipelineStageFlags ss, VkPipelineStageFlags ds) {
    VkImageMemoryBarrier b{VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER};
    b.srcAccessMask = sa; b.dstAccessMask = da; b.oldLayout = from; b.newLayout = to;
    b.srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED; b.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
    b.image = img; b.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    g_fc.CmdPipelineBarrier(cb, ss, ds, 0, 0, nullptr, 0, nullptr, 1, &b);
}

static void fc_write_raw(uint64_t idx, const FcSwap& s, uint64_t mono_ns) {
    char path[512];
    snprintf(path, sizeof path, "%s/f%06llu.raw", g_fc.dir.c_str(), (unsigned long long)idx);
    FILE* o = fopen(path, "wb");
    if (!o) { g_fc.write_fail++; return; }
    size_t n = (size_t)s.w * s.h * 4;
    size_t wr = fwrite(g_fc.rb_map, 1, n, o);
    fclose(o);
    if (wr != n) { g_fc.write_fail++; return; }
    g_fc.captured++;
    char ipath[512];
    snprintf(ipath, sizeof ipath, "%s/index.csv", g_fc.dir.c_str());
    if (FILE* ix = fopen(ipath, "a")) {
        fprintf(ix, "%llu,%u,%u,%u,%u,%llu\n", (unsigned long long)idx, s.w, s.h, s.fmt, s.transform, (unsigned long long)mono_ns);
        fclose(ix);
    }
}

static uint64_t fc_mono_ns() {
    struct timespec ts; clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

// present 钩子里调。可能改写 present 的等待信号量 —— 返回要传给下层的 present info。
// sub/subsem 由调用方提供存储 (生命周期覆盖下层 present 调用)。
static const VkPresentInfoKHR* fc_on_present(const DevDisp& d, VkQueue q, uint64_t f, const VkPresentInfoKHR* pi,
                                             VkPresentInfoKHR* sub, VkSemaphore* subsem) {
    if ((!g_fc.on && !g_fc.stamp) || !pi) return pi;
    uint64_t now = fc_mono_ns();
    if (g_fc.base < 0) {
        std::string m = "/storage/emulated/0/Android/data/" + self_pkg() + "/files/" + g_fc.marker;
        struct stat st;
        if (stat(m.c_str(), &st) != 0) return pi;
        g_fc.base = (int64_t)f;
        g_fc.dir = fc_dir();
        mkdir(g_fc.dir.c_str(), 0775);
        std::string lp = g_fc.dir + "/present_log.csv";
        g_fc.log = fopen(lp.c_str(), "w");
        RK_LOG("framecap: 看到标记 %s, 0 号帧 = 第 %llu 次 present", m.c_str(), (unsigned long long)f);
    }
    uint64_t idx = f - (uint64_t)g_fc.base;
    if (g_fc.log) { fprintf(g_fc.log, "%llu,%llu\n", (unsigned long long)idx, (unsigned long long)now); if (idx % 30 == 0) fflush(g_fc.log); }
    if (pi->swapchainCount != 1) return pi;
    bool cap = fc_want(idx);
    bool stamp = g_fc.stamp && idx < (1u << FC_BITS);
    if (!cap && !stamp) return pi;

    std::lock_guard<std::mutex> lk(g_mtx);
    auto si = g_fc.swaps.find(pi->pSwapchains[0]);
    if (si == g_fc.swaps.end()) return pi;
    FcSwap& s = si->second;
    uint32_t ii = pi->pImageIndices[0], fam = 0;
    if (!fc_ensure(d, q, pi->pSwapchains[0], s, ii, &fam)) return pi;
    FcSlot& sl = s.slots[ii];
    if (sl.used) {
        // 同一张图像上一轮的 submit 必然早已完成 (它已被 acquire 回来), 这里正常不会等
        g_fc.WaitForFences(d.dev, 1, &sl.fence, VK_TRUE, 2000000000ull);
    }
    g_fc.ResetFences(d.dev, 1, &sl.fence);
    g_fc.ResetCommandBuffer(sl.cb, 0);
    VkCommandBufferBeginInfo bi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    bi.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    g_fc.BeginCommandBuffer(sl.cb, &bi);
    VkImage img = s.imgs[ii];
    const VkPipelineStageFlags T = VK_PIPELINE_STAGE_TRANSFER_BIT;
    VkImageLayout cur = VK_IMAGE_LAYOUT_PRESENT_SRC_KHR;
    if (cap) {
        fc_barrier(sl.cb, img, cur, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, 0, VK_ACCESS_TRANSFER_READ_BIT, T, T);
        cur = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
        VkBufferImageCopy r{};
        r.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
        r.imageExtent = {s.w, s.h, 1};
        g_fc.CmdCopyImageToBuffer(sl.cb, img, cur, g_fc.rb, 1, &r);
    }
    if (stamp) {
        fc_barrier(sl.cb, img, cur, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                   cap ? VK_ACCESS_TRANSFER_READ_BIT : 0, VK_ACCESS_TRANSFER_WRITE_BIT, T, T);
        cur = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;
        VkBufferImageCopy rs[FC_NT]{};
        uint32_t bits[FC_NT];
        uint32_t par = 0, k = 0;
        bits[k++] = 1; bits[k++] = 0;
        for (int b = FC_BITS - 1; b >= 0; b--) { uint32_t v = (uint32_t)(idx >> b) & 1u; par ^= v; bits[k++] = v; }
        bits[k++] = par; bits[k++] = 1;
        uint32_t n = 0;
        for (uint32_t t = 0; t < FC_NT; t++) {
            uint32_t x = FC_X0 + t * FC_T;
            if (x + FC_T > s.w || FC_Y0 + FC_T > s.h) break;
            VkBufferImageCopy& r = rs[n++];
            r.bufferOffset = bits[t] ? 0 : (VkDeviceSize)FC_T * FC_T * 4;
            r.bufferRowLength = FC_T; r.bufferImageHeight = FC_T;
            r.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
            r.imageOffset = {(int32_t)x, (int32_t)FC_Y0, 0};
            r.imageExtent = {FC_T, FC_T, 1};
        }
        if (n) g_fc.CmdCopyBufferToImage(sl.cb, g_fc.tile, img, cur, n, rs);
    }
    fc_barrier(sl.cb, img, cur, VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
               VK_ACCESS_TRANSFER_READ_BIT | VK_ACCESS_TRANSFER_WRITE_BIT, 0, T, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT);
    g_fc.EndCommandBuffer(sl.cb);

    std::vector<VkPipelineStageFlags> ws(pi->waitSemaphoreCount, T);
    VkSubmitInfo sub_i{VK_STRUCTURE_TYPE_SUBMIT_INFO};
    sub_i.waitSemaphoreCount = pi->waitSemaphoreCount; sub_i.pWaitSemaphores = pi->pWaitSemaphores;
    sub_i.pWaitDstStageMask = ws.data();
    sub_i.commandBufferCount = 1; sub_i.pCommandBuffers = &sl.cb;
    sub_i.signalSemaphoreCount = 1; sub_i.pSignalSemaphores = &sl.sem;
    VkResult r = g_fc.QueueSubmit(q, 1, &sub_i, sl.fence);
    if (r != VK_SUCCESS) { fc_disable("submit 失败 " + std::to_string((int)r)); return pi; }
    sl.used = true;
    if (stamp) g_fc.stamped++;
    if (cap) {
        g_fc.WaitForFences(d.dev, 1, &sl.fence, VK_TRUE, 5000000000ull);
        if (!g_fc.rb_coherent && g_fc.InvalidateMappedMemoryRanges) {
            VkMappedMemoryRange mr{VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE};
            mr.memory = g_fc.rb_mem; mr.offset = 0; mr.size = VK_WHOLE_SIZE;
            g_fc.InvalidateMappedMemoryRanges(d.dev, 1, &mr);
        }
        fc_write_raw(idx, s, now);
    }
    *sub = *pi;
    *subsem = sl.sem;
    sub->waitSemaphoreCount = 1;
    sub->pWaitSemaphores = subsem;
    return sub;
}
