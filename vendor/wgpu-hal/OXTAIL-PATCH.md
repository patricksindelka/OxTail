# wgpu-hal 29.0.4, patched for OxTail

This is wgpu-hal 29.0.4 from crates.io (MIT OR Apache-2.0, see `LICENSE.MIT` and
`LICENSE.APACHE`; copyright the gfx-rs developers), used through
`[patch.crates-io]` in the workspace `Cargo.toml`.

## The change

On DirectX 12, wgpu-hal gives every bind group that holds samplers a small
buffer of sampler indices, and shaders read it for every texture sample. The
buffer is CPU-written (`MAP_WRITE`), so it was placed in write-combined memory.
That is fine for a GPU. With WARP, the software rasterizer Windows uses when
there is no GPU (virtual machines, Remote Desktop), the shaders run on the CPU
and read that memory uncached: OxTail spent about 85% of its CPU time stalled
on those reads, roughly 3 µs of CPU per pixel per frame, which made redraws
cost whole cores.

On cache-coherent unified memory the buffer now goes into write-back, cached
memory instead (a committed buffer in a custom heap, write-back pages in L0),
which the GPU reads coherently; that is also what D3D12 uses for upload heaps
there. WARP reports such memory, and so do most integrated GPUs, which take
the new path too (one small committed allocation per bind group with
samplers). Dedicated GPUs keep the upstream behaviour.

Changed files, every change marked with an `OxTail patch` comment:

- `src/dx12/suballocation.rs`: `DeviceAllocationContext::create_shader_read_buffer`.
- `src/dx12/device.rs`: `create_bind_group` uses it for the sampler index buffer.

Measured on WARP (`OXTAIL_WGPU_ADAPTER=software`, 1100x700 window, 32-thread
CPU): CPU per redraw went from 2.2 s to about 0.1 s; moving the mouse across
the window from 8 cores to under half of one.

## Upgrading wgpu

Copy the new release from `~/.cargo/registry/src/*/wgpu-hal-<version>/` over this
directory (keep this file and the licenses; drop `.cargo-ok`,
`.cargo_vcs_info.json`, `Cargo.toml.orig` and `Cargo.lock`), and re-apply the
change if upstream has not fixed the placement. Then check on WARP:

```sh
OXTAIL_WGPU_ADAPTER="basic render" oxtail some.log
```

(`software` would take the first CPU adapter of any backend, which may be a
Vulkan one such as SwiftShader rather than WARP.) The change is Windows-only,
so the Linux CI job does not compile it; the Windows builds do.

Moving the mouse over the menu bar and tabs should cost a fraction of a core.
