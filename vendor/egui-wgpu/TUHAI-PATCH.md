# TuHaiView native mesh-upload patch

Based on crates.io `egui-wgpu` 0.31.1, egui commit
`1669e52a7ccfc3489c1b0999b9ed48894a0b3887`, directory `crates/egui-wgpu`.
Original source and licenses: https://github.com/emilk/egui/tree/0.31.1

Changes are confined to `src/renderer.rs` and `src/winit.rs`:

- The native winit painter enables a 256 KiB wgpu staging belt for vertex and
  index writes. Mapped memory is filled directly, without an extra CPU copy.
- `finish` unmaps writes before frame submission. The next paint recalls the
  previous frame only after its encoder has been submitted or dropped on a
  surface error. Mapping completion is asynchronous; the UI never waits for it.
- Other renderer integrations retain the original queue-write path. Image
  textures, shaders, rendering order and image-upload budgets are unchanged.

The chunk size is not a pool-wide cap: upstream StagingBelt retains reusable
chunks and may allocate more while mapping is pending. Native buffers are
outside the application's image-texture budget; measure process private bytes
and allocator/object counts as well. Never report the image budget as a limit
on these buffers or driver memory.

The manual regression compares 32 GPU readbacks against the original path and
abandons an unsubmitted encoder every fourth frame:

```powershell
cargo test --release --locked -p egui-wgpu --features wgpu/dx12 native_mesh_reuse_matches_queue_upload_after_abandoned_frames -- --ignored --nocapture --test-threads=1
```

Local Intel UHD 630 / DX12 diagnostics isolated growing process private bytes
to repeated queue buffer uploads. An empty painter stayed stable; adding only
queue uploads reproduced growth; reusing staging buffers removed that growth
in the short diagnostic. These experiments motivate this patch but do not
substitute for final product acceptance. See `performance-results/20260906`.
