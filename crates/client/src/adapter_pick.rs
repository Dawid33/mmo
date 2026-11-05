//! Which GPU the native client renders on when none is pinned.
//!
//! Bevy takes `WGPU_ADAPTER_NAME` (a name substring) and, when it is unset or
//! matches no adapter, falls back to wgpu's high-performance default — on this
//! laptop the NVIDIA T600 on Mesa's NVK driver, ~30× slower on the voxel pass
//! than even the Intel iGPU, whenever the RX 6700 eGPU is unplugged (8 fps on
//! `just run`, 2026-11-05). So before Bevy starts, [`pin_adapter`] checks the
//! wanted name against the adapters actually present and, if it names none,
//! pins the best by [`rank`]: a discrete GPU (not on NVK) > integrated > any
//! other GPU (NVK) > software. A name that is present is left alone.

use wgpu::{AdapterInfo, DeviceType};

/// Preference of an adapter when none is pinned; higher is better.
pub fn rank(info: &AdapterInfo) -> u8 {
    let nvk = info.driver.to_ascii_lowercase().contains("nvk") || info.name.contains("NVK");
    match info.device_type {
        DeviceType::DiscreteGpu if !nvk => 4,
        DeviceType::IntegratedGpu => 3,
        DeviceType::DiscreteGpu | DeviceType::VirtualGpu | DeviceType::Other => 2,
        DeviceType::Cpu => 0,
    }
}

/// The adapter name to pin: `wanted` if it matches one of `adapters` (Bevy's
/// case-insensitive substring rule), else the best-ranked one's name; `None`
/// when there is nothing to choose from.
pub fn choose(wanted: Option<&str>, adapters: &[AdapterInfo]) -> Option<String> {
    if let Some(wanted) = wanted.filter(|w| !w.is_empty()) {
        let wanted = wanted.to_lowercase();
        if adapters.iter().any(|a| a.name.to_lowercase().contains(&wanted)) {
            return None;
        }
    }
    adapters.iter().max_by_key(|a| rank(a)).map(|a| a.name.clone())
}

/// Set `WGPU_ADAPTER_NAME` to the adapter [`choose`] picks. Call first thing in
/// `main`, before any thread exists (it writes the process environment).
pub fn pin_adapter() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::all());
    let adapters: Vec<AdapterInfo> = bevy::tasks::block_on(instance.enumerate_adapters(backends))
        .iter()
        .map(|a| a.get_info())
        .collect();
    let wanted = std::env::var("WGPU_ADAPTER_NAME").ok();
    if let Some(name) = choose(wanted.as_deref(), &adapters) {
        if let Some(wanted) = wanted.filter(|w| !w.is_empty()) {
            eprintln!("[adapter] no GPU matches WGPU_ADAPTER_NAME={wanted:?}; using {name:?}");
        }
        std::env::set_var("WGPU_ADAPTER_NAME", name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(name: &str, driver: &str, device_type: DeviceType) -> AdapterInfo {
        AdapterInfo {
            name: name.into(),
            vendor: 0,
            device: 0,
            device_type,
            device_pci_bus_id: String::new(),
            driver: driver.into(),
            driver_info: String::new(),
            backend: wgpu::Backend::Vulkan,
            subgroup_min_size: 0,
            subgroup_max_size: 0,
            transient_saves_memory: false,
        }
    }

    #[test]
    fn integrated_beats_nvk_and_a_real_discrete_beats_both() {
        let intel = adapter("Intel(R) Iris(R) Xe Graphics", "Intel open-source Mesa driver", DeviceType::IntegratedGpu);
        let nvk = adapter("NVIDIA T600 Laptop GPU (NVK TU117)", "NVK", DeviceType::DiscreteGpu);
        let cpu = adapter("llvmpipe", "llvmpipe", DeviceType::Cpu);
        let amd = adapter("AMD Radeon RX 6700 XT", "radv", DeviceType::DiscreteGpu);
        let laptop = [nvk.clone(), intel.clone(), cpu.clone()];
        assert_eq!(choose(None, &laptop).as_deref(), Some(intel.name.as_str()));
        // A pinned name that is absent (eGPU unplugged) falls back the same way.
        assert_eq!(choose(Some("6700"), &laptop).as_deref(), Some(intel.name.as_str()));
        // A present name is left to Bevy.
        assert_eq!(choose(Some("t600"), &laptop), None);
        let docked = [nvk, intel, cpu, amd.clone()];
        assert_eq!(choose(None, &docked).as_deref(), Some(amd.name.as_str()));
        assert_eq!(choose(None, &[]), None);
    }
}
