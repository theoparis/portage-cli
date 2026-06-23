//! `em select clang` — LLVM/clang slot selection.
//!
//! Manages which LLVM/clang version (slot) is active. Unlike gcc which uses
//! env.d/gcc/ profiles, clang is installed under /usr/lib/llvm/${SLOT}/ and
//! uses symlinks managed by the clang-toolchain-symlinks package.

use std::io::Write as _;

use anyhow::{Context, Result, bail};
use camino::Utf8PathBuf;

use super::config_portage_dir;
use crate::cli::{ClangAction, Cli};
use crate::style::{C_HOST, C_PREFIX, C_STAR};

/// Base directory for LLVM installations.
fn llvm_base_dir(globals: &Cli) -> Utf8PathBuf {
    // Check if we're in a prefix/local context
    let roots = globals.roots();
    let is_prefix_context = roots.config().is_none() && roots.config_overlay().is_some();

    if is_prefix_context {
        // For prefix, LLVM would be under EPREFIX/usr/lib/llvm
        if let Some(eprefix) = roots.eprefix() {
            return eprefix.join("usr/lib/llvm");
        }
    }
    // System location
    Utf8PathBuf::from("/usr/lib/llvm")
}

/// Path to the current clang slot config file.
fn current_clang_slot_path(globals: &Cli) -> Utf8PathBuf {
    let config_dir = config_portage_dir(globals);
    config_dir.join("clang").join("current-slot")
}

/// An LLVM/clang slot.
#[derive(Debug, Clone)]
struct ClangSlot {
    name: String,
    /// Whether this slot is from the host system or the current config root
    is_host: bool,
}

/// List all available LLVM/clang slots, grouped by... (no target grouping for clang)
fn list_all_clang_slots(globals: &Cli) -> Result<Vec<ClangSlot>> {
    let mut slots: Vec<ClangSlot> = Vec::new();

    // Check if we're in a prefix/local context
    let roots = globals.roots();
    let is_prefix_context = roots.config().is_none() && roots.config_overlay().is_some();

    // Collect slots from the current config root (prefix/local)
    let prefix_llvm_dir = llvm_base_dir(globals);
    if prefix_llvm_dir.is_dir() {
        collect_clang_slots(&prefix_llvm_dir, &mut slots, false)?;
    }

    // If in prefix context, also check system location
    if is_prefix_context {
        let system_dir = Utf8PathBuf::from("/usr/lib/llvm");
        if system_dir.is_dir() {
            collect_clang_slots(&system_dir, &mut slots, true)?;
        }
    }

    // Sort by slot name (version)
    slots.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(slots)
}

/// Helper to collect clang slots from a directory
fn collect_clang_slots(
    llvm_dir: &Utf8PathBuf,
    slots: &mut Vec<ClangSlot>,
    is_host: bool,
) -> Result<()> {
    if !llvm_dir.is_dir() {
        return Ok(());
    }

    for entry in std::fs::read_dir(llvm_dir)? {
        let entry = entry?;
        let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
            continue;
        };
        let name = path.file_name().unwrap_or_default().to_string();

        // Skip non-directory entries
        if !path.is_dir() {
            continue;
        }

        // LLVM slots are numeric (e.g., "15", "16", "17", "22") or major.minor (e.g., "17.0")
        // We use a simple heuristic: if it starts with a digit, it's likely a slot
        if name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            slots.push(ClangSlot { name, is_host });
        }
    }

    Ok(())
}

/// Get the current clang slot.
fn get_current_clang_slot(globals: &Cli) -> Option<String> {
    let config_path = current_clang_slot_path(globals);
    if let Ok(content) = std::fs::read_to_string(&config_path) {
        for line in content.lines() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                return Some(line.to_string());
            }
        }
    }
    None
}

/// Set the current clang slot.
fn set_clang_slot(globals: &Cli, slot: &str) -> Result<()> {
    let config_path = current_clang_slot_path(globals);

    // Verify the slot exists
    let llvm_dir = llvm_base_dir(globals);
    let slot_dir = llvm_dir.join(slot);
    if !slot_dir.is_dir() {
        // Also check system location
        let system_slot_dir = Utf8PathBuf::from(format!("/usr/lib/llvm/{}", slot));
        if !system_slot_dir.is_dir() {
            bail!("LLVM slot '{}' not found", slot);
        }
    }

    // Ensure the config directory exists
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent))?;
    }

    std::fs::write(&config_path, format!("{}\n", slot))
        .with_context(|| format!("writing {}", config_path))?;

    Ok(())
}

pub fn run(action: &ClangAction, globals: &Cli) -> Result<()> {
    match action {
        ClangAction::List => list(globals),
        ClangAction::Show => show(globals),
        ClangAction::Set { slot } => set(globals, slot),
    }
}

fn list(globals: &Cli) -> Result<()> {
    let slots = list_all_clang_slots(globals)?;
    let mut out = anstream::stdout();

    if slots.is_empty() {
        println!("No LLVM/clang slots found");
        return Ok(());
    }

    let total_count = slots.len();
    let num_width = total_count.to_string().len();

    let current = get_current_clang_slot(globals);

    for (idx, slot) in slots.iter().enumerate() {
        let n = idx + 1;
        let is_current = current.as_deref() == Some(&slot.name);
        let num = format!("[{:>width$}]", n, width = num_width);
        let mut slot_display = if is_current {
            format!("{}{C_STAR} *{C_STAR:#}", slot.name)
        } else {
            slot.name.clone()
        };

        // Add source label if in prefix context
        let roots = globals.roots();
        let is_prefix_context = roots.config().is_none() && roots.config_overlay().is_some();
        if is_prefix_context {
            let label = if slot.is_host {
                format!("{C_HOST} (host){C_HOST:#}")
            } else {
                format!("{C_PREFIX} (prefix){C_PREFIX:#}")
            };
            slot_display.push_str(&label);
        }

        writeln!(out, "  {num} {}", slot_display).ok();
    }

    Ok(())
}

fn show(globals: &Cli) -> Result<()> {
    match get_current_clang_slot(globals) {
        Some(slot) => println!("{}", slot),
        None => println!("(no LLVM/clang slot set)"),
    }
    Ok(())
}

fn set(globals: &Cli, slot: &str) -> Result<()> {
    set_clang_slot(globals, slot)?;
    println!(">>> LLVM/clang slot set: {}", slot);
    Ok(())
}
