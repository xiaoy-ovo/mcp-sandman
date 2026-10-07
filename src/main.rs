//! mcp-sandman 鈥?a policy-enforcing sandbox proxy for MCP servers.

mod audit;
mod config;
mod error;
mod policy;
mod protocol;
mod proxy;
mod upstream;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config::Config;
use crate::error::Result;
use crate::proxy::Proxy;

#[derive(Parser, Debug)]
#[command(
    name = "mcp-sandman",
    version,
    about = "A policy-enforcing sandbox proxy for Model Context Protocol servers",
    long_about = None,
)]
struct Cli {
    /// Path to a TOML policy file. Defaults to ./sandman.toml when present.
    #[arg(short, long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Override `name`.
    #[arg(long, global = true)]
    name: Option<String>,

    /// Override `log_level` (trace|debug|info|warn|error).
    #[arg(long, global = true, value_name = "LEVEL")]
    log_level: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug, Clone)]
enum Command {
    /// Run the proxy on stdio (default).
    Run,
    /// Validate the policy without connecting to anything.
    Check,
    /// Connect to the upstream and report the tools that survive the policy.
    Doctor,
    /// Print a starter policy for a given upstream command.
    Init {
        /// The command that starts the MCP server to be sandboxed.
        command: String,
    },
}

fn main() {
    let cli = Cli::parse();

    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        let level = cli.log_level.clone().unwrap_or_else(|| "info".to_string());
        tracing_subscriber::EnvFilter::new(level)
    });

    // Logs go to stderr, never stdout: stdout carries the JSON-RPC stream and
    // a stray log line there corrupts the protocol.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    if let Err(e) = run(cli) {
        eprintln!("mcp-sandman: {e}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    // `command` is moved out below, so read the flags we still need first.
    let command = match cli.command.clone() {
        Some(c) => c,
        None => Command::Run,
    };

    match command {
        Command::Run => {
            let config = load_config(&cli)?;
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(async move {
                    let proxy = Proxy::new(config).await?;
                    proxy.run().await
                })
        }
        Command::Check => check(&cli),
        Command::Doctor => doctor(&cli),
        Command::Init { command } => print_starter_config(&command),
    }
}

/// Load the policy: explicit `--config`, else `./sandman.toml` if it exists.
fn load_config(cli: &Cli) -> Result<Config> {
    let mut config = match &cli.config {
        Some(path) => Config::from_file(path)?,
        None => {
            let default_path = PathBuf::from("sandman.toml");
            if default_path.exists() {
                Config::from_file(&default_path)?
            } else {
                Config::default()
            }
        }
    };

    if let Some(name) = &cli.name {
        config.name = name.clone();
    }
    if let Some(level) = &cli.log_level {
        config.log_level = level.clone();
    }

    config.validate()?;
    Ok(config)
}

/// Validate the policy and report what it would expose, without connecting.
fn check(cli: &Cli) -> Result<()> {
    let config = load_config(cli)?;

    println!("name:       {}", config.name);
    println!("upstream:   {}", describe_upstream(&config));
    println!("isolation:  {}", describe_isolation(&config));
    println!(
        "filesystem: read={} write={}",
        describe_globs(&config.filesystem.read),
        describe_globs(&config.filesystem.write)
    );
    println!("network:    {}", describe_network(&config));
    println!("tools:      {}", describe_tools(&config));

    // Compiling the policy is the real validation: it surfaces every bad glob
    // and regex, which parsing the file alone would not.
    crate::policy::Policy::compile(&config)?;
    println!("\npolicy is valid");
    Ok(())
}

/// Connect to the upstream and list the tools that survive the policy. This is
/// the command that catches a typo'd tool name.
fn doctor(cli: &Cli) -> Result<()> {
    let config = load_config(cli)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let tools = runtime.block_on(proxy::probe(&config))?;
    if tools.is_empty() {
        println!("no tools survive the policy");
        if config.tools.require_non_empty {
            eprintln!(
                "warning: `tools.require_non_empty` is set, so this server will refuse to start"
            );
        }
        return Ok(());
    }

    println!("{} tool(s) exposed:", tools.len());
    for tool in tools {
        println!("  {tool}");
    }
    Ok(())
}

fn describe_isolation(config: &Config) -> String {
    match &config.isolation {
        crate::config::Isolation::None => "none (policy checks only)".to_string(),
        crate::config::Isolation::Container { image, args } => {
            let extra = if args.is_empty() {
                String::new()
            } else {
                format!(" ({})", args.join(" "))
            };
            format!("container {image}{extra}")
        }
    }
}

fn describe_network(config: &Config) -> String {
    let net = &config.network;
    if net.is_offline() {
        return "offline".to_string();
    }
    let mut parts = vec![format!("allow {}", describe_globs(&net.allow_hosts))];
    if !net.deny_hosts.is_empty() {
        parts.push(format!("deny {}", describe_globs(&net.deny_hosts)));
    }
    if !net.allow_ports.is_empty() {
        let ports: Vec<String> = net.allow_ports.iter().map(u16::to_string).collect();
        parts.push(format!("ports {}", ports.join(",")));
    }
    if net.block_dns {
        parts.push("dns blocked".to_string());
    }
    parts.join(", ")
}

fn describe_tools(config: &Config) -> String {
    let tools = &config.tools;
    let mut parts = Vec::new();
    if tools.allow.is_empty() {
        parts.push("allow all".to_string());
    } else {
        parts.push(format!("allow {}", describe_globs(&tools.allow)));
    }
    if !tools.deny.is_empty() {
        parts.push(format!("deny {}", describe_globs(&tools.deny)));
    }
    if let Some(ns) = &tools.namespace {
        parts.push(format!("namespace {ns}"));
    }
    parts.join(", ")
}

fn describe_upstream(config: &Config) -> String {
    match &config.upstream {
        config::UpstreamSpec::Stdio { command, args, .. } => {
            if args.is_empty() {
                command.clone()
            } else {
                format!("{command} {}", args.join(" "))
            }
        }
        config::UpstreamSpec::Http { url, .. } => url.clone(),
    }
}

fn describe_globs(patterns: &[String]) -> String {
    if patterns.is_empty() {
        return "none".to_string();
    }
    patterns.join(", ")
}

/// `sandman init <cmd>` 鈥?emit a starter policy for a given server command.
///
/// The command is split on whitespace into a program and arguments, so
/// `init "npx -y @acme/db"` produces a runnable `command`/`args` pair instead
/// of putting the whole string in `command`, where it would fail to spawn.
fn print_starter_config(command: &str) -> Result<()> {
    let (program, args) = split_command(command)?;
    print!("{}", render_template(&program, &args));
    Ok(())
}

/// Fill the starter template's placeholders.
fn render_template(program: &str, args: &[String]) -> String {
    let args_toml = if args.is_empty() {
        "[]".to_string()
    } else {
        format!(
            "[{}]",
            args.iter()
                .map(|a| format!("{a:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    include_str!("starter.toml")
        .replace("__UPSTREAM_COMMAND__", &program.replace('\\', "\\\\"))
        .replace("__UPSTREAM_ARGS__", &args_toml)
}

/// Split a shell-ish command string into a program and its arguments.
fn split_command(command: &str) -> Result<(String, Vec<String>)> {
    let mut parts = command.split_whitespace().peekable();
    let program = parts.next().unwrap_or_default().to_string();
    if program.is_empty() {
        return Err(crate::error::config_err("no command given"));
    }
    Ok((program, parts.map(str::to_string).collect()))
}

/// The starter template must stay loadable.
///
/// Regression guard: a top-level key placed after a `[table]` header silently
/// becomes part of that table, and the first version of this template shipped
/// with `log_level` under `[limits]`. Every key that belongs to the document
/// root has to sit above the first table header.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_template_parses_and_validates() {
        for command in ["echo hi", "npx", "python -u server.py --port 8080"] {
            let (program, args) = split_command(command).unwrap();
            let rendered = render_template(&program, &args);
            let config: Config =
                toml::from_str(&rendered).unwrap_or_else(|e| panic!("{command}: {e}"));
            config
                .validate()
                .unwrap_or_else(|e| panic!("{command}: {e}"));
        }
    }

    #[test]
    fn rendered_command_and_args_land_in_the_upstream_table() {
        let (program, args) = split_command("python -u server.py --port 8080").unwrap();
        let rendered = render_template(&program, &args);
        let config: Config = toml::from_str(&rendered).unwrap();
        match &config.upstream {
            crate::config::UpstreamSpec::Stdio {
                command, args: got, ..
            } => {
                assert_eq!(command, "python");
                assert_eq!(got, &["-u", "server.py", "--port", "8080"]);
            }
            other => panic!("expected a stdio upstream, got {other:?}"),
        }
    }

    #[test]
    fn command_splits_into_program_and_args() {
        assert_eq!(
            split_command("npx -y @acme/db").unwrap(),
            (
                "npx".to_string(),
                vec!["-y".to_string(), "@acme/db".to_string()]
            )
        );
        assert_eq!(
            split_command("server").unwrap(),
            ("server".to_string(), vec![])
        );
        assert!(
            split_command("   ").is_err(),
            "an empty command is not runnable"
        );
    }

    #[test]
    fn starter_template_keeps_root_keys_above_the_first_table() {
        let rendered = include_str!("starter.toml");
        let first_table = rendered
            .lines()
            .position(|l| l.trim_start().starts_with('['))
            .expect("template has tables");
        for key in ["name =", "log_level ="] {
            let line = rendered
                .lines()
                .position(|l| l.starts_with(key))
                .unwrap_or_else(|| panic!("`{key}` is missing from the template"));
            assert!(
                line < first_table,
                "`{key}` sits after the first table header, so TOML would file it \
                 under that table instead of the document root"
            );
        }
    }
}
