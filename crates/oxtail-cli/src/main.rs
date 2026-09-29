//! The `oxtail` binary: parses the command line, resolves the data folder,
//! makes sure only one instance runs per data folder, and starts the GUI with
//! a renderer fallback chain (wgpu, then OpenGL).

// Release builds on Windows are GUI-subsystem apps, so launching OxTail does
// not also open a console window. (Console output such as `--help` is then not
// shown when started from a terminal; attaching to the parent console is M6.)
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod args;
mod ipc;
mod logging;
mod profile_name;
mod renderer;

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use oxtail_config::{DataDir, Renderer, ResolveInput, update::LatestCheck};
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
    if cli.integrate || cli.remove_integration || cli.check_update {
        return maintenance(&cli);
    }
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("oxtail: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// `--integrate`, `--remove-integration` and `--check-update`: print and exit,
/// without a window, an instance lock or a log file.
fn maintenance(cli: &args::Cli) -> ExitCode {
    let data_dir = DataDir::resolve(ResolveInput::from_environment(cli.data_dir.clone()));
    let mut ok = true;
    if cli.remove_integration {
        match oxtail_config::integration::remove(&data_dir) {
            Ok(done) if done.is_empty() => {
                println!("OxTail was not integrated; nothing to remove.")
            }
            Ok(done) => done.iter().for_each(|l| println!("{l}")),
            Err(e) => {
                eprintln!("oxtail: {e}");
                ok = false;
            }
        }
    }
    if cli.integrate {
        let result = std::env::current_exe()
            .map_err(|e| {
                oxtail_config::ConfigError::Integration(format!("no executable path: {e}"))
            })
            .and_then(|exe| oxtail_config::integration::integrate(&data_dir, &exe));
        match result {
            Ok(done) => done.iter().for_each(|l| println!("{l}")),
            Err(e) => {
                eprintln!("oxtail: {e}");
                ok = false;
            }
        }
    }
    if cli.check_update {
        let current = env!("CARGO_PKG_VERSION");
        match oxtail_config::update::check_latest_detailed(current) {
            Ok(result) => {
                match result {
                    LatestCheck::Newer(r) => {
                        println!(
                            "OxTail {} is available (you have {current}): {}",
                            r.version, r.url
                        );
                        if let Some(notes) = r.notes {
                            println!("\n{notes}");
                        }
                    }
                    LatestCheck::UpToDate => println!("OxTail {current} is up to date."),
                    LatestCheck::NoRelease => println!("No release has been published yet."),
                }
                // Only a successful check resets the throttle of the automatic one.
                let _ = oxtail_config::update::record_check(&data_dir);
            }
            Err(e) => {
                eprintln!("oxtail: {e}");
                ok = false;
            }
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run(cli: args::Cli) -> anyhow::Result<ExitCode> {
    let data_dir = DataDir::resolve(ResolveInput::from_environment(cli.data_dir.clone()));
    logging::init(data_dir.root.as_deref());
    if let Some(reason) = data_dir.in_memory_reason() {
        tracing::warn!("running without a data folder: {reason}");
    }
    let mut request = cli.to_request().absolutized();
    if let Some(input) = request.profile.clone() {
        let cands = profile_name::candidates(data_dir.profiles_dir().as_deref());
        match profile_name::resolve(&input, &cands) {
            Ok(name) => request.profile = Some(name),
            Err(e) => {
                eprintln!("oxtail: {e}");
                return Ok(ExitCode::from(2));
            }
        }
    }

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
    run_gui(init, renderer)
}

fn renderer_name(r: eframe::Renderer) -> &'static str {
    match r {
        eframe::Renderer::Wgpu => "wgpu",
        eframe::Renderer::Glow => "glow (OpenGL)",
    }
}

/// Runs the GUI. `winit` allows one event loop per process, so the fallback
/// from wgpu to OpenGL is a probe before the window exists plus a restart of
/// the process if wgpu still fails (see `renderer`).
fn run_gui(init: AppInit, choice: Renderer) -> anyhow::Result<ExitCode> {
    let is_retry = std::env::var_os(renderer::RETRY_ENV).is_some();
    let plan = renderer::plan(choice, renderer::wgpu_adapter_available, is_retry);
    tracing::info!(
        "starting with the {} renderer{}",
        renderer_name(plan.first),
        if is_retry {
            " (retry after a wgpu failure)"
        } else {
            ""
        }
    );
    let created = Arc::new(AtomicBool::new(false));
    let flag = created.clone();
    let used = plan.first;
    let options = oxtail_gui::native_options(plan.first, init.startup.session.window);
    let result = eframe::run_native(
        "OxTail",
        options,
        Box::new(move |cc| {
            flag.store(true, Ordering::Release);
            tracing::info!("using the {} renderer", renderer_name(used));
            Ok(Box::new(OxTailApp::new(cc, init)))
        }),
    );
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(e) if created.load(Ordering::Acquire) => {
            // The window worked; do not open a second one.
            Err(anyhow::anyhow!("the GUI stopped with an error: {e}"))
        }
        Err(e) if plan.fall_back => {
            tracing::warn!(
                "the {} renderer failed ({e}); restarting with OpenGL",
                renderer_name(plan.first)
            );
            renderer::retry_with_glow().map_err(|io| {
                anyhow::anyhow!(
                    "could not create a window ({e}) and could not restart with OpenGL: {io}"
                )
            })
        }
        Err(e) => Err(anyhow::anyhow!("could not create a window: {e}")),
    }
}
