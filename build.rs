use std::process::Command;

fn main() {
    Command::new("cargo")
        .args(["about", "generate", "about.hbs"])
        .stdout(std::fs::File::create("src/licenses.md").unwrap())
        .status()
        .expect("cargo-about failed");

    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=about.hbs");
    #[cfg(target_os = "linux")]
    println!("cargo:rustc-link-lib=cap");
}
