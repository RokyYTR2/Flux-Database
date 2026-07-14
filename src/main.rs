use clap::Parser;
use fluxdb::engine::{Engine, QueryResult};
use fluxdb::security::{AuditLogger, AuthManager, CryptoManager, Role};
use fluxdb::types::Value;
use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "fluxdb",
    version,
    about = "A tiny PostgreSQL-inspired SQL database in Rust"
)]
struct Cli {
    #[arg(long, default_value = "./data")]
    data_dir: PathBuf,
    #[arg(
        long,
        default_value = "FLUXDB_MASTER_KEY",
        help = "Environment variable holding base64 32-byte encryption key"
    )]
    master_key_env: String,
    #[arg(long, help = "Print a new secure base64 master key and exit")]
    keygen: bool,
    #[arg(long, help = "Username for login")]
    user: Option<String>,
    #[arg(
        long,
        default_value = "FLUXDB_PASSWORD",
        help = "Environment variable with login password"
    )]
    password_env: String,
    #[arg(long, help = "Read login password from stdin")]
    password_stdin: bool,
    #[arg(
        long,
        help = "Create the first admin user when no users exist in auth store"
    )]
    bootstrap_admin: Option<String>,
    #[arg(long, help = "Admin-only: create a new user and exit")]
    add_user: Option<String>,
    #[arg(
        long,
        default_value = "read_only",
        help = "Role for --add-user: admin | read_write | read_only"
    )]
    add_role: String,
    #[arg(
        long,
        default_value = "FLUXDB_NEW_USER_PASSWORD",
        help = "Environment variable with password for --add-user"
    )]
    new_user_password_env: String,
    #[arg(long, help = "Read new user password from stdin for --add-user")]
    new_user_password_stdin: bool,
    #[arg(short = 'e', long, help = "Execute SQL script passed as a string")]
    execute: Option<String>,
    #[arg(short = 'f', long, help = "Execute SQL script from file")]
    file: Option<PathBuf>,
}

fn main() {
    let cli = Cli::parse();

    if cli.keygen {
        println!("{}", CryptoManager::generate_base64_key());
        return;
    }

    if cli.execute.is_some() && cli.file.is_some() {
        eprintln!("Use either --execute or --file, not both.");
        std::process::exit(2);
    }

    let crypto = match CryptoManager::from_env(&cli.master_key_env) {
        Ok(crypto) => crypto,
        Err(err) => {
            eprintln!("Security initialization error: {err}");
            eprintln!("Tip: run `fluxdb --keygen` and set {}.", cli.master_key_env);
            std::process::exit(1);
        }
    };

    let mut auth_manager = match AuthManager::open(&cli.data_dir, crypto.clone()) {
        Ok(manager) => manager,
        Err(err) => {
            eprintln!("Failed to open auth store: {err}");
            std::process::exit(1);
        }
    };

    if !auth_manager.has_users() {
        let Some(admin_username) = cli.bootstrap_admin.as_ref() else {
            eprintln!("No users exist. Provide --bootstrap-admin <username> to initialize auth.");
            std::process::exit(1);
        };
        let bootstrap_password = match read_password(
            &cli.password_env,
            cli.password_stdin,
            "Bootstrap admin password: ",
        ) {
            Ok(password) => password,
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(1);
            }
        };

        if let Err(err) = auth_manager.create_user(admin_username, &bootstrap_password, Role::Admin)
        {
            eprintln!("Failed to bootstrap admin user: {err}");
            std::process::exit(1);
        }
        println!("Admin user '{}' created.", admin_username);
    }

    let username = match cli.user.as_ref() {
        Some(user) => user.to_string(),
        None => {
            eprintln!("Provide --user <username>.");
            std::process::exit(1);
        }
    };

    let login_password = match read_password(&cli.password_env, cli.password_stdin, "Password: ") {
        Ok(password) => password,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    };
    let identity = match auth_manager.authenticate(&username, &login_password) {
        Ok(identity) => identity,
        Err(err) => {
            eprintln!("Authentication failed: {err}");
            std::process::exit(1);
        }
    };

    if let Some(new_user) = cli.add_user.as_ref() {
        if identity.role != Role::Admin {
            eprintln!("Only admin user can execute --add-user.");
            std::process::exit(1);
        }
        let role = match Role::parse(&cli.add_role) {
            Ok(role) => role,
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(1);
            }
        };
        let new_password = match read_password(
            &cli.new_user_password_env,
            cli.new_user_password_stdin,
            "New user password: ",
        ) {
            Ok(password) => password,
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(1);
            }
        };
        if let Err(err) = auth_manager.create_user(new_user, &new_password, role.clone()) {
            eprintln!("Failed to create user '{new_user}': {err}");
            std::process::exit(1);
        }
        println!("User '{new_user}' created with role '{role}'.");
        return;
    }

    let audit_logger = match AuditLogger::open(&cli.data_dir) {
        Ok(logger) => logger,
        Err(err) => {
            eprintln!("Failed to initialize audit logger: {err}");
            std::process::exit(1);
        }
    };
    let mut engine = match Engine::open(&cli.data_dir, crypto, identity, audit_logger) {
        Ok(engine) => engine,
        Err(err) => {
            eprintln!("Failed to initialize database: {err}");
            std::process::exit(1);
        }
    };

    if let Some(script) = cli.execute {
        if let Err(err) = run_script(&mut engine, &script) {
            eprintln!("{err}");
            std::process::exit(1);
        }
        return;
    }

    if let Some(path) = cli.file {
        let script = match fs::read_to_string(&path) {
            Ok(script) => script,
            Err(err) => {
                eprintln!("Failed to read SQL file '{}': {err}", path.display());
                std::process::exit(1);
            }
        };
        if let Err(err) = run_script(&mut engine, &script) {
            eprintln!("{err}");
            std::process::exit(1);
        }
        return;
    }

    run_repl(&mut engine);
}

fn run_script(engine: &mut Engine, script: &str) -> Result<(), String> {
    let results = engine
        .execute_script(script)
        .map_err(|err| err.to_string())?;
    for result in &results {
        print_result(result);
    }
    Ok(())
}

fn run_repl(engine: &mut Engine) {
    println!("FluxDB shell");
    println!("Type SQL statements ending with ';'. Use 'exit' or '\\q' to quit.");

    let mut buffer = String::new();
    loop {
        let prompt = if buffer.is_empty() {
            "fluxdb> "
        } else {
            "   ...> "
        };
        print!("{prompt}");
        if io::stdout().flush().is_err() {
            eprintln!("Failed to flush stdout");
            break;
        }

        let mut line = String::new();
        let bytes = match io::stdin().read_line(&mut line) {
            Ok(bytes) => bytes,
            Err(err) => {
                eprintln!("Input error: {err}");
                break;
            }
        };
        if bytes == 0 {
            break;
        }

        let trimmed = line.trim();
        if buffer.is_empty() && matches_quit_command(trimmed) {
            break;
        }
        if trimmed.is_empty() && buffer.is_empty() {
            continue;
        }

        buffer.push_str(&line);
        if !trimmed.ends_with(';') {
            continue;
        }

        match run_script(engine, &buffer) {
            Ok(()) => {}
            Err(err) => eprintln!("Error: {err}"),
        }
        buffer.clear();
    }
}

fn matches_quit_command(input: &str) -> bool {
    input.eq_ignore_ascii_case("exit")
        || input.eq_ignore_ascii_case("quit")
        || input.eq_ignore_ascii_case("\\q")
}

fn read_password(env_name: &str, read_from_stdin: bool, prompt: &str) -> Result<String, String> {
    if read_from_stdin {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .map_err(|err| format!("Failed to read password from stdin: {err}"))?;
        let password = input.trim_end_matches(['\r', '\n']).to_string();
        if password.is_empty() {
            return Err("Password from stdin is empty".to_string());
        }
        return Ok(password);
    }

    if let Ok(value) = std::env::var(env_name) {
        if !value.trim().is_empty() {
            return Ok(value);
        }
    }
    rpassword::prompt_password(prompt).map_err(|err| format!("Password read failed: {err}"))
}

fn print_result(result: &QueryResult) {
    match result {
        QueryResult::Message(message) => println!("{message}"),
        QueryResult::Rows { columns, rows } => print_rows(columns, rows),
    }
}

fn print_rows(columns: &[String], rows: &[Vec<Value>]) {
    let printable_rows = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let mut widths = columns
        .iter()
        .map(|column| column.len())
        .collect::<Vec<_>>();
    for row in &printable_rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }

    print_separator(&widths);
    print_cells(columns, &widths);
    print_separator(&widths);
    for row in &printable_rows {
        print_cells(row, &widths);
    }
    print_separator(&widths);
    println!("{} row(s)", rows.len());
}

fn print_separator(widths: &[usize]) {
    let segment = widths
        .iter()
        .map(|width| "-".repeat(width + 2))
        .collect::<Vec<_>>()
        .join("+");
    println!("+{segment}+");
}

fn print_cells(cells: &[String], widths: &[usize]) {
    print!("|");
    for (cell, width) in cells.iter().zip(widths.iter()) {
        print!(" {:width$} |", cell, width = *width);
    }
    println!();
}
