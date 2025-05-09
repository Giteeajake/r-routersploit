use anyhow::Result;
use std::{
    fs::File,
    io::{self, Write},
    net::{SocketAddr, ToSocketAddrs},
    sync::Arc,
};
use tokio::{
    net::{TcpStream, UdpSocket},
    sync::Semaphore,
    time::{timeout, Duration},
};

#[allow(dead_code)]
pub struct ScanSettings {
    pub concurrency: usize,
    pub timeout_secs: u64,
    pub show_only_open: bool,
    pub verbose: bool,
    pub scan_udp_enabled: bool,
    pub output_file: String,
}

#[allow(dead_code)]
/// Prompt user for scan configuration
pub fn prompt_settings() -> Result<ScanSettings> {
    Ok(ScanSettings {
        concurrency: prompt_usize("Concurrency: ")?,
        timeout_secs: prompt_usize("Timeout (in seconds): ")? as u64,
        show_only_open: prompt_bool("Show only open ports? (y/n): ")?,
        verbose: prompt_bool("Verbose output? (y/n): ")?,
        scan_udp_enabled: prompt_bool("Include UDP scan? (y/n): ")?,
        output_file: prompt("Output filename: ")?,
    })
}

#[allow(dead_code)]
/// Interactive entry point
pub async fn run_interactive(target: &str) -> Result<()> {
    let settings = prompt_settings()?;
    run_with_settings(
        target,
        settings.concurrency,
        settings.timeout_secs,
        settings.show_only_open,
        settings.verbose,
        settings.scan_udp_enabled,
        &settings.output_file,
    )
    .await
}

/// Dispatch-compatible wrapper
#[allow(dead_code)]
pub async fn run(target: &str) -> Result<()> {
    run_interactive(target).await
}

/// Renamed internal function to avoid clash
pub async fn run_with_settings(
    target: &str,
    concurrency: usize,
    timeout_secs: u64,
    show_only_open: bool,
    verbose: bool,
    scan_udp_enabled: bool,
    output_file: &str,
) -> Result<()> {
    let target = normalize_target(target)?;
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut tasks = vec![];
    let mut file = File::create(output_file)?;
    writeln!(file, "Scan Results for {}\n", target)?;

    println!("[*] Starting TCP scan...");
    for port in 1..=65535 {
        let permit = semaphore.clone().acquire_owned().await?;
        let target = target.clone();
        let mut file = file.try_clone()?;

        let handle = tokio::spawn(async move {
            let _permit = permit;
            if let Some((status, banner)) = scan_tcp(&target, port, timeout_secs).await {
                let line = format!("[TCP] {}:{} => {}", target, port, status);
                if status == "OPEN" || !show_only_open {
                    if !banner.is_empty() {
                        writeln!(file, "{} | Banner: {}", line, banner).ok();
                        if verbose {
                            println!("{} | Banner: {}", line, banner);
                        }
                    } else {
                        writeln!(file, "{}", line).ok();
                        if verbose {
                            println!("{}", line);
                        }
                    }
                }
            }
        });
        tasks.push(handle);
    }

    if scan_udp_enabled {
        println!("[*] Starting UDP scan...");
        for port in 1..=65535 {
            let permit = semaphore.clone().acquire_owned().await?;
            let target = target.clone();
            let mut file = file.try_clone()?;

            let handle = tokio::spawn(async move {
                let _permit = permit;
                if let Some(status) = scan_udp(&target, port, timeout_secs).await {
                    let line = format!("[UDP] {}:{} => {}", target, port, status);
                    if status == "OPEN" || !show_only_open {
                        writeln!(file, "{}", line).ok();
                        if verbose {
                            println!("{}", line);
                        }
                    }
                }
            });
            tasks.push(handle);
        }
    }

    for task in tasks {
        let _ = task.await;
    }

    println!("[*] Scan complete. Results saved to {}", output_file);
    Ok(())
}

/// TCP connect scan + banner grab
async fn scan_tcp(ip: &str, port: u16, timeout_secs: u64) -> Option<(String, String)> {
    let addr = format!("{}:{}", ip, port);
    match timeout(Duration::from_secs(timeout_secs), TcpStream::connect(&addr)).await {
        Ok(Ok(stream)) => {
            let mut buf = [0; 1024];
            match timeout(Duration::from_secs(2), stream.readable()).await {
                Ok(Ok(())) => match stream.try_read(&mut buf) {
                    Ok(n) if n > 0 => {
                        let banner = String::from_utf8_lossy(&buf[..n]).to_string();
                        Some(("OPEN".into(), banner))
                    }
                    _ => Some(("OPEN".into(), "".into())),
                },
                _ => Some(("OPEN".into(), "".into())),
            }
        }
        Ok(Err(_)) => Some(("CLOSED".into(), "".into())),
        Err(_) => Some(("TIMEOUT".into(), "".into())),
    }
}

/// UDP scan (null packet, timeout-based)
async fn scan_udp(ip: &str, port: u16, timeout_secs: u64) -> Option<String> {
    let local = "0.0.0.0:0".parse::<SocketAddr>().unwrap();
    let remote = format!("{}:{}", ip, port);
    let remote = match normalize_addr(&remote) {
        Ok(addr) => addr,
        Err(_) => return None,
    };

    let socket = UdpSocket::bind(local).await.ok()?;
    let _ = socket.send_to(b"\x00", &remote).await;
    let mut buf = [0u8; 512];

    match timeout(Duration::from_secs(timeout_secs), socket.recv_from(&mut buf)).await {
        Ok(Ok((_n, _))) => Some("OPEN".into()),
        _ => None,
    }
}

/// Normalize IP/hostname for bracket handling and clean input
fn normalize_target(input: &str) -> Result<String> {
    let input = input.trim().trim_start_matches('[').trim_end_matches(']').trim();
    if input.contains(':') && !input.contains('.') {
        // Likely IPv6, re-add brackets
        Ok(format!("[{}]", input))
    } else {
        Ok(input.to_string())
    }
}

/// Normalize and parse into a real SocketAddr
fn normalize_addr(input: &str) -> Result<SocketAddr> {
    // Remove extra brackets first
    let cleaned = input.trim().trim_start_matches('[').trim_end_matches(']');
    // If IPv6, wrap again properly
    let formatted = if cleaned.contains(':') && !cleaned.contains('.') {
        format!("[{}]", cleaned)
    } else {
        cleaned.to_string()
    };

    let addrs = formatted.to_socket_addrs()?;
    addrs.into_iter().next().ok_or_else(|| anyhow::anyhow!("Invalid address"))
}

/// Prompt for string input
fn prompt(message: &str) -> Result<String> {
    print!("{}", message);
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    Ok(buf.trim().to_string())
}

/// Prompt for boolean yes/no
fn prompt_bool(message: &str) -> Result<bool> {
    loop {
        let input = prompt(message)?;
        match input.to_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => println!("Please enter 'y' or 'n'."),
        }
    }
}

/// Prompt for number input
fn prompt_usize(message: &str) -> Result<usize> {
    loop {
        let input = prompt(message)?;
        if let Ok(n) = input.parse::<usize>() {
            return Ok(n);
        }
        println!("Please enter a valid number.");
    }
}
