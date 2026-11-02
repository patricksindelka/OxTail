//! Which wgpu adapter OxTail renders with.
//!
//! `OXTAIL_WGPU_ADAPTER` overrides egui-wgpu's choice (hardware first):
//! `software` (or `cpu`) picks a software rasterizer such as WARP or
//! llvmpipe, anything else the first adapter whose name contains it,
//! ignoring case. This reproduces a machine without a GPU (a virtual machine
//! over Remote Desktop) on one that has one, and works around a broken
//! driver.

use std::sync::Arc;

use eframe::egui_wgpu::{NativeAdapterSelectorMethod, WgpuSetup};
use eframe::wgpu::DeviceType;

/// Environment variable choosing the wgpu adapter.
pub const ADAPTER_ENV: &str = "OXTAIL_WGPU_ADAPTER";

/// Whether an adapter called `name` of type `device_type` is the one
/// `wanted` (a value of [`ADAPTER_ENV`]) asks for.
pub fn adapter_matches(name: &str, device_type: DeviceType, wanted: &str) -> bool {
    let wanted = wanted.trim().to_lowercase();
    match wanted.as_str() {
        "software" | "cpu" => device_type == DeviceType::Cpu,
        "" => false,
        w => name.to_lowercase().contains(w),
    }
}

/// Makes eframe pick the adapter `wanted` asks for (see [`adapter_matches`]).
/// Starting fails, with the adapters there are, if none matches. A blank
/// `wanted` (the variable set but empty) leaves egui-wgpu's choice alone.
pub fn choose_adapter(options: &mut eframe::NativeOptions, wanted: String) {
    if wanted.trim().is_empty() {
        return;
    }
    let WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup else {
        return;
    };
    let select: NativeAdapterSelectorMethod = Arc::new(move |adapters, surface| {
        let usable = adapters
            .iter()
            .filter(|a| surface.is_none_or(|s| a.is_surface_supported(s)));
        for a in usable {
            let info = a.get_info();
            if adapter_matches(&info.name, info.device_type, &wanted) {
                return Ok(a.clone());
            }
        }
        let names: Vec<String> = adapters
            .iter()
            .map(|a| {
                let i = a.get_info();
                format!("{} ({:?}, {:?})", i.name, i.backend, i.device_type)
            })
            .collect();
        Err(format!(
            "no wgpu adapter that can draw to the window matches {ADAPTER_ENV}={wanted}; there are: {}",
            names.join(", ")
        ))
    });
    create.native_adapter_selector = Some(select);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_picks_cpu_adapters_only() {
        let warp = "Microsoft Basic Render Driver";
        assert!(adapter_matches(warp, DeviceType::Cpu, "software"));
        assert!(adapter_matches("llvmpipe", DeviceType::Cpu, " CPU "));
        assert!(!adapter_matches(
            "NVIDIA GeForce",
            DeviceType::DiscreteGpu,
            "software"
        ));
    }

    #[test]
    fn other_values_match_part_of_the_name() {
        assert!(adapter_matches(
            "NVIDIA GeForce RTX 3090",
            DeviceType::DiscreteGpu,
            "rtx"
        ));
        assert!(adapter_matches(
            "Microsoft Basic Render Driver",
            DeviceType::Cpu,
            "basic render"
        ));
        assert!(!adapter_matches(
            "Intel UHD",
            DeviceType::IntegratedGpu,
            "nvidia"
        ));
        assert!(!adapter_matches("anything", DeviceType::Other, "  "));
    }

    fn has_selector(options: &eframe::NativeOptions) -> bool {
        matches!(
            &options.wgpu_options.wgpu_setup,
            WgpuSetup::CreateNew(c) if c.native_adapter_selector.is_some()
        )
    }

    #[test]
    fn choosing_installs_a_selector_unless_the_value_is_blank() {
        let mut options = eframe::NativeOptions::default();
        choose_adapter(&mut options, " ".into());
        assert!(!has_selector(&options), "blank keeps egui-wgpu's choice");
        choose_adapter(&mut options, "software".into());
        assert!(has_selector(&options));
    }
}
