//! Makes sure the web UI has been built before the assets are embedded.
//!
//! `web/dist` is committed so `cargo install duw` works without Node. For
//! contributors working from a fresh checkout we try to build it once.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=web/dist/index.html");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let dist = Path::new(&manifest).join("web/dist/index.html");
    if dist.exists() {
        return;
    }

    let web = Path::new(&manifest).join("web");
    if !web.join("package.json").exists() {
        panic!(
            "web/dist is missing and web/package.json was not found; \
             this package was assembled incorrectly"
        );
    }

    println!("cargo:warning=web/dist is missing, running `npm install && npm run build`");
    run(&web, &["install"]);
    run(&web, &["run", "build"]);

    if !dist.exists() {
        panic!("web build finished but web/dist/index.html still does not exist");
    }
}

fn run(dir: &Path, args: &[&str]) {
    // npm is a shell script on Unix and a .cmd shim on Windows.
    let program = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| {
            panic!(
                "web/dist is missing and `{program} {}` could not be run ({e}); \
                 install Node.js and run `npm install && npm run build` in web/",
                args.join(" ")
            )
        });
    if !status.success() {
        panic!("`{program} {}` failed with {status}", args.join(" "));
    }
}
