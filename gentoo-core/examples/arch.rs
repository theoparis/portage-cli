//! Gentoo Architecture Demo
//!
//! Demonstrates the KnownArch enumeration and Arch typed keyword.

use gentoo_core::{Arch, KnownArch};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Gentoo Architecture Demo");
    println!("=========================\n");

    let architectures = [
        "amd64",
        "x86_64",
        "arm",
        "aarch64",
        "arm64",
        "riscv64",
        "powerpc64",
        "ppc64",
        "i686",
        "armv7",
        "riscv32",
        "powerpc",
        "ppc",
    ];

    println!("KnownArch parsing:");
    for arch_str in architectures {
        match KnownArch::parse(arch_str) {
            Ok(arch) => println!(
                "  {} -> {} (keyword: {}, bitness: {})",
                arch_str,
                arch,
                arch.as_keyword(),
                arch.bitness()
            ),
            Err(e) => println!("  {} -> Error: {}", arch_str, e),
        }
    }

    println!("\nArch global interning:");
    let exotic = Arch::intern("mymachine");
    println!(
        "  intern(\"mymachine\") -> {:?}  as_keyword={}",
        exotic,
        exotic.as_keyword()
    );
    let known = Arch::intern("amd64");
    println!(
        "  intern(\"amd64\")     -> {:?}  as_keyword={}",
        known,
        known.as_keyword()
    );

    println!("\nArch::from_str:");
    let arch: Arch = "arm64".parse()?;
    println!(
        "  \"arm64\".parse() -> {:?}  as_keyword={}",
        arch,
        arch.as_keyword()
    );
    let arch: Arch = "custom-board".parse()?;
    println!(
        "  \"custom-board\".parse() -> {:?}  as_keyword={}",
        arch,
        arch.as_keyword()
    );

    println!("\nCHOST parsing:");
    for chost in [
        "x86_64-pc-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "powerpc64le-unknown-linux-gnu",
    ] {
        if let Some(arch) = Arch::from_chost(chost) {
            println!("  {} -> {}", chost, arch.as_keyword());
        }
    }

    println!("\nCurrent system architecture:");
    let arch = Arch::current();
    println!("  {} (keyword: {})", arch, arch.as_keyword());

    Ok(())
}
