//! The `oxtail` binary: parses the command line, resolves the data folder,
//! makes sure only one instance runs per data folder, and starts the GUI with
//! a renderer fallback chain (wgpu, then OpenGL).

mod args;
mod ipc;
mod logging;

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use oxtail_config::{DataDir, Renderer, ResolveInput};
use oxtail_gui::{AppInit, ExternalOpen, OxTailApp};

fn main() -> ExitCode {
    let cli = match args::parse(std::env::args_os().skip(1)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("oxtail: {e}\nTry 'oxtail --help'.");
            return ExitCode::from(2);
        }
    };
    if cli.help {
        print!("{}", args::HELP);
        return ExitCode::SUCCESS;
    }
    if cli.version {
        println!("oxtail {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("oxtail: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: args::Cli) -> anyhow::Result<ExitCode> {
    let data_dir = DataDir::resolve(ResolveInput::from_environment(cli.data_dir.clone()));
    logging::init(data_dir.root.as_deref());
    if let Some(reason) = data_dir.in_memory_reason() {
        tracing::warn!("running without a data folder: {reason}");
    }
    let request = cli.to_request().absolutized();

    // Single instance. Reading stdin means this process cannot be a mere
    // forwarder, so `-` implies a private instance.
    let private = cli.new_instance || cli.stdin;
    let mut external = ExternalOpen::disconnected();
    let mut holds_lock = false;
    if !private {
        match ipc::acquire(&data_dir.instance_key(), data_dir.root.as_deref(), &request) {
            ipc::Role::Forwarded => {
                tracing::info!("handed the request to the running instance");
                return Ok(ExitCode::SUCCESS);
            }
            ipc::Role::Server(server) => {
                external = server.external;
                holds_lock = true;
            }
            ipc::Role::Standalone => {}
        }
    }
    if holds_lock {
        // No other live instance uses this data folder, so anything left in
        // `tmp/` is from a crashed run.
        let removed = data_dir.cleanup_stale_temp();
        if removed > 0 {
            tracing::info!("removed {removed} stale temporary files");
        }
    }

    let startup = oxtail_gui::load_startup(data_dir);
    let renderer = cli.renderer.unwrap_or(startup.settings.renderer);
    let init = AppInit {
        startup,
        request,
        external,
    };
    run_gui(&init, renderer)?;
    Ok(ExitCode::SUCCESS)
}

/// The renderers to try, in order.
fn renderer_chain(choice: Renderer) -> &'static [eframe::Renderer] {
    match choice {
        Renderer::Auto => &[eframe::Renderer::Wgpu, eframe::Renderer::Glow],
        Renderer::Wgpu => &[eframe::Renderer::Wgpu],
        Renderer::Glow => &[eframe::Renderer::Glow],
    }
}

fn renderer_name(r: eframe::Renderer) -> &'static str {
    match r {
        eframe::Renderer::Wgpu => "wgpu",
        eframe::Renderer::Glow => "glow (OpenGL)",
    }
}

fn run_gui(init: &AppInit, choice: Renderer) -> anyhow::Result<()> {
    let mut last_error = None;
    for &renderer in renderer_chain(choice) {
        tracing::info!("trying the {} renderer", renderer_name(renderer));
        let created = Arc::new(AtomicBool::new(false));
        let flag = created.clone();
        let init = init.clone();
        let options = oxtail_gui::native_options(renderer, init.startup.session.window);
        let result = eframe::run_native(
            "OxTail",
            options,
            Box::new(move |cc| {
                flag.store(true, Ordering::Release);
                tracing::info!("using the {} renderer", renderer_name(renderer));
                Ok(Box::new(OxTailApp::new(cc, init)))
            }),
        );
        match result {
            Ok(()) => return Ok(()),
            Err(e) if created.load(Ordering::Acquire) => {
                // The window worked; do not open a second one.
                return Err(anyhow::anyhow!("the GUI stopped with an error: {e}"));
            }
            Err(e) => {
                tracing::warn!("the {} renderer failed: {e}", renderer_name(renderer));
                last_error = Some(e.to_string());
            }
        }
    }
    Err(anyhow::anyhow!(
        "could not create a window: {}",
        last_error.unwrap_or_else(|| "no renderer available".into())
    ))
}
