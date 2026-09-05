use std::fs;

fn main() {
    embuild::espidf::sysenv::output();
    // Build-time secrets (git-ignored). Seeded into NVS on first boot only.
    println!("cargo:rerun-if-changed=secrets.env");
    if let Ok(text) = fs::read_to_string("secrets.env") {
        // Fingerprint so a changed secrets.env re-seeds NVS on the next boot.
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
