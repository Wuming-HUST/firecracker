# openEuler HDBSS for E2B

Base: FPR commit 2160f28ac66e7735a16c425aa8ab86276451c998.

`--hdbss-order 1` enables vendor KVM capability502 once, after all vCPUs exist and before any run. Cold boot and restore share this setup. Only arm64 / 4KiB host pages / cap502=1 are accepted. Order1..9 allocates 8KiB..2MiB per vCPU; invalid or unsupported requested configurations fail, without silent fallback. Host buffer addresses are not snapshot state. Do not use dirty-ring with this branch.

`--dirty-export-policy hybrid|batched|bitmap` selects the experiment collector. `hybrid` is the compatibility default, `batched` keeps pagemap semantics but reads 32 KiB chunks, and `bitmap` omits pagemap and uses the cumulative KVM/VMM bitmap. `--kvm-dirty-bitmap` enables the software control arm; HDBSS implies this export. Both require memory slots with dirty logging; the paired E2B branch enables `track_dirty_pages` and `enable_diff_snapshots`. The custom dirty export is 4KiB only.

Kernel dirty-log harvest is accumulated per memory slot for the VM lifetime and unions VMM dirty pages before they can be reset. Full snapshots in this mode retain tracking; `/memory/dirty` unions the cumulative log with existing pagemap/UFFD-WP selection unless the explicit bitmap policy is selected. Failed repeated exports cannot lose already-harvested kernel bits. This deliberately over-approximates dirty epochs. Go-side UFFD REMOVE/discard handling remains essential. No cold-page policy, host swap, live migration protocol or dirty-ring consumer is added.

The batched and bitmap policies clone the live `vm-memory` AtomicBitmap before
reading/resetting the clone's words. They never clear the live device-write
bitmap. `DIRTY_STATS` logs KVM ioctl time, VMM merge time, pagemap scan time,
selection counts, and total collector time; API JSON serialization and export
copying are outside that total. Pagemap reads count requested batches (a short
read retry can require more kernel syscalls). Bitmap mode reports zero pagemap
reads. The small bookkeeping/allocation time remains in `pagemap_ns` even when
there is no scan.

`--uffd-wp-async` requires the newer kernel feature and registers MISSING|WP;
the companion pager preserves WP on read fault copies. This is a separate
baseline, not an HDBSS dependency. Unsupported kernels fail feature negotiation
without fallback. The current agent03 kernel returns EINVAL for this feature.

The VMM seccomp policy permits KVM_ENABLE_CAP. Custom seccomp filters must permit it too. `cargo test -p vmm hdbss -- --include-ignored` includes an ignored host test for capability initialization, rejection of untracked slots and retention of VMM dirty writes across repeated collection/reset. The paired E2B branch includes a disposable real-guest-code KVM dirty-log stress probe and full intranet acceptance instructions under scripts/overcommit/hdbss.

Companion Firecracker validation on agent03 (Rust 1.85.0, aarch64-unknown-linux-gnu): `cargo check -p firecracker`, `cargo build -p firecracker`, and `cargo test -p vmm --lib --no-run` passed with --locked. The GNU debug binary uses the repository's empty debug seccomp policy; this does not validate the production musl seccomp filter.

Targeted tests passed: `rejects_invalid_orders` and real-KVM `hdbss_vm_initialization --ignored`. The latter enables capability 502 after creating a vCPU, rejects untracked slots, and checks that VMM dirty writes survive repeated harvest and userspace bitmap reset.

Debug GNU binary CLI help exposes the new flags. SHA256: `a11dbae41db088cc4093e97dfb4af42840f80e65c1542800da101a30d0e84e49`. Artifact: `/home/coder/projects/yangshengxin/e2b/validation-20261016/firecracker-hdbss`.

## Measured collector revision, 2026-10-29

The release GNU executable used for the current matrix and four SWE mechanism
replays has SHA256
`471f3d422cd6436902cd9c00ac58b56989cc3cf1b1c312bdbc597cf37b4c255e`.
Rebuilding the final sources (including comment corrections) produced the same
hash. The target is Kunpeng 950 / CPU part 0xd06, openEuler 6.6, 4 KiB pages.
The paired E2B report is `docs/memory-lab/HDBSS-EXPERIMENT.md`; it separates
provider effects from pagemap batching/removal and includes failed runs.

External RAM export spans API requests. In the explicit dirty-export lab mode,
snapshot creation with no memory file freezes net/virtio-block event handlers
before CPU/device state is saved. Block prepare-save drains outstanding async
I/O. Resume unfreezes and kicks the devices. The freeze flag is host-only state,
initialized false on restore, and does not change the persisted snapshot schema.
A block event-handler regression checks used-ring progress before/after unfreeze.
Other devices are outside this freeze, and pending level-triggered events may
spin during export; use only for short lab pauses. This closes one state/RAM
race but did not establish stability of repeated nine-checkpoint chains.

Targeted Rust tests passed for bitmap merge/tail, non-destructive bitmap word
snapshot, pagemap chunk boundaries and present/WP filtering, invalid order and
export policy, real-KVM cap502 initialization and retained device writes, and
block freeze/unfreeze. Four real-VM SWE trace replays (software/HDBSS crossed
with hybrid/bitmap) each verified three restores, private RAM/disk hashes and
the live guest PID. The replay uses synthetic tools and scaled time, not original
SWE workload performance. No KAE compression or PMU/VM-exit benefit is claimed.
