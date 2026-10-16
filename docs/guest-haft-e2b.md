# E2B guest HAFT requirement

Based on the FPR branch at 2160f28ac66e7735a16c425aa8ab86276451c998.

`--require-guest-haft` adds an opt-in guard on arm64: after applying the boot CPU template, require the KVM guest MMFR1.HAFDBS field to be 3 or 4 and TCR2 to be accessible. On restore, check again after all system registers are restored and require TCR2.HAFT bit11 on every vCPU. No hardware feature is forged and no guest register is force-enabled by the VMM. The HAFT-capable guest kernel is responsible for enabling TCR2; the paired E2B preflight requires MGLRU effective mask 0x0007 and negotiated FPR before envd starts.

This flag intentionally rejects a pre-HAFT snapshot, a CPU template hiding HAFT, a host lacking the register, or a snapshot with HAFT disabled on a vCPU. Build new templates from a suitable guest kernel. The host buffer flags used by HDBSS are independent and are not included in this branch.

Build with the repository Rust toolchain. On a compatible host, `cargo test -p vmm test_guest_haft_guard -- --ignored` tests the guard against real KVM guest registers. It does not boot Linux or prove MGLRU reclaim effectiveness. Complete guest boot/FPR and snapshot restore tests using the paired `mem-overcommit-guest-mglru-haft-fpr-20261016` E2B branch before rollout.

Companion Firecracker validation on agent03 (Rust 1.85.0, aarch64-unknown-linux-gnu): `cargo check -p firecracker`, `cargo build -p firecracker`, and `cargo test -p vmm --lib --no-run` passed with --locked. The GNU debug binary uses the repository's empty debug seccomp policy; this does not validate the production musl seccomp filter.

Real-KVM `test_guest_haft_guard --ignored` passed: guest feature/register check succeeds, restored TCR2 with HAFT clear is rejected, and TCR2.HAFT set is accepted. This tests KVM register handling, not Linux guest HAFT page-table behavior.

Debug GNU binary CLI help exposes the new flags. SHA256: `f76bf2167aa58543afaa55c586f6b1846c6c5a8de1cc0a4225258b051e2b92a9`. Artifact: `/home/coder/projects/yangshengxin/e2b/validation-20261016/firecracker-guest-haft`.
