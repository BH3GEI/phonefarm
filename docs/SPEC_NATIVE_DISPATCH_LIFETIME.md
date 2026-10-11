# Native dispatch ownership (offline acceptance)

Refs [.github #14](https://github.com/HGamey/.github/issues/14), [PR20](https://github.com/HGamey/phonefarm/pull/20), and [PR21](https://github.com/HGamey/phonefarm/pull/21). This replaces the injector's shared mutable resource group. It does not qualify deployment: artifacts remain `deployable=false`, GPU validation/equivalence remain NOT_RUN.

## Frozen contract

- One immutable device asset group (shader, compute layout/pipeline). A local owning transaction destroys partial creations before publication. Ordinary create/allocate outputs enter ownership only on SUCCESS; failed output values may be undefined and are never destroyed. Compute-pipeline partial-success outputs follow that API’s specified contract and are cleaned separately.
- A CB allocation has a device/pool/level/allocation epoch. A successful Begin creates a recording generation. Failed Begin/End/Reset preserves its old owned objects and makes uncertain recordings ineligible.
- Each dispatch owns a distinct output image/memory/view and an immutable compute descriptor set. Each recording owns its descriptor pools, dispatch groups, and immutable graphics clones. Clone identity includes dispatch identity and original set allocation/write generation.
- A descriptor is fully initialized before its first bind and never rewritten afterwards. Resize or application set update creates fresh resources; it never frees an older recording's resources.
- A recording's objects live until successful lower Begin/Reset, successful pool reset, valid lower Free/DestroyPool, or device teardown. Submission success and recorded counts are not completion proof. Executable but idle recordings retain all objects because they can be submitted again.
- Original callbacks always forward when a lower function exists. Refusing layer qualification never drops original game commands.
- If a failure occurs after commands have been injected, the layer retains referenced objects, stops further injection, and records an ineligible reason. Already recorded commands cannot be erased by disabling the feature. A clean re-record/restart is required.

## Supported identities and finite scope

First scope: a known primary CB with a known command pool/graphics+compute queue family, one device, non-SIMULTANEOUS_USE, ordinary inline render pass, known scalar combined-sampler target, plain supported descriptor layouts with no extension chains or immutable samplers. Unknown identities, secondary buffers, simultaneous recording, unobserved pool/family, descriptor indexing/update-after-bind/variable count/descriptor-buffer/push-descriptor layouts are refused.

Image/view/layout/set/pool/sampler identities include a device and monotonically assigned creation/allocation epoch. Successful create/allocate publishes metadata; successful free/reset and destruction invalidate future identity lookups. Descriptor-copy updates propagate the tracked image/sampler identity and write generation. Original layout definitions are frozen at set allocation, so a later application layout destruction cannot cause allocation from its dead handle. Graphics clones use an owned compatible layout.

Source state, target hook identity and preservation of original compute bind state are not yet proved by the legacy observer. **Production injection fails closed with an explicit unverified-contract reason.** A compile-time host-fixture seam can exercise the actual dispatch/clone/ownership callbacks with known fixture contracts; it is not exposed as a property or supported Android build flag. Host fixture success cannot grant runtime deployment qualification.

Image support in that finite fixture/mechanism is 2D RGBA8 UNORM, one sample, full mip 0/layer 0, sampled source usage, supported scalar target descriptor and a live known sampler. Unknown layout/view/source epochs are refused. Production proof of injection-point layout, actual game target and compute state is a later feature, not assumed from dimensions.

## Bounded resources

Hard caps: 256 MiB live owned output memory using `VkMemoryRequirements.size`, 64 retained nonempty recording generations, 64 dispatch groups per recording, 256 graphics clones per recording, and 4096 tracked identities per new ownership metadata family. These are not an RSS bound: the pre-existing observer tables have separate historical limits/coverage and are not comprehensively bounded by this change. Observer-wide capacity cleanup remains backlog. Metadata exhaustion explicitly invalidates the mechanism; no silent oldest-entry eviction. Owned descriptor pools are sized for frozen layouts and retained only with their generation.

Budget is checked before publication. Unreferenced partial outputs/pools/layouts roll back immediately. Referenced groups cannot be evicted for budget. Device assets and all resource groups are destroyed with their owning lower dispatch table, never another device's handles.

## Callback transitions

CreateCommandPool captures family/flags; AllocateCommandBuffers captures pool/level; successful Begin retires old generation and establishes new flags; End observes success/failure; successful Reset retires recording; successful ResetPool retires all pool recordings; Free/DestroyPool call lower then retire the affected allocations. Device teardown destroys all layer-owned pools/output groups/assets before lower DestroyDevice. Metadata lifecycle functions cover set free/pool reset/destroy, layout destroy, sampler destroy, image/view destroy, and descriptor copies. Tracking refers to Vulkan-valid application calls and their required external synchronization.

Vulkan non-pending boundaries: [Begin](https://docs.vulkan.org/refpages/latest/refpages/source/vkBeginCommandBuffer.html), [Reset](https://docs.vulkan.org/refpages/latest/refpages/source/vkResetCommandBuffer.html), [Free](https://docs.vulkan.org/refpages/latest/refpages/source/vkFreeCommandBuffers.html), [ResetPool](https://docs.vulkan.org/refpages/latest/refpages/source/vkResetCommandPool.html), [DestroyPool](https://docs.vulkan.org/refpages/latest/refpages/source/vkDestroyCommandPool.html). Descriptor update and destruction requirements: [Update](https://docs.vulkan.org/refpages/latest/refpages/source/vkUpdateDescriptorSets.html), [FreeSets](https://docs.vulkan.org/refpages/latest/refpages/source/vkFreeDescriptorSets.html). No measured-frame wait-idle is added.

## Acceptance

Actual native hook code plus mock lower dispatch must cover: same CB multiple dispatches; interleaved CBs; per-dispatch immutable descriptors/private output; original set write/copy generation changes; resize without early destruction; Begin/Reset failures retaining objects; successful Begin/Reset/ResetPool/Free/DestroyPool/device teardown releasing exactly once; partial allocation failures; handle reuse; unsupported scopes; caps; forwarding original callbacks on refusal; accepted submission versus completion distinction. Both legacy and dynamic artifact builds are compiled as host fixtures.

Full phonefarm cargo test and clippy are required. Missing host Vulkan headers is a reported NOT_RUN native gate, not native acceptance. Real Android NDK build, SPIR-V validation, Vulkan validation layers, multiple in-flight frames, copy/adapted shader pixel equivalence, actual game hook coverage and condition/rollback/independent confirmation remain required before deployment. No device, model API, SDK download or GPU execution is part of this PR.
