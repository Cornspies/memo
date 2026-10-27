//! `memo-server` command line. See `docs/server.md#admin-cli`.

use std::{path::PathBuf, process::ExitCode};

use memo_server::{AppState, Config, admin, config::Size, jobs, web};

const USAGE: &str = "usage: memo-server [--config <path>] <command>

commands:
  serve
  invite create [--role user|admin] [--expires <days>d]
  invite list
  invite revoke <id>
  user list
  user disable <username>
  user enable <username>
  user set-quota <username> <size|default>
  user delete <username>
  backup <dir>
  restore <dir>
  gc";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = match take_option(&mut args, "--config") {
        Ok(path) => path.map(PathBuf::from),
        Err(e) => return fail(&e),
    };
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return if args.is_empty() { ExitCode::FAILURE } else { ExitCode::SUCCESS };
    }
    let config = match Config::load(config_path.as_deref()) {
        Ok(config) => config,
        Err(e) => return fail(&e),
    };
    match run(config, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("memo-server: {message}");
    ExitCode::FAILURE
}

/// Removes `--name value` from `args`.
fn take_option(args: &mut Vec<String>, name: &str) -> Result<Option<String>, String> {
    let Some(index) = args.iter().position(|a| a == name) else { return Ok(None) };
    if index + 1 >= args.len() {
        return Err(format!("{name} needs a value"));
    }
    let value = args.remove(index + 1);
    args.remove(index);
    Ok(Some(value))
}

fn run(config: Config, mut args: Vec<String>) -> Result<(), String> {
    let role = take_option(&mut args, "--role")?.unwrap_or_else(|| "user".into());
    let expires = take_option(&mut args, "--expires")?;
    let words: Vec<&str> = args.iter().map(String::as_str).collect();

    if words == ["restore", words.get(1).copied().unwrap_or_default()] && words.len() == 2 {
        admin::restore(&PathBuf::from(words[1]), &config.data_dir).map_err(|e| e.message)?;
        println!("restored; clients will resync when they next connect");
        return Ok(());
    }

    let default_quota = config.limits.default_quota.0;
    let data_dir = config.data_dir.clone();
    let app = AppState::open(config)?;
    let db = &app.db;
    let message = |e: memo_server::error::ApiError| e.message;
    match words.as_slice() {
        ["serve"] => {
            tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).with_target(false).init();
            let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            return runtime.block_on(memo_server::serve(app));
        }
        ["invite", "create"] => {
            let days = match expires.as_deref() {
                None => 7,
                Some(text) => text
                    .trim_end_matches('d')
                    .parse::<i64>()
                    .map_err(|_| "--expires takes a number of days, e.g. 7d")?,
            };
            let code = admin::invite_create(db, &role, days * 24 * 3600 * 1000).map_err(message)?;
            println!("{code}");
            eprintln!("({role} invite, valid for {days} days; shown only once)");
            if web::is_built() {
                eprintln!("link: {}/#/invite/{code}", app.config.public_url.trim_end_matches('/'));
            }
        }
        ["invite", "list"] => {
            for invite in admin::invite_list(db).map_err(message)? {
                let status = match invite.used_by {
                    Some(user) => format!("used by {user}"),
                    None => format!("expires {}", admin::format_time(invite.expires_at)),
                };
                println!(
                    "{}  {:5}  created {}  {status}",
                    invite.id,
                    invite.role,
                    admin::format_time(invite.created_at)
                );
            }
        }
        ["invite", "revoke", id] => admin::invite_revoke(db, id).map_err(message)?,
        ["user", "list"] => {
            for user in admin::user_list(db, default_quota).map_err(message)? {
                println!(
                    "{:32}  {:5}  {:>10} / {:<10}  since {}{}",
                    user.username,
                    user.role,
                    human_size(user.used_bytes),
                    human_size(user.quota_bytes),
                    admin::format_time(user.created_at),
                    if user.disabled { "  DISABLED" } else { "" }
                );
            }
        }
        ["user", "disable", name] => admin::user_set_disabled(db, name, true).map_err(message)?,
        ["user", "enable", name] => admin::user_set_disabled(db, name, false).map_err(message)?,
        ["user", "set-quota", name, size] => {
            let quota = if *size == "default" { None } else { Some(Size::parse(size)?.0) };
            admin::user_set_quota(db, name, quota).map_err(message)?;
        }
        ["user", "delete", name] => admin::user_delete(db, name).map_err(message)?,
        ["backup", dir] => {
            admin::backup(db, &data_dir, &PathBuf::from(dir)).map_err(message)?;
            println!("backup written to {dir}");
        }
        ["gc"] => {
            let report = jobs::gc(db, &app.config).map_err(message)?;
            println!("{report:?}");
        }
        _ => return Err(format!("unknown command\n\n{USAGE}")),
    }
    Ok(())
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}
