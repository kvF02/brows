//! Explicit proxy diagnostics; optional credentials come only from probe env vars.
use brows_adapters_proxy::TcpProxyConnectivityTester;
use brows_application::{ProxyConnectivityTester, SecretValue};
use brows_domain::{DnsMode, ProxyId, ProxyScheme, ProxySpec, SecretRef};
use std::{env, net::Ipv4Addr, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() != 3 {
        eprintln!("Usage: public_proxy_probe <http|socks5> <IPv4> <port>");
        return ExitCode::from(2);
    }
    let scheme = match args[0].as_str() {
        "http" => ProxyScheme::Http,
        "socks5" => ProxyScheme::Socks5,
        _ => return ExitCode::from(2),
    };
    let (Ok(host), Ok(port)) = (args[1].parse::<Ipv4Addr>(), args[2].parse::<u16>()) else {
        return ExitCode::from(2);
    };
    let mut proxy = ProxySpec {
        id: ProxyId::new("public-proxy-probe").expect("fixed valid identifier"),
        name: "Explicit proxy smoke test".to_owned(),
        scheme,
        host: host.to_string(),
        port,
        username: None,
        secret_ref: None,
        dns_mode: DnsMode::Proxy,
        bypass_hosts: Vec::new(),
        connect_timeout_ms: 5000,
    };
    let credential = match (
        env::var("BROWS_PROXY_PROBE_USERNAME"),
        env::var("BROWS_PROXY_PROBE_PASSWORD"),
    ) {
        (Err(env::VarError::NotPresent), Err(env::VarError::NotPresent)) => None,
        (Ok(username), Ok(password)) => match set_credentials(&mut proxy, &username, &password) {
            Ok(secret) => Some(secret),
            Err(()) => return ExitCode::from(2),
        },
        _ => return ExitCode::from(2),
    };
    match TcpProxyConnectivityTester.test(&proxy, credential.as_ref()) {
        Ok(check) => {
            println!(
                "OK|{}|{}|{}",
                check.reachable,
                check.exit_ip_checked,
                check.exit_ip.as_deref().unwrap_or("none")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("ERROR|{:?}|{}", error.kind, error.message);
            ExitCode::FAILURE
        }
    }
}

fn set_credentials(
    proxy: &mut ProxySpec,
    username: &str,
    password: &str,
) -> Result<SecretValue, ()> {
    if username.trim().is_empty()
        || [username, password].iter().any(|value| {
            value.is_empty() || value.len() > 255 || value.chars().any(char::is_control)
        })
        || username.contains(':')
    {
        return Err(());
    }
    proxy.username = Some(username.to_owned());
    proxy.secret_ref =
        Some(SecretRef::new("probe-credential").expect("fixed valid secret identifier"));
    SecretValue::new(password.as_bytes().to_vec()).map_err(|_| ())
}
