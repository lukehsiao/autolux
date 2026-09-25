//! Invalid limits must be rejected before autolux touches any hardware or
//! the system bus, so a typo in a service's environment file fails loudly and
//! the same way on every machine, including ones with no sensor or panel.

use std::process::{Command, Output};

fn autolux(args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_autolux"))
        .args(args)
        .env_remove("AUTOLUX_MIN")
        .env_remove("AUTOLUX_MAX")
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

fn assert_fails_with(output: &Output, message: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "should fail\nstderr:\n{stderr}");
    assert!(
        stderr.contains(message),
        "expected {message:?}\nstderr:\n{stderr}"
    );
}

#[test]
fn rejects_an_inverted_range() {
    let output = autolux(&["--min", "80", "--max", "20"], &[]);
    assert_fails_with(&output, "--min (80%) must not exceed --max (20%)");
}

#[test]
fn rejects_a_percentage_above_100() {
    let output = autolux(&["--max", "150"], &[]);
    assert_fails_with(&output, "150% is not a valid brightness");
}

#[test]
fn reads_limits_from_the_environment() {
    let output = autolux(&[], &[("AUTOLUX_MIN", "80"), ("AUTOLUX_MAX", "20")]);
    assert_fails_with(&output, "--min (80%) must not exceed --max (20%)");
}
