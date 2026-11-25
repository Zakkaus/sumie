//! Checks that the pinned wgpu-hal fork lets `nextDrawable` time out on macOS. Creating a
//! surface from a bare `CAMetalLayer` needs one unsafe call, so this probe lives in its own
//! test target and the library keeps `#![forbid(unsafe_code)]`.
#![cfg(target_os = "macos")]

use std::ffi::c_void;

use objc2_quartz_core::CAMetalLayer;
use sumie_render::{SurfaceRenderer, backends, instance_descriptor, wgpu};

#[test]
fn configure_allows_next_drawable_timeout() {
    let layer = CAMetalLayer::new();
    // Start from the value stock wgpu-hal sets, so only configure can make it true.
    layer.setAllowsNextDrawableTimeout(false);
    let instance = wgpu::Instance::new(instance_descriptor(
        wgpu::InstanceDescriptor::new_without_display_handle(),
        backends(),
    ));
    let target = wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(
        std::ptr::from_ref::<CAMetalLayer>(&layer)
            .cast_mut()
            .cast::<c_void>(),
    );
    // SAFETY: `layer` is a live CAMetalLayer. wgpu-hal retains it, and `layer` is declared
    // first, so it outlives the surface regardless.
    let surface = unsafe { instance.create_surface_unsafe(target) }
        .expect("wgpu creates a Metal surface from a CAMetalLayer");
    let options = wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surface),
        ..Default::default()
    };
    let adapter = match pollster::block_on(instance.request_adapter(&options)) {
        Ok(adapter) => adapter,
        Err(err) => {
            let reason = format!("no Metal adapter: {err}");
            assert_ne!(
                std::env::var("SUMIE_REQUIRE_GPU").as_deref(),
                Ok("1"),
                "{reason}"
            );
            eprintln!("Skipping drawable timeout check: {reason}");
            return;
        }
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("the Metal adapter supplies a device");
    let renderer = SurfaceRenderer::new(surface, &adapter, &device, &queue, [64, 64])
        .expect("the CAMetalLayer surface configures");
    assert!(
        layer.allowsNextDrawableTimeout(),
        "configure left CAMetalLayer.allowsNextDrawableTimeout false, so nextDrawable can block \
         without bound; is the wgpu-hal fork in [patch.crates-io] applied?"
    );
    drop(renderer);
}
