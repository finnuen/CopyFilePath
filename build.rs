//! Writes the resource script: icon, version info, and the shell handler DLL.
//!
//! The version comes from `Cargo.toml` - `build.ps1` bumps the manifest, this file
//! turns it into the .exe's properties, and the app prints the same value. Cargo needs
//! `major.minor.patch`, so the patch digit is padding: everything a person sees is
//! `major.minor`.
//!
//! The handler DLL is built first and staged in `target/shell-helper` by `build.ps1`;
//! embedding it here is what keeps the release a single file. A plain `cargo build`
//! has no staged DLL, and says so instead of failing - the exe then refuses to install
//! the menu and names `build.bat`.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION is set by cargo");
    let description = env::var("CARGO_PKG_DESCRIPTION")
        .unwrap_or_default()
        .replace('"', "'"); // a quote would end the RC string early

    let numbers: Vec<u32> = version.split('.').filter_map(|p| p.parse().ok()).collect();
    // The version a person sees: major.minor. Cargo's patch digit is padding and
    // stays out of the file properties, the file name and the tag.
    let product = numbers
        .iter()
        .take(2)
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".");

    let mut quad = numbers;
    quad.resize(4, 0);
    let quad = format!("{},{},{},{}", quad[0], quad[1], quad[2], quad[3]);

    let icon = fs::canonicalize("assets/icon.ico").expect("assets/icon.ico is missing");
    let icon = icon.display().to_string().replace('\\', "\\\\");

    let handler = PathBuf::from("target/shell-helper/CopyFilePathShell.dll");
    println!("cargo:rerun-if-changed={}", handler.display());
    let handler_resource = match fs::canonicalize(&handler) {
        Ok(path) => format!("100 RCDATA \"{}\"\n", path.display().to_string().replace('\\', "\\\\")),
        Err(_) => {
            println!(
                "cargo:warning={} is missing: this exe carries no shell helper and cannot install the menu - run build.bat",
                handler.display()
            );
            String::new()
        }
    };

    let resource = format!(
        r#"1 ICON "{icon}"

{handler_resource}
1 VERSIONINFO
FILEVERSION {quad}
PRODUCTVERSION {quad}
FILEOS 0x40004
FILETYPE 1
{{
  BLOCK "StringFileInfo"
  {{
    BLOCK "040904B0"
    {{
      VALUE "FileDescription", "{description}"
      VALUE "ProductName", "Copy file path"
      VALUE "FileVersion", "{product}"
      VALUE "ProductVersion", "{product}"
      VALUE "OriginalFilename", "CopyFilePath.exe"
    }}
  }}
  BLOCK "VarFileInfo"
  {{
    VALUE "Translation", 0x0409, 0x04B0
  }}
}}
"#
    );

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by cargo")).join("app.rc");
    fs::write(&out, resource).expect("could not write the generated resource script");
    embed_resource::compile(&out, embed_resource::NONE);
}
