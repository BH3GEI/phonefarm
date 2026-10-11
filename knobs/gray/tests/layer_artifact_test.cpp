// Host callbacks exercise the actual layer hooks. No Vulkan driver, Android runtime or device.
#include "../layer/vk_layer_refknobs.cpp"
#include <cassert>
template<typename T> static T h(uintptr_t x) { return reinterpret_cast<T>(x); }
static VkResult next_result = VK_SUCCESS;
static uint32_t dispatched = 0, shader_created = 0;
static VkDescriptorSet bound_set = VK_NULL_HANDLE;
static VkResult VKAPI_CALL submit(VkQueue,uint32_t,const VkSubmitInfo*,VkFence) { return next_result; }
static VkResult VKAPI_CALL begin(VkCommandBuffer,const VkCommandBufferBeginInfo*) { return next_result; }
static VkResult VKAPI_CALL reset_cb(VkCommandBuffer,VkCommandBufferResetFlags) { return next_result; }
static VkResult VKAPI_CALL reset_pool(VkDevice,VkCommandPool,VkCommandPoolResetFlags) { return next_result; }
static void VKAPI_CALL destroy_pool(VkDevice,VkCommandPool,const VkAllocationCallbacks*) {}
static void VKAPI_CALL free_cbs(VkDevice,VkCommandPool,uint32_t,const VkCommandBuffer*) {}
static void VKAPI_CALL execute(VkCommandBuffer,uint32_t,const VkCommandBuffer*) {}
static VkResult VKAPI_CALL sm(VkDevice,const VkShaderModuleCreateInfo*,const VkAllocationCallbacks*,VkShaderModule* out) { ++shader_created; *out=h<VkShaderModule>(1); return VK_SUCCESS; }
static VkResult VKAPI_CALL dsl(VkDevice,const VkDescriptorSetLayoutCreateInfo*,const VkAllocationCallbacks*,VkDescriptorSetLayout* out) { *out=h<VkDescriptorSetLayout>(2); return VK_SUCCESS; }
static VkResult VKAPI_CALL pl(VkDevice,const VkPipelineLayoutCreateInfo*,const VkAllocationCallbacks*,VkPipelineLayout* out) { *out=h<VkPipelineLayout>(3); return VK_SUCCESS; }
static VkResult VKAPI_CALL pipeline(VkDevice,VkPipelineCache,uint32_t,const VkComputePipelineCreateInfo*,const VkAllocationCallbacks*,VkPipeline* out) { *out=h<VkPipeline>(4); return next_result; }
static VkResult VKAPI_CALL dp(VkDevice,const VkDescriptorPoolCreateInfo*,const VkAllocationCallbacks*,VkDescriptorPool*) { return VK_ERROR_OUT_OF_POOL_MEMORY; }
static void VKAPI_CALL update(VkDevice,uint32_t,const VkWriteDescriptorSet*,uint32_t,const VkCopyDescriptorSet*) {}
static void VKAPI_CALL barrier(VkCommandBuffer,VkPipelineStageFlags,VkPipelineStageFlags,VkDependencyFlags,uint32_t,const VkMemoryBarrier*,uint32_t,const VkBufferMemoryBarrier*,uint32_t,const VkImageMemoryBarrier*) {}
static void VKAPI_CALL bindpipe(VkCommandBuffer,VkPipelineBindPoint,VkPipeline) {}
static void VKAPI_CALL bindsets(VkCommandBuffer,VkPipelineBindPoint,VkPipelineLayout,uint32_t,uint32_t n,const VkDescriptorSet* sets,uint32_t,const uint32_t*) { if(n) bound_set=sets[0]; }
static void VKAPI_CALL dispatch(VkCommandBuffer,uint32_t,uint32_t,uint32_t) { ++dispatched; }
int main() {
    void* dispatch_key = h<void*>(0x100);
    void* device_handle = dispatch_key; void* primary_handle = dispatch_key;
    void* secondary_handle = dispatch_key; void* queue_handle = dispatch_key;
    auto dev = reinterpret_cast<VkDevice>(&device_handle);
    auto primary = reinterpret_cast<VkCommandBuffer>(&primary_handle);
    auto secondary = reinterpret_cast<VkCommandBuffer>(&secondary_handle);
    auto queue = reinterpret_cast<VkQueue>(&queue_handle);
    DevDisp d; d.dev=dev; d.QueueSubmit=submit; d.BeginCommandBuffer=begin;
    d.ResetCommandBuffer=reset_cb; d.ResetCommandPool=reset_pool; d.DestroyCommandPool=destroy_pool;
    d.FreeCommandBuffers=free_cbs; d.CmdExecuteCommands=execute;
    d.CreateShaderModule=sm; d.CreateDescriptorSetLayout=dsl; d.CreatePipelineLayout=pl;
    d.CreateComputePipelines=pipeline; d.CreateDescriptorPool=dp;
    d.UpdateDescriptorSets=update; d.CmdPipelineBarrier=barrier; d.CmdBindPipeline=bindpipe;
    d.CmdBindDescriptorSets=bindsets; d.CmdDispatch=dispatch;
    g_dev[dispatch_key]=d; g_upop=true; g_dump_passes=true;
    // Pipeline success is counted even when a later allocation fails.
    assert(!copyprobe_ensure(dev,d,16,16,VK_FORMAT_R8G8B8A8_UNORM));
    assert(g_artifact_stats.pipeline_created==1 && g_artifact_stats.disabled==1);
    g_cp=CopyProbe{}; next_result=VK_ERROR_INITIALIZATION_FAILED;
    assert(!copyprobe_ensure(dev,d,16,16,VK_FORMAT_R8G8B8A8_UNORM));
    assert(g_artifact_stats.pipeline_created==1);
    next_result=VK_SUCCESS; g_cp=CopyProbe{};
#ifdef RK_DYNAMIC_UPOP_BUILD
    g_upop=false; auto before=shader_created;
    assert(!copyprobe_ensure(dev,d,16,16,VK_FORMAT_R8G8B8A8_UNORM));
    assert(shader_created==before); // Never use copy / fixed upop instead of candidate.
    g_upop=true; g_cp=CopyProbe{};
#endif
    auto source=h<VkImage>(10); auto view=h<VkImageView>(11); auto game=h<VkDescriptorSet>(12);
    auto clone=h<VkDescriptorSet>(13); auto layout=h<VkDescriptorSetLayout>(14);
    g_cp.dev=dev;g_cp.pipe=h<VkPipeline>(4);g_cp.w=16;g_cp.h=16;g_cp.fmt=VK_FORMAT_R8G8B8A8_UNORM;
    g_cp.dst=h<VkImage>(20);g_cp.dst_view=h<VkImageView>(21);g_cp.cset=h<VkDescriptorSet>(22);
    g_swap_w=16;g_swap_h=16;g_view[view]=source;
    g_img[source]={8,8,VK_FORMAT_R8G8B8A8_UNORM,VK_IMAGE_USAGE_SAMPLED_BIT};
    g_set_views[game][0]=view;g_set_samplers[game][0]=h<VkSampler>(23);
    copyprobe_dispatch(secondary,d,VK_NULL_HANDLE,{view});
    assert(dispatched==1 && g_artifact_stats.dispatch_recorded==1);
    g_set_layout[game]=layout;g_layout_bindings[layout]={{0,1,VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER}};
    g_cp.clones[game]={clone,0};g_cb_cur_is_swap[primary]=true;
    rk_CmdBindDescriptorSets(primary,VK_PIPELINE_BIND_POINT_GRAPHICS,VK_NULL_HANDLE,0,1,&game,0,nullptr);
    assert(bound_set==clone && g_artifact_stats.substituted==1);
    rk_CmdExecuteCommands(primary,1,&secondary);
    VkSubmitInfo si{VK_STRUCTURE_TYPE_SUBMIT_INFO};si.commandBufferCount=1;si.pCommandBuffers=&primary;
    next_result=VK_ERROR_DEVICE_LOST;assert(rk_QueueSubmit(queue,1,&si,VK_NULL_HANDLE)==next_result);
    assert(g_artifact_stats.queue_submitted==0);
    next_result=VK_SUCCESS;rk_QueueSubmit(queue,1,&si,VK_NULL_HANDLE);rk_QueueSubmit(queue,1,&si,VK_NULL_HANDLE);
    assert(g_artifact_stats.queue_submitted==2);
    next_result=VK_ERROR_INITIALIZATION_FAILED;rk_BeginCommandBuffer(primary,nullptr);
    assert(g_artifact_stats.submitted_count({reinterpret_cast<uintptr_t>(primary)})==1);
    next_result=VK_SUCCESS;rk_BeginCommandBuffer(primary,nullptr);
    assert(g_artifact_stats.submitted_count({reinterpret_cast<uintptr_t>(primary)})==0);
    rk_ResetCommandBuffer(secondary,0);assert(g_artifact_stats.pending.empty());
    auto pool=h<VkCommandPool>(30);
    g_artifact_stats.allocated(reinterpret_cast<uintptr_t>(primary),reinterpret_cast<uintptr_t>(pool));
    g_artifact_stats.record(reinterpret_cast<uintptr_t>(primary));rk_ResetCommandPool(dev,pool,0);
    assert(g_artifact_stats.pending.empty());
    g_artifact_stats.record(reinterpret_cast<uintptr_t>(primary));rk_FreeCommandBuffers(dev,pool,1,&primary);
    assert(g_artifact_stats.pending.empty());
    g_artifact_stats.allocated(reinterpret_cast<uintptr_t>(primary),reinterpret_cast<uintptr_t>(pool));
    g_artifact_stats.record(reinterpret_cast<uintptr_t>(primary));rk_DestroyCommandPool(dev,pool,nullptr);
    assert(g_artifact_stats.pending.empty() && g_artifact_stats.pools.empty());
    void* other_handle=dispatch_key;
    auto other_dev=reinterpret_cast<VkDevice>(&other_handle);
    assert(!copyprobe_ensure(other_dev,d,16,16,VK_FORMAT_R8G8B8A8_UNORM));
    assert(g_cp.disabled && dispatched==1); // Reject foreign-device handles before any GPU operation.
    g_dump_passes=false;write_marker();puts(rk_host_marker.c_str());
}
