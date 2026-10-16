# openEuler HDBSS for E2B

Base: FPR commit 2160f28ac66e7735a16c425aa8ab86276451c998.

`--hdbss-order 1` enables vendor KVM capability502 once, after all vCPUs exist and before any run. Cold boot and restore share this setup. Only arm64 / 4KiB host pages / cap502=1 are accepted. Order1..9 allocates 8KiB..2MiB per vCPU; invalid or unsupported requested configurations fail, without silent fallback. Host buffer addresses are not snapshot state. Do not use dirty-ring with this branch.

`--kvm-dirty-bitmap` enables the same cumulative KVM/VMM bitmap export without HDBSS, for the software control arm. HDBSS implies this export. Both require memory slots with dirty logging; the paired E2B branch enables `track_dirty_pages` and `enable_diff_snapshots`. The custom dirty export is 4KiB only.

Kernel dirty-log harvest is accumulated per memory slot for the VM lifetime and unions VMM dirty pages before they can be reset. Full snapshots in this mode retain tracking; `/memory/dirty` unions the cumulative log with existing pagemap/ UFFD-WP selection. Failed repeated exports cannot lose already-harvested kernel bits. This deliberately over-approximates dirty epochs and retains the old pagemap scan. Go-side UFFD REMOVE/discard handling remains essential. No cold-page policy, host swap, live migration protocol or dirty-ring consumer is added.

The VMM seccomp policy permits KVM_ENABLE_CAP. Custom seccomp filters must permit it too. `cargo test -p vmm hdbss -- --include-ignored` includes an ignored host test for capability initialization, rejection of untracked slots and retention of VMM dirty writes across repeated collection/reset. The paired E2B branch includes a disposable real-guest-code KVM dirty-log stress probe and full intranet acceptance instructions under scripts/overcommit/hdbss.

Companion Firecracker validation on agent03 (Rust 1.85.0, aarch64-unknown-linux-gnu): `cargo check -p firecracker`, `cargo build -p firecracker`, and `cargo test -p vmm --lib --no-run` passed with --locked. The GNU debug binary uses the repository's empty debug seccomp policy; this does not validate the production musl seccomp filter.

Targeted tests passed: `rejects_invalid_orders` and real-KVM `hdbss_vm_initialization --ignored`. The latter enables capability 502 after creating a vCPU, rejects untracked slots, and checks that VMM dirty writes survive repeated harvest and userspace bitmap reset.

Debug GNU binary CLI help exposes the new flags. SHA256: `a11dbae41db088cc4093e97dfb4af42840f80e65c1542800da101a30d0e84e49`. Artifact: `/home/coder/projects/yangshengxin/e2b/validation-20261016/firecracker-hdbss`.
