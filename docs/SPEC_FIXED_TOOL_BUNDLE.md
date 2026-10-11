# Bounded fixed tools: batched installation and fresh verification

Status: implementation contract. No device performance speedup is claimed.
The historical source19bf run installed14 files/39183B using43 supervised
transport calls in94.490734s. Those historical artifacts remain unchanged.

## Scope

Only the receiver's existing confirmation-driver/control-probe execution files
are transferable. The expected set is derived from purpose, registered thermal
preset and the validated control count; it is not supplied by the caller.
Prepare's loaded one-control set contains14 files. Multiple controls add only
the receiver-generated probe_N.plan.tsv entries. Capability purpose remains
control-only. Request/workload JSON, arbitrary paths and unrelated local files
cannot be uploaded through this facility.

The registered `kgsl_queued_origin_spf_v1` main phase adds exactly four names:
`render_ownership.sh`, `render_args.tsv`, `render_reader.elf` and the existing
`ftrace-trace_clock` restoration file. Both existing confirmation thermal routes
remain supported: legacy cold has16 files for one control and loaded has18.
Control-only and capability purposes cannot select it. The maximum
set is49 names for32 validated controls. The ELF receives only a receiver-fixed
0700 mode in the batched installer; every fresh verifier requires it executable
as well as matching exact length and SHA256. Verification never repairs its
mode or executes it.

The preceding native initial witness is a distinct helper-only phase. Its
five existing files remain on the bounded per-file installation path, bound
to the helper directory, boot, token and preinstall manifest. Main bundle files
do not exist yet and are excluded from that phase's expected manifest. The
helper route accepts only its exact five names before main intent; its closure,
absence verification and durable recovery barrier remain required. It cannot
call the main installer or claim a control-state restore. This integration does
not supply automatic recovery for a partially installed helper/main directory.

Before any device write, freeze tools.manifest.json, tools.install.sh and
tools.verify.sh together with every required original file, state, conditions,
restore graph and other existing immutable inputs. The manifest includes exact
names, lengths, SHA256, count, total bytes, actual boot/session ownership, and
SHA256 of the shared include_str source template for both modes. The receiver reconstructs the
bundle from the frozen originals and current templates; caller-edited manifest
and script hashes cannot change the expected set or executable logic.

The complete raw set is limited to4MiB, each file1MiB, and the generated script
is limited to the existing8MiB stdin bound. Encoded file bytes travel in fixed
base64 heredocs via the existing owned-fd stdin, not ADB arguments or a large
printf argument. All OS argv contain only bounded flags, paths and ownership
strings. Fixed generated filenames have no separators or shell metacharacters.
This is not a generic archive extractor or a general upload command.

## Installation

The existing full lease and durable recovery barrier precede installation.
The frozen host-generated installer checks required helper availability before
creating a new0700 directory, writes its owner, and checks actual owner, boot and
absence of session.closed before every file write and publication. Files are
decoded to create-only temporary entries, checked for decoded length, synced,
then renamed to their required names. No installed tool is executed here.
An exact owner/boot/bundle/count receipt and exit0 only confirm installation;
they do not substitute for the following full SHA256 acceptance.

One separate supervised command executes the frozen **host** verifier via stdin.
It does not execute an unverified remote verifier to validate itself. For every
required name it reads actual regular non-symlink files, actual length and
SHA256. The host accepts only the complete exact ordered set, matching hashes,
sizes, actual owner/boot, bundle identity, count, terminal marker and real exit0.
Unknown checksum tooling, malformed/partial/extra records or file-type changes
block. The trusted transport/source declaration is not hardware attestation.

Target call count is two for installation plus full acceptance, replacing
per-file host round trips; no wall-time or game speedup factor is predicted.
Transport permit, frozen executable/input hashing, bounded output/deadlines,
process-group supervision and lease heartbeats are unchanged.

## Collect, reconcile and failure

Collect and reconcile continue to perform **fresh actual complete verification**
of every required file. They do not reuse an installation receipt as proof of
current bytes and cannot drop required names from a supplied hash map. The same
host-generated frozen verifier is used before remote worker execution.

Partial installation, decoder failure, output limit, timeout, ownership/boot
change or unknown completion preserves the existing recovery barrier and stops
before a probe/sample. The complete expected set remains frozen even when only
some remote files exist. Existing reconcile reports BLOCKED if a required tool
is missing or changed; it does not execute that tool or infer idleness from its
absence. Automatic repair/resume and safe recovery of an incomplete initial
installation remain unimplemented. This limitation must not be described as a
full restoration. Fully verified installation follows existing positive worker
quiescence, full snapshot equality and permanent session revocation.

The order stays receiver rules/metadata -> stable warmup -> initial snapshot and
sibling restore graph -> immutable freeze -> recovery barrier -> installation
and full acceptance -> fresh typed host/worker control admission. No thermal
threshold, reference centering, whole-window qualification, restore equality or
old failed sample changes. The distinct thermal diagnostic patch is independent.
Old tools/sessions remain immutable; new bundle inputs require a new session.

## Offline acceptance

Execute generated production templates against host-owned temporary directories
with real base64/SHA256/wc, not only simulated protocol output. Positive cases
must preserve exact binary bytes and count; missing/extra/duplicate/truncated,
wrong name/size/hash, symlink/FIFO, owner/boot/closed-session cases must block.
Exercise actual bounded Transport deadline/output handling and frozen-input
mutation. The CLI explicit fake route must show two calls, full required-set
rechecks on collect/reconcile, and partial-install -> BLOCKED/barrier retained
without worker execution. Synthetic reports stay synthetic/ineligible.

Run the repository's complete cargo test and default Clippy before building and
signing a new immutable real-device tool. Offline templates/fixtures are not
Android command, thermal, GPU or game acceptance. A new separately recorded
real prepare may follow only the full source gate, review and signing.
