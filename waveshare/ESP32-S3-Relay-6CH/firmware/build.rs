use std::env;
use std::fs;

fn main() {
    embuild::espidf::sysenv::output();
    // Build-time seed (git-ignored): WiFi credentials and API key written to NVS on first boot.
    // RELAY_NO_SEED=1 builds an image with no secrets at all (used for public releases).
    // RELAY_SECRETS=<path> selects another seed file, e.g. one per unit.
    println!("cargo:rerun-if-env-changed=RELAY_NO_SEED");
    println!("cargo:rerun-if-env-changed=RELAY_SECRETS");
    if env::var("RELAY_NO_SEED").is_ok_and(|v| v == "1") {
        println!("cargo:warning=building WITHOUT a credential seed (release image)");
        return;
    }
    let path = env::var("RELAY_SECRETS").unwrap_or_else(|_| "secrets.env".into());
    println!("cargo:rerun-if-changed={path}");
    if let Ok(text) = fs::read_to_string(&path) {
        // Fingerprint so a changed seed file re-seeds NVS on the next boot.
        let mut h: u64 = 0xcbf29ce484222325;
        for b in text.bytes() {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
        println!("cargo:rustc-env=SEED_HASH={h:016x}");
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                println!("cargo:rustc-env=SEED_{}={}", k.trim(), v.trim());
            }
        }
    }
}
