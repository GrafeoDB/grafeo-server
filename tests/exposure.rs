//! The server binds to loopback by default and refuses a non-loopback address
//! without credentials, unless `--allow-unauthenticated` is given.

use clap::Parser;
use grafeo_server::config::Config;

fn config(args: &[&str]) -> Config {
    Config::try_parse_from(std::iter::once("grafeo-server").chain(args.iter().copied())).unwrap()
}

#[test]
fn the_default_host_is_loopback() {
    let c = config(&[]);
    assert_eq!(c.host, "127.0.0.1");
    assert!(c.check_exposure(false).is_ok());
}

#[test]
fn a_network_address_without_credentials_is_refused() {
    for host in ["0.0.0.0", "::", "192.168.1.5"] {
        let err = config(&["--host", host]).check_exposure(false).unwrap_err();
        assert!(err.contains("--allow-unauthenticated"), "{host}: {err}");
        assert!(err.contains("127.0.0.1"), "{host}: {err}");
    }
}

#[test]
fn credentials_or_the_flag_allow_a_network_address() {
    assert!(config(&["--host", "0.0.0.0"]).check_exposure(true).is_ok());
    assert!(
        config(&["--host", "0.0.0.0", "--allow-unauthenticated"])
            .check_exposure(false)
            .is_ok()
    );
}

#[test]
fn every_loopback_address_passes() {
    for host in ["127.0.0.1", "127.0.0.2", "::1"] {
        assert!(
            config(&["--host", host]).check_exposure(false).is_ok(),
            "{host}"
        );
    }
}

/// The binary applies the rule at start-up: it exits with an error before
/// binding anything.
#[test]
fn the_binary_refuses_to_start_exposed_without_credentials() {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_grafeo-server"));
    for (key, _) in std::env::vars() {
        if key.starts_with("GRAFEO_") {
            cmd.env_remove(key);
        }
    }
    let out = cmd
        .args(["--host", "0.0.0.0", "--port", "0", "--log-level", "error"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("--allow-unauthenticated"), "{text}");
}
