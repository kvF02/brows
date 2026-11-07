//! Explicit, anonymous public-proxy diagnostics; never reads stored credentials.
use brows_adapters_proxy::TcpProxyConnectivityTester;
use brows_application::ProxyConnectivityTester;
use brows_domain::{DnsMode, ProxyId, ProxyScheme, ProxySpec};
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
    let proxy = ProxySpec {
        id: ProxyId::new("public-proxy-probe").expect("fixed valid identifier"),
        name: "Anonymous public proxy smoke test".to_owned(),
        scheme,
        host: host.to_string(),
        port,
        username: None,
        secret_ref: None,
        dns_mode: DnsMode::Proxy,
        bypass_hosts: Vec::new(),
        connect_timeout_ms: 5000,
    };
    match TcpProxyConnectivityTester.test(&proxy, None) {
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
