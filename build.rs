use std::{env, fs};

const KEYS: [&str; 2] = ["FASTKRAKEN_GITHUB_CLIENT_ID", "FASTKRAKEN_GITHUB_APP_SLUG"];

fn main() {
    let file = "packaging/github-app.env";
    println!("cargo:rerun-if-changed={file}");
    let text = fs::read_to_string(file).unwrap_or_default();
    for key in KEYS {
        println!("cargo:rerun-if-env-changed={key}");
        if env::var(key).is_ok_and(|value| !value.trim().is_empty()) {
            continue;
        }
        let value = text
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .find_map(|line| line.trim().strip_prefix(key)?.trim_start().strip_prefix('='))
            .map(str::trim)
            .unwrap_or_default();
        if !value.is_empty() {
            println!("cargo:rustc-env={key}={value}");
        }
    }
}
