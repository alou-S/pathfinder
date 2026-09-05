use std::process::Command;

fn main() {
    let is_release = std::env::var("OPT_LEVEL").as_deref() == Ok("3");

    if is_release {
        Command::new("cargo")
            .args(["about", "generate", "about.hbs"])
            .stdout(std::fs::File::create("src/licenses.md").unwrap())
            .status()
            .expect("cargo-about failed");
    }

    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=about.hbs");

    let target = std::env::var("TARGET").unwrap();

    if target.contains("linux") {
        println!("cargo:rustc-link-lib=cap");
    }

    if target.contains("windows") {
        use embedinator::{Icon, ResourceBuilder};

        ResourceBuilder::from_env()
            .add_icon(
                32512,
                Icon::from_png_bytes(std::fs::read("assets/icon.png").unwrap()),
            )
            .finish();
    }
}
