//! The server binds to loopback by default and refuses a non-loopback address
//! without credentials, unless `--allow-unauthenticated` is given.

use std::sync::{Mutex, OnceLock};

use clap::Parser;
use grafeo_server::config::Config;

/// Serializes the in-process parses: `config` below removes the `GRAFEO_*`
/// environment variables while parsing, so two tests must not interleave.
fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Parses args with the `GRAFEO_*` environment removed: clap reads those for
/// every flag, so a variable set in the test runner's environment would
/// otherwise change the parse (the subprocess tests strip them for the child).
#[allow(unsafe_code)] // env mutation is unsafe in edition 2024; see the SAFETY note below
fn config(args: &[&str]) -> Config {
    let _guard = env_lock().lock().unwrap();
    // clap reads GRAFEO_* env vars for every flag, so a variable set in the
    // environment running the tests would change what is parsed here. The
    // subprocess tests strip the same variables for the child process.
    let saved: Vec<(String, String)> = std::env::vars()
        .filter(|(key, _)| key.starts_with("GRAFEO_"))
        .collect();
    // SAFETY: the lock keeps the removal and the restore in one critical
    // section, so no other test reads the environment in between.
    for (key, _) in &saved {
        unsafe { std::env::remove_var(key) };
    }
    let parsed =
        Config::try_parse_from(std::iter::once("grafeo-server").chain(args.iter().copied()));
    for (key, value) in saved {
        unsafe { std::env::set_var(key, value) };
    }
    parsed.unwrap()
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

#[cfg(feature = "gwp")]
#[test]
fn gwp_sessions_are_limited_by_default() {
    assert_eq!(config(&[]).gwp_max_sessions, 1000);
    assert_eq!(config(&["--gwp-max-sessions", "0"]).gwp_max_sessions, 0);
}

#[cfg(feature = "bolt")]
#[test]
fn bolt_sessions_are_limited_by_default() {
    assert_eq!(config(&[]).bolt_max_sessions, 1000);
    assert_eq!(config(&["--bolt-max-sessions", "0"]).bolt_max_sessions, 0);
}

/// #83: `--read-only` on a data directory without a database refuses to start
/// with a clear error and exit code 2 (it panicked with 101), and creates
/// nothing there.
#[test]
fn read_only_without_a_database_refuses_to_start_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_grafeo-server"));
    for (key, _) in std::env::vars() {
        if key.starts_with("GRAFEO_") {
            cmd.env_remove(key);
        }
    }
    let out = cmd
        .args([
            "--read-only",
            "--port",
            "0",
            "--log-level",
            "error",
            "--data-dir",
        ])
        .arg(dir.path())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(2), "{text}");
    assert!(text.contains("without --read-only"), "{text}");
    assert!(!text.contains("panicked"), "{text}");
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "nothing created"
    );
}

#[test]
#[allow(unsafe_code)] // see config(): the lock makes the mutation safe here
fn in_process_parse_ignores_grafeo_env_vars() {
    // SAFETY: every mutation happens under env_lock(), and config() takes the
    // same lock while it strips and restores the variables, so no parse ever
    // sees GRAFEO_HOST and no restore can interleave with the removal.
    {
        let _guard = env_lock().lock().unwrap();
        unsafe { std::env::set_var("GRAFEO_HOST", "0.0.0.0") };
    }
    let c = config(&[]);
    {
        let _guard = env_lock().lock().unwrap();
        unsafe { std::env::remove_var("GRAFEO_HOST") };
    }
    assert_eq!(c.host, "127.0.0.1");
}

/// Empty configured credentials are not credentials: `--host 0.0.0.0
/// --auth-user '' --auth-password ''` must still be refused, since a Basic
/// `:` header would authenticate against the empty pair.
#[test]
fn empty_credentials_do_not_satisfy_the_exposure_check() {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_grafeo-server"));
    for (key, _) in std::env::vars() {
        if key.starts_with("GRAFEO_") {
            cmd.env_remove(key);
        }
    }
    let out = cmd
        .args([
            "--host",
            "0.0.0.0",
            "--port",
            "0",
            "--auth-user",
            "",
            "--auth-password",
            "",
            "--log-level",
            "error",
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
