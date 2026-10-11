# Finite storage-input → sampled-input SPIR-V adapter

Refs: [HGamey/.github#14](https://github.com/HGamey/.github/issues/14), [harness epic #11](https://github.com/HGamey/.github/issues/11). Builds on [dynamic artifact SPEC](SPEC_DYNAMIC_UPOP_ARTIFACT.md).

## Scope and status

This is an offline IR transformation, frozen adaptation record and explicit frozen-SPIR-V input to the existing offline layer builder. It does not select/deploy a candidate, call a device/model, qualify a candidate, or prove pixel equivalence. The sampled-source build route is retained. A storage source is never recompiled to claim this adapted ABI. GOL driver integration and source/validator attestation remain separate work.

Six actual Naga-generated SR candidates observed during the campaign integration use the same `OpTypeImage` and `OpTypePointer` for input and output. The input is loaded directly, consumed only by `OpImageRead` and `OpImageQuerySize`, with a second unused direct load. Changing the shared original image type in place would also break the storage output.

## Support matrix

| Item | Supported | Rejected |
| --- | --- | --- |
| Module ABI | Single compute `main`, fixed 16×16×1, Logical/GLSL450, set0 bindings0/1 | Additional descriptors, buffers, push constants, specialization constants, other entry/local modes |
| Input | Direct global UniformConstant pointer to readonly float32 Rgba8 storage image2D | Missing readonly, writable/readwrite input, arrays, MS, depth, other formats/types |
| Output | Existing writeonly float32 Rgba8 storage image2D at binding1 | Readable output or other ABI |
| Resource uses | Direct `OpLoad` without memory operands; unused load allowed | AccessChain, CopyObject/phi/select/resource alias, function resource argument, other pointer use |
| Loaded input uses | `OpImageRead` without optional operands (int32×2 coordinate, float32×4 result); `OpImageQuerySize` (int32×2 result) | Image writes, sampling/filtering/gather, atomics, optional image masks, resource arguments/aliases, any unknown consumer |

Only typed `Operand::IdRef` edges establish resource use; numeric enum/literal words are never interpreted as IDs. Unsupported constructs fail explicitly, even when apparently dead.

## Transformation

1. Validate source resource shape and its complete typed direct-use graph. Validate ID uniqueness, definitions and header bounds. The adapter is not a complete SPIR-V semantic or shader safety validator.
2. Allocate fresh IDs; clone an input `OpTypeImage` with `Sampled=1` and `ImageFormat=Unknown`, add a combined `OpTypeSampledImage` and UniformConstant pointer. Keep the original image/pointer types for the output.
3. Change only the input variable pointer and remove its `NonWritable` decoration. Each direct input load returns the combined sampled image under a fresh ID, immediately followed by `OpImage` under the original load ID. Existing read/size SSA consumers thus refer to the extracted image.
4. Rewrite `OpImageRead` to `OpImageFetch` with explicit integer `Lod 0`, and `OpImageQuerySize` to `OpImageQuerySizeLod` with the same zero. No filtering or normalization is introduced. All other original instructions are preserved, including output types, loads, size queries and writes.
5. Assemble/reparse and run the sampled-input ABI reflection gate. Record original and transformed hashes, original/shared type IDs and changed operation counts. These structural checks are not a Vulkan validator or execution/quality proof; image format/view, bounds and device semantics require later equivalence testing.

## CLI, freeze and integrity

`phonefarm gray-artifact adapt-storage --source original.comp --spirv original.spv --out DIR [--json]` snapshots both bounded regular files, transforms the in-memory original SPIR-V and publishes an atomic, flat adaptation directory only after checks succeed. Source is bounded at4MiB, original/adapted SPIR-V at16MiB, SPIR-V instruction count at131072 before parser allocation, metadata at64KiB. Input def-use indexing is constructed in one pass rather than scanning the whole module once per load; no subprocess/compiler is invoked. Failed adaptation never publishes a successful manifest.

`phonefarm gray-artifact verify-adaptation --artifact DIR [--json]` verifies the exact file set, regular files/no symlinks, bounded reads, descriptor ID and file hashes, reruns the deterministic adapter on the original snapshot and checks the adapted bytes and metadata. An existing matching directory may be reused only after these checks; a directory name alone does not prove identity.

Files: `original.comp`, `original.spv`, `adapted.spv`, `descriptor.json`, `manifest.json`. The descriptor freezes adapter protocol version, hashes, ABI and transformation counts. Its SHA-256 is the adaptation ID. The manifest always records `deployable:false`, `device_validation:not_run`, `semantic_equivalence:not_run`, `qualification:not_run`, and `source_spirv_association:caller_supplied_unverified`. Hashing the source/SPIR-V pair proves file integrity, not compilation provenance or passage through GOL's safety gate. Future driver attestation must bind the original source and original SPIR-V hashes to a successful validator run before any deployment path.

`phonefarm gray-artifact build --adapted-artifact DIR --out DIR [--ndk DIR]` is mutually exclusive with `--source`. It verifies and snapshots all adaptation files, replays the deterministic transformation, archives original source/SPIR-V and adaptation descriptor/manifest hashes in its build descriptor, and embeds the exact adapted SPIR-V. The candidate compile log explicitly records that the original storage source was not recompiled. The `adaptation` origin field retains the unverified source association and unaccepted equivalence/qualification/device statuses; the build manifest route is `adapted_spirv`. The legacy compiled-source descriptor omits this optional field, preserving its previous identity format. The fixed copy shader still requires `glslc`, and the native layer still requires the Android NDK. Successful controlled compiler fixtures remain orchestration evidence only, and every resulting manifest remains `deployable:false`.

## Acceptance and remaining gates

Generate real storage SPIR-V with the same Naga30 GLSL frontend/validator/SPIR-V backend as GOL, including the real SR template. Verify shared input/output types stay storage for output, input types are cloned, load/read/size transformations are typed, output instructions remain unchanged, output reparses and reflects the new ABI. Reject aliases, phi/function parameters, extra image operands, wrong resources/entry/local size, duplicate/missing IDs and input/output decoration changes. External-process CLI tests cover successful freeze/reuse/verify, mismatched caller-supplied source status, tampering, wrong inputs and bounded/nonregular files without device/API access.

Neither `spirv-val` nor an Android NDK/GPU is available for this phase. Real Naga code generation and structural parsing are verified offline; independent Vulkan validation, actual layer construction, pixel equivalence, descriptor lifetime synchronization, device lease/rollback and paired game measurements remain unaccepted. Issue#14 stays open.
