//! Build-time Windows resource embedding for the `portlens` executable.

use std::env;
use std::path::Path;

const PRODUCT_NAME: &str = "PortLens";

fn main() {
    println!("cargo::rerun-if-changed=assets");

    if target_os().as_deref() != Ok("windows") {
        return;
    }

    let icon_path = Path::new("assets/icon.ico");
    if !icon_path.is_file() {
        println!(
            "cargo::warning=Windows builds look for assets/icon.ico. assets/icon.png is source artwork only and is not embedded into the executable."
        );
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/icon.ico");

    for (key, value) in version_strings() {
        resource.set(key, &value);
    }

    if let Err(error) = resource.compile() {
        panic!("failed to compile Windows resources: {error}");
    }
}

fn target_os() -> Result<String, env::VarError> {
    env::var("CARGO_CFG_TARGET_OS")
}

fn version_strings() -> [(&'static str, String); 5] {
    let version = cargo_package("CARGO_PKG_VERSION");
    let internal_name = cargo_package("CARGO_PKG_NAME");
    let executable_name = format!("{internal_name}.exe");
    let description = cargo_package("CARGO_PKG_DESCRIPTION");

    [
        ("FileDescription", description),
        ("FileVersion", version),
        ("InternalName", internal_name),
        ("OriginalFilename", executable_name),
        ("ProductName", PRODUCT_NAME.to_owned()),
    ]
}

fn cargo_package(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| String::new())
}
