# External RAM checkpoints

When `PUT /snapshot/create` omits `mem_file_path`, the VMM freezes its
in-process virtio RAM writers before saving CPU and device state. This applies
independently of dirty export tracking. Block `prepare_save` drains outstanding
asynchronous I/O. Net, block, balloon (including free-page reporting), vsock and
entropy event handlers preserve pending events while frozen. Resume unfreezes
and kicks devices using the existing resume path.

An external exporter may then read every logical RAM region reported by
`GET /memory/mappings`, in file-offset order. It must handle nonresident backing
pages, private swapped pages, short reads and read errors. Residency does not
establish that a page is logically zero. Keep the VM paused until all checkpoint
payloads are captured; only publish a checkpoint manifest after the payloads
are complete and durable. On export failure, terminate the paused VM or resume
it explicitly; a failed snapshot request does not unfreeze devices implicitly.

Vhost-user block devices can write outside this event loop and are rejected for
external RAM checkpoints. These changes do not introduce a persistent device
field or change the existing snapshot serialization version. Runtime freeze
flags initialize to false on construction and restore.

The paired E2B implementation uses complete logical RAM export for File and
shared read-only NBD backends. KVM dirty bitmaps and mincore/pagemap filters are
not substitutes for that export. Shared NBD restore uses an O_RDONLY backing
with MAP_PRIVATE so each VM retains private writes.

Focused regressions are named `test_external_snapshot_*` in the balloon, vsock
and entropy event-handler modules. They verify RAM and pending-event stability
while frozen and processing after thaw. Target KVM tests and application trace
acceptance remain separate from these unit tests.
