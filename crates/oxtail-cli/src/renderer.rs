//! Renderer selection and the wgpu-to-OpenGL fallback (PLAN.md section 3.1).
//!
//! `winit` allows one event loop per process, so a failed wgpu start cannot be
//! retried in the same process. Instead:
//!
//! 1. In `auto` mode the presence of a wgpu adapter is probed *before* any
//!    window exists ([`wgpu_adapter_available`]); no adapter means OpenGL from
//!    the start.
//! 2. If wgpu still fails while creating the window, the process re-runs
//!    itself with `--renderer glow` ([`retry_with_glow`]).

use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Duration;

use oxtail_config::Renderer;

/// Environment variable set on the re-executed process, so a failing OpenGL
/// start is not retried forever.
pub const RETRY_ENV: &str = "OXTAIL_RENDERER_RETRY";

/// How long the adapter probe may take before wgpu counts as unavailable.
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// What to run first and whether a failure may fall back to OpenGL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// The renderer to start with.
    pub first: eframe::Renderer,
    /// If `first` fails before the window exists, retry with OpenGL.
    pub fall_back: bool,
}

/// Decides the plan for `choice`. `wgpu_ok` is the result of the adapter
/// probe (only consulted in `auto` mode); `is_retry` is true in the
/// re-executed process.
pub fn plan(choice: Renderer, wgpu_ok: impl FnOnce() -> bool, is_retry: bool) -> Plan {
    match choice {
        Renderer::Wgpu => Plan {
            first: eframe::Renderer::Wgpu,
            fall_back: false,
        },
        Renderer::Glow => Plan {
            first: eframe::Renderer::Glow,
            fall_back: false,
        },
        Renderer::Auto => {
            if is_retry || !wgpu_ok() {
                Plan {
                    first: eframe::Renderer::Glow,
                    fall_back: false,
                }
            } else {
                Plan {
                    first: eframe::Renderer::Wgpu,
                    fall_back: true,
                }
            }
        }
    }
}

/// Whether wgpu can find a GPU adapter (Vulkan, Metal, DX12 or GL through
/// wgpu). Runs on a helper thread so a hanging driver cannot hang the start.
pub fn wgpu_adapter_available() -> bool {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("oxtail-gpu-probe".into())
        .spawn(move || {
            let instance = eframe::wgpu::Instance::new(
                eframe::wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
            );
            let found = pollster::block_on(
                instance.request_adapter(&eframe::wgpu::RequestAdapterOptions::default()),
            )
            .is_ok();
            let _ = tx.send(found);
        });
    spawned.is_ok() && rx.recv_timeout(PROBE_TIMEOUT).unwrap_or(false)
}

/// Runs this program again with `--renderer glow`.
///
/// On Unix the current process is replaced (so the single-instance socket is
/// released and stdin is kept). Elsewhere a child process runs to completion
/// and its exit status is returned. Returns an error only if the program
/// could not be started.
pub fn retry_with_glow() -> std::io::Result<ExitCode> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(std::env::args_os().skip(1))
        .arg("--renderer")
        .arg("glow")
        .env(RETRY_ENV, "1");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // `exec` only returns on failure.
        Err(cmd.exec())
    }
    #[cfg(not(unix))]
    {
        // The single-instance pipe of this process would swallow the
        // child's requests: let the child run without it.
        cmd.arg("--new-instance");
        let status = cmd.status()?;
        Ok(ExitCode::from(
            u8::try_from(status.code().unwrap_or(1)).unwrap_or(1),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_choices_never_probe_or_fall_back() {
        let p = plan(Renderer::Wgpu, || panic!("must not probe"), false);
        assert_eq!(
            p,
            Plan {
                first: eframe::Renderer::Wgpu,
                fall_back: false
            }
        );
        let p = plan(Renderer::Glow, || panic!("must not probe"), false);
        assert_eq!(p.first, eframe::Renderer::Glow);
        assert!(!p.fall_back);
    }

    #[test]
    fn auto_prefers_wgpu_with_a_fallback_when_an_adapter_exists() {
        let p = plan(Renderer::Auto, || true, false);
        assert_eq!(p.first, eframe::Renderer::Wgpu);
        assert!(p.fall_back);
    }

    #[test]
    fn auto_uses_opengl_without_an_adapter() {
        let p = plan(Renderer::Auto, || false, false);
        assert_eq!(p.first, eframe::Renderer::Glow);
        assert!(!p.fall_back);
    }

    #[test]
    fn the_retry_process_goes_straight_to_opengl() {
        let p = plan(Renderer::Auto, || panic!("must not probe"), true);
        assert_eq!(p.first, eframe::Renderer::Glow);
        assert!(!p.fall_back);
    }
}
