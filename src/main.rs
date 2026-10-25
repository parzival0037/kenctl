use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use kenctl::{client, devices, engine, hardware, profiles, proto, server, state};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "kenctl",
    version,
    about = "Linux configuration daemon and web UI for Kensington trackballs"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the engine and serve the web UI on localhost.
    Run(RunArgs),
    /// Show what a running daemon is doing.
    Status(StatusArgs),
    /// List detected Kensington input nodes.
    Devices,
    /// List stored profiles from disk.
    Profiles,
    /// Switch the running daemon to a different profile.
    Activate {
        name: String,
        #[command(flatten)]
        net: StatusArgs,
    },
    /// Report on the vendor HID (on-device settings) channel.
    Hardware,
    /// Print live button/wheel events (no grab) to discover mapping names.
    Listen {
        #[arg(long)]
        device: Option<PathBuf>,
        /// Also show pointer motion (REL_X / REL_Y).
        #[arg(long)]
        all: bool,
    },
    /// Minimal single-feature prototypes, validated one at a time.
    Proto {
        #[command(subcommand)]
        feature: ProtoFeature,
    },
}

#[derive(Subcommand)]
enum ProtoFeature {
    /// Remap one trackball button to a key, with optional held modifiers.
    ///
    /// --dry-run reads events without grabbing, so it needs no root/uinput.
    Button {
        #[arg(long, default_value = "BTN_SIDE")]
        from: String,
        #[arg(long, default_value = "KEY_C")]
        to: String,
        /// Modifier held while `to` is pressed, e.g. KEY_LEFTCTRL.
        #[arg(long = "modifier")]
        modifiers: Vec<String>,
        #[arg(long)]
        device: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Scale (and optionally invert) scroll wheel deltas.
    ///
    /// --dry-run reads events without grabbing, so it needs no root/uinput.
    Scroll {
        #[arg(long, default_value_t = 1.5)]
        speed: f64,
        #[arg(long)]
        invert: bool,
        #[arg(long)]
        device: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Args)]
struct RunArgs {
    /// Interface to bind the web UI to.
    #[arg(long, default_value = "127.0.0.1")]
    bind: String,

    #[arg(long, default_value_t = 3789)]
    port: u16,

    /// Profile to activate on startup.
    #[arg(long)]
    profile: Option<String>,

    /// Serve the UI without grabbing input (does not need /dev/uinput).
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct StatusArgs {
    #[arg(long, default_value_t = 3789)]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Run(args) => run(args).await,

        Command::Status(args) => {
            let raw = client::get_json(args.port, "/api/state")
                .context("is `kenctl run` active? try: kenctl run")?;
            print_status(&serde_json::from_str(&raw)?);
            Ok(())
        }

        Command::Devices => {
            let found = devices::scan();
            if found.is_empty() {
                println!("No supported Kensington input nodes found.");
            }
            for d in found {
                println!(
                    "{:<10} {:<44} 0x{:04X}  {}",
                    if d.managed { "grabbed" } else { "free" },
                    d.path.display(),
                    d.product_id,
                    d.model
                );
            }
            Ok(())
        }

        Command::Profiles => {
            let path = profiles::store_path();
            if !path.exists() {
                println!("No profiles stored yet at {}", path.display());
                return Ok(());
            }
            let store = state::load_store(&path);
            println!("{}", path.display());
            for p in store.profiles {
                let marker = if p.name == store.active { "*" } else { " " };
                let mapped = p.buttons.len();
                println!(
                    "{marker} {:<24} pointer {:>4.2}  scroll {:>4.2}  {} button{} mapped",
                    p.name,
                    p.pointer_speed,
                    p.scroll_speed,
                    mapped,
                    if mapped == 1 { "" } else { "s" }
                );
            }
            Ok(())
        }

        Command::Activate { name, net } => {
            client::send_json(net.port, "POST", &format!("/api/profiles/{name}/activate"), "")?;
            println!("activated: {name}");
            Ok(())
        }

        Command::Listen { device, all } => proto::listen(device.as_deref(), all),

        Command::Proto { feature } => match feature {
            ProtoFeature::Button {
                from,
                to,
                modifiers,
                device,
                dry_run,
            } => proto::button(&from, &to, &modifiers, device.as_deref(), dry_run),

            ProtoFeature::Scroll {
                speed,
                invert,
                device,
                dry_run,
            } => proto::scroll(speed, invert, device.as_deref(), dry_run),
        },

        Command::Hardware => {
            let status = hardware::HardwareStatus::probe();
            println!("config node : {}", status.config_node.as_deref().unwrap_or("-"));
            println!("hidraw read : {}", status.hidraw_readable);
            println!("usbfs node  : {}", status.usbfs_node.as_deref().unwrap_or("-"));
            println!("usbfs write : {}", status.usbfs_writable);
            println!("opcode table: {}", if status.protocol_recovered { "known" } else { "not recovered" });
            println!();
            println!("{}", status.message);
            Ok(())
        }
    }
}

async fn run(args: RunArgs) -> Result<()> {
    let path = profiles::store_path();
    let mut store = state::load_store(&path);
    if let Some(name) = &args.profile {
        store
            .activate(name)
            .with_context(|| format!("unknown startup profile {name}"))?;
    }

    let app = state::AppState::new(store, path);
    let devices_slot = app.devices.clone();

    if args.dry_run {
        tracing::warn!("--dry-run: UI only, input nodes are not grabbed");
        engine::spawn_scanner(devices_slot);
    } else {
        let vdev = std::sync::Arc::new(std::sync::Mutex::new(engine::create_virtual_device()?));
        engine::set_current_vdev(vdev.clone());
        engine::spawn_supervisor(app.active.clone(), vdev, devices_slot);
    }

    let listener = tokio::net::TcpListener::bind((args.bind.as_str(), args.port))
        .await
        .with_context(|| format!("binding {}:{}", args.bind, args.port))?;

    let ui = format!("http://{}:{}/", args.bind, args.port);
    println!("KenCtl for Linux");
    println!("  web UI : {ui}");
    println!("  store  : {}", app.store_path.display());
    println!("  mode   : {}", if args.dry_run { "dry-run (UI only)" } else { "engine active" });
    println!();

    axum::serve(listener, server::router(app))
        .await
        .context("web server")?;
    Ok(())
}

fn print_status(v: &Value) {
    println!("store          : {}", v["store_path"]);
    println!("active profile : {}", v["active_name"]);
    let running = v["engine_running"].as_bool().unwrap_or(false);
    println!(
        "engine         : {}",
        if running {
            "grabbing input"
        } else {
            "not grabbing (dry-run, or no device matched)"
        }
    );

    let found = v["devices"].as_array().cloned().unwrap_or_default();
    if found.is_empty() {
        println!("devices        : none found");
    } else {
        println!("devices        :");
        for d in &found {
            println!(
                "  {} {} 0x{:04x} {}",
                if d["managed"].as_bool().unwrap_or(false) {
                    "[grabbed]"
                } else {
                    "[free]   "
                },
                d["path"].as_str().unwrap_or("?"),
                d["product_id"].as_u64().unwrap_or(0),
                d["model"].as_str().unwrap_or("?"),
            );
        }
    }

    if let Some(msg) = v["hardware"]["message"].as_str() {
        println!("hardware       : {msg}");
    }
}
