# Dynamic sampled-input SR artifacts

Refs [.github #14](https://github.com/HGamey/.github/issues/14) and [.github epic #11](https://github.com/HGamey/.github/issues/11). This is the offline build part of dynamic candidate deployment. It does not deploy to a device, acquire its lease, validate image quality, or prove a blackbox optimization.

## Scope and ABI

The legacy `knobs/gray/build/build_layer.sh` without arguments keeps its fixed upop build and output path. A new zero-Token `phonefarm gray-artifact build --source candidate.comp --out tasks/<task>/artifacts` compiles an explicitly sampled-input SR source into an isolated Android arm64-v8a layer artifact.

The candidate must already use this ABI. There is no regex conversion from storage-image input:

- One compute entry point `main`, exactly one fixed `LocalSize` **16×16×1**, Logical/GLSL450 memory model. Specialization constants and grouped/member decorations are currently unsupported.
- Exactly set 0 binding 0: scalar combined image sampler, 2D, float sampled image, non-array, non-multisampled.
- Exactly set 0 binding 1: scalar 2D `rgba8` storage image, write-only (`NonReadable`), non-array, non-multisampled.
- No additional descriptors, uniform/SSBO buffers, push constants or undeclared resource binding.

The compiler output is inspected as SPIR-V instructions/types/decorations, not matched as GLSL text. Source/compile errors or unsupported ABI are hard failures. ABI reflection is not a full shader correctness or termination proof; deployment requires further safety and real-game quality gates.

## Build and provenance

1. Read the requested source once into an owner-only staging directory and write a snapshot. Compilation always reads that snapshot. Check the snapshot hash again after compilation.
2. Run the selected Android NDK `glslc`; retain exact source and SPIR-V bytes. Reflect the ABI before native layer compilation.
3. Freeze a build descriptor covering source/SPIR-V hashes, reflected ABI, layer native inputs, compiler binaries/versions and normalized compiler options. Its SHA-256 is the artifact ID. Native metadata embeds the ID and source/SPIR-V hashes.
4. Build in a separate staging directory with the validated candidate as the embedded upop shader. Dynamic builds must have their metadata header and nonempty candidate SPIR-V. They never substitute the fixed legacy upop or copy shader when dynamic upop was requested. If `debug.knobs.upop` is off, the dynamic operator is inactive; a copy-only request cannot silently execute another operator.
5. Verify required exports and Android ELF dependencies. Hash the produced layer and all archived files; save a versioned manifest with build provenance and explicit `device_validation: not_run` and `deployable: false`. These offline artifacts do not qualify device deployment, including when a controlled compiler fixture is used in tests. A layer cannot embed its own final hash without a circular dependency: the native marker carries the build-descriptor/source/SPIR-V hashes, and the host manifest carries the layer SHA-256.
6. Publish only by atomic directory rename after every check succeeds. Failed builds do not publish a successful manifest. Existing artifacts are reusable only when the descriptor and all stored file hashes match; an artifact directory name alone is never identity evidence. Compiler binary hashes are checked before and after use to detect an accidental toolchain change.

Source paths, tool executable paths and subprocess arguments are passed as structured process arguments. Input files and tool output must not be accepted through a stale mutable build directory. Sources are capped at 4 MiB, SPIR-V at 16 MiB, layers/archived inputs at 128 MiB and hashed tool binaries at 256 MiB. Tool invocations have a 120-second default/hard maximum (`--timeout-seconds 1..120`), version probes at most 10 seconds, and 1 MiB stdout/stderr limits per stream. A timeout/output overflow kills that invocation’s Unix process group and retains a failed snapshot. Even when a parent exits and closes both pipes, a surviving/unknown process group is rejected and terminated before success. This is a bounded trusted-tool invocation, not a sandbox: helpers that deliberately detach with `setsid` are not controlled. Artifact verification permits flat filenames only and rejects file symlinks. Artifacts and local test evidence live under `tasks/<task>/`; binary artifacts are not committed to Git.

## Native marker semantics

External preparation drivers can explicitly select the provisional inherited
process group interface in [native build supervision](SPEC_NATIVE_BUILD_SUPERVISION.md).
Its artifact state requires the outer supervisor to establish group absence and
verify bytes again; launcher/helper exit alone is not a completed preparation.

The marker retains existing copyprobe fields and adds the artifact identity and these counts:

| Counter | Actual event represented | Does not establish |
|---|---|---|
| `pipeline_created` | `vkCreateComputePipelines` returned SUCCESS | Successful execution |
| `dispatch_recorded` | Candidate `vkCmdDispatch` was recorded | Queue submission |
| `queue_submitted` | Recorded candidate dispatches in command buffers accepted by `vkQueueSubmit` returning SUCCESS | GPU completion/display |
| `substituted` | Descriptor binding was replaced with the candidate output clone while recording commands | Displayed quality |
| `disabled` | Transition to fail-closed disabled state, with reason | A performance negative |

Re-recording/freeing command buffers clears their pending dispatch counts. Primary command buffers inherit the recorded dispatch count of executed secondaries. Re-submitting a valid recorded command buffer counts another accepted submission. Failed `vkQueueSubmit` calls add zero. This phase observes `vkQueueSubmit` only; `vkQueueSubmit2`/KHR and GPU completion are not covered, and the marker states that API scope. Counters refer to actual native control flow, not the artifact manifest or requested settings.

## Offline acceptance and boundaries

- Real SPIR-V reflection tests cover valid sampled-input ABI and wrong set/binding/type/access/format/local size/entry point, extra resources, push constants and malformed binaries.
- Cross-process CLI tests cover input snapshots, compiler errors, ABI rejection, missing tools and no fixed-shader fallback. Host native tests cover success/failure submission, secondary buffers, reset/free and disabled counters.
- With an installed Android NDK, compile a real sampled source and the Android layer and verify hashes/exports/dependencies/reuse. Missing NDK/glslc is an explicit unverified build gate; do not download an SDK or substitute the OpenHarmony toolchain automatically.
- Run the full Rust test suite. No device, root write, model API or Token is used in this phase.
- Device deployment and game_opt_loop adaptation are later stages, conditional on the device lease and receiver qualification protocol. Keep #14 open.

The native injector now owns immutable descriptor state and private output images per dispatch and retains them for the command-buffer recording generation; see [SPEC_NATIVE_DISPATCH_LIFETIME.md](SPEC_NATIVE_DISPATCH_LIFETIME.md). Host hook tests cover the ownership mechanism. They do not establish actual GPU lifetime/synchronization correctness or qualify multi-device Vulkan processes.

**Behavior change:** current production lacks verified source-layout, target-pass, and compute-state contract producers, so it refuses the previous heuristic copy/upop injection with an explicit reason. The test-only contract fixture cannot be enabled in Android builds. A later real tracker must satisfy the same identity/epoch contract before injection can resume. This offline ownership work does not complete deployment: Vulkan validation, hook/quality/equivalence and rollback evidence are still required, and `deployable: false` remains mandatory. Native input snapshots now also freeze `dispatch_ownership.h`; older artifact manifests without this dependency must be rebuilt for this version. Compiler executable hashes/versions are recorded, but the whole NDK sysroot and compiler helper dependency closure is not independently hashed. Artifact integrity is not toolchain authenticity or shader correctness.
