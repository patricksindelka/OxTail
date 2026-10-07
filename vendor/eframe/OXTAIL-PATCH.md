# eframe 0.35.0, patched for OxTail

This is eframe 0.35.0 from crates.io (MIT OR Apache-2.0, see `LICENSE-MIT` and
`LICENSE-APACHE`; copyright Emil Ernerfeldt and the egui contributors), used
through `[patch.crates-io]` in the workspace `Cargo.toml`.

## The change

A frame identical to the last one painted is not tessellated, painted or
presented again. egui runs a full frame for every mouse move, plus two more
after any input; for a log viewer those frames almost always draw the same
picture, and painting them took a whole CPU core with software rasterizers
(Remote Desktop, virtual machines, WARP).

Changed files, every change marked with an `OxTail patch` comment:

- `src/native/skip_paint.rs` (new): the comparison, the per-viewport last
  frame, and its unit tests.
- `src/native/mod.rs`: declares the module.
- `src/native/run.rs`: tracks which redraws eframe requested itself (only
  those may skip), which window events force a paint, and schedules a forced
  paint one second after a skipped frame.
- `src/native/glow_integration.rs`, `src/native/wgpu_integration.rs`: skip
  tessellating, painting and presenting an unchanged frame; forget the last
  frame when the window is hidden or the wgpu surface could not be acquired.

## Upgrading eframe

Copy the new release from `~/.cargo/registry/src/*/eframe-<version>/` over this
directory (keep this file and the licenses), re-apply the changes above, and
run the patch's tests:

```sh
cargo test --manifest-path vendor/eframe/Cargo.toml --lib skip_paint --target-dir target
```
