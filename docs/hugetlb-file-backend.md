# Experimental 2 MiB file-backed snapshot restore

Branch base: feat-fpr at 2160f28ac. This adds a private hugetlbfs mapping to the
existing File snapshot backend. The snapshot's huge_pages setting stays 2M;
there is no new snapshot serialization or backend enum.

The backing file must reside on a 2 MiB hugetlbfs filesystem (a hugetlb memfd also
qualifies), have no write permission bits, exactly match the sum of memory
regions, and have 2 MiB aligned region sizes and guest addresses. The loader
opens it read-only and maps it PROT_READ|PROT_WRITE, MAP_PRIVATE. Unlike the
existing anonymous backend, this mapping does not add MAP_NORESERVE. Template
storage and future private COW pages require a deliberately sized hugepage pool.
Reservations are conservative; this feature does not implement memory admission
control, cgroup accounting policy or guaranteed overcommit.

The orchestrator must publish a fully initialized inode and never truncate or
modify it while consumers exist. All same-template VMs must see the same inode,
including across mount namespaces. A read-only mode is not protection against a
privileged writer. Snapshot export refuses to write to any currently mapped
backing inode, including aliases, to prevent corrupting shared template data.

For this first version, balloon/FPR remain disabled. Existing configuration and
restore checks rejecting balloon + hugepages are retained: merely setting FPR
false on a snapshot that still contains a balloon device is not sufficient.
Use templates created without the balloon device. No hugepage discard/remap or
FPR support is claimed. The paired E2B integration uses full memory export;
UFFD-WP-based dirty tracking must not be applied to these private file mappings.

Validation on agent03 (aarch64 Linux 6.6, Rust 1.85.0): native debug build and 9
memory unit tests passed. Paired Go tests execute a real 1-vCPU/64-MiB KVM guest:
2 MiB host backing, equal PFNs after guest reads, private PFNs after guest writes,
unchanged template, full process_vm_readv export and restore. They also reject
ordinary-file backing and template overwrite. See the paired infra branch's
hugetlb-results.md for logs and limitations, including libc/seccomp testing.

This is not a production musl release or a full Linux guest/API/envd acceptance
result. Existing UFFD compatibility issues on the target kernel remain separate.
