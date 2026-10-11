# Finite Android NDK r28c export profile

Refs system [#14](https://github.com/HGamey/.github/issues/14) and [#11](https://github.com/HGamey/.github/issues/11). This input preparation tool supports only `ndk-r28c-darwin-arm64-api30-v1`; it does not make arbitrary NDK layouts acceptable or change GOL35 regular-file limits. SPEC precedes implementation.

## Observed incompatibility

The actual Google SDKmanager `ndk;28.2.13676358` installation has 8,897 entries, 2,962,187,103 regular-file bytes and 23 internal symlinks. GOL35 permits at most 4,096 entries, 512 MiB and no symlinks. Its refusal is correct. The complete original NDK stays intact. APK carrier builds use that official installation independently.

## Explicit closure

Export the existing API30 aarch64 C++ wrapper; materialize only the exact original alias chains `clang++ -> clang -> clang-19`, `ld.lld -> lld`, and `llvm-readelf -> llvm-readobj`. The resulting filenames preserve driver/linker invocation semantics. Keep llvm-nm and glslc, three explicit local Mach-O dylibs (libc++, libc++abi, libxml2.2), complete Clang19 headers/aarch64 runtime directory and aarch64 builtins archive, complete Android C/C++ header tree and complete aarch64 sysroot library directory (including its API stubs), plus original license notices/source.properties. Do not retain other architectures, debugger, Python, unused tools or opaque four-executable substitutes. This profile serves PF's frozen Android-arm64-v8a/API30 non-sanitized native build flags; it is not a general NDK, not an APK C compiler profile and not valid for other targets/flags.

The original package revision and exact alias texts must match. Read and copy through regular no-follow descriptors with bounded streaming and before/after inode/size/mtime identity; parent components cannot be symlinks. Refuse special files, unknown symlinks, unreadable entries, source mutation, escapes, depth>16, entries>4096, output bytes>512 MiB or metadata>2 MiB. Maintain a fixed whole export timer between stream blocks (regular filesystem syscalls are not a hard real-time sandbox). Selected directory inventories are rechecked before publication. Export to a newly created private directory, never cover an existing path or symlink. On error persist an incomplete negative receipt; never publish a complete manifest or execute partial tools.

Every output file records original path/size/SHA256 and alias chain if any, output size/SHA256, original package identity and selected policy. Manifest resides outside the SDK tree so it is not a compiler input. Verification rehashes exact file set, regular type/permissions and provenance association; unsupported or missing data fail closed. Package name, file hashes and versions do not authenticate an untrusted supplied toolchain. Source origin is declared `caller_supplied_unverified`; separate host installation evidence documents the official URL/checksum verified installation. Synthetic fixtures must remain explicitly synthetic, never codegen or deployment evidence.

## Runtime and acceptance scope

Known host loader dependencies from real `otool -L` include system libSystem/libz and NDK dylibs. System library/shared-cache resolution, shell/interpreter binaries, OS and deliberate detached/networking behavior are outside this profile; trusted local tooling is required. Read-only files do not sandbox the same user. Exact compiler version/hashes and PF actual descriptor tool/flags/staged-source hashes must be recorded during real acceptance.

Run exporter counterexamples for links/escapes/mutation/limits/partial output and manifest contradictions. Run actual GOL35 private compile/verify -> actual PF adapt/verify -> inherited-group actual NDK native build/verify using this exported regular profile, with final outer-group retirement and hashes. No fake NDK or fake successful PF substitutes. Validate real ELF ARM64, symbols, DT_NEEDED and SPIR-V with installed spirv-val. Host Android code generation can be recorded as executed; GPU, quality, semantic, device, real-game, deployability and promotion proof stay false/not_run. Do not relax limits if the closure does not fit.

## Usage and actual host acceptance (2026-10-12)

```sh
python3 tools/export_ndk_profile.py export --ndk /absolute/official/r28c --out /new/profile
python3 tools/export_ndk_profile.py verify --out /new/profile
# Supply /new/profile/sdk to GOL candidate-prepare --ndk and --build.
```

The actual installed closure exports 3,490 entries and 483,954,086 bytes, within the unchanged limits. Real GOL35 (source70141d5, binary0b6a45…) and actual PF24 legacy4 (db1721…) completed six stages in137.77s; the accepted integrated PF ownership5 (sourcef88188c, binary5abd653…) did so in130.96s. Both use the same profile and actual NDK code generation, with no `--synthetic` flag. Original/adapted/copy SPIR-V passed the installed spirv-val's Vulkan1.1 static validation. The complete original NDK still receives an actual BLOCKED result; no native helpers are launched after its refusal.

The existing GOL35 journal conservatively retains `android_codegen_validated=false` and `real_ndk_validation=not_run`; those accepted raw receipts were not edited. The separate host acceptance summary records the observed real compiler execution and ARM64 ELF only. This is no GPU validation, semantic/image-quality acceptance, injection readiness or commercial-game result. The actual refbench PR2 APK is independently built from ad251d8, not from this C++-only profile. Data and raw local paths are documented in tasks/ndk_r28c_profile_20261012; large executables/SDK/ELF/APK stay outside version control.
