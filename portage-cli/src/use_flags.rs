use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use portage_repo::{DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF, MakeConf};

pub fn run(add: &[String], remove: &[String], make_conf: Option<&Utf8Path>) -> Result<()> {
    let path = resolve_path(make_conf)?;

    if add.is_empty() && remove.is_empty() {
        return show(&path);
    }

    let mut mc = MakeConf::load(&path).with_context(|| format!("reading {path}"))?;

    let current = mc.get("USE").unwrap_or("").to_string();
    let mut flags: Vec<String> = current.split_whitespace().map(str::to_string).collect();

    for flag in remove {
        let flag = flag.trim_start_matches('+');
        flags.retain(|f| f != flag && f != &format!("+{flag}"));
    }

    for flag in add {
        let flag = flag.trim_start_matches('+');
        flags.retain(|f| f != flag && f != &format!("+{flag}") && f != &format!("-{flag}"));
        flags.push(flag.to_string());
    }

    let new_use = flags.join(" ");
    mc.set("USE", &new_use);
    mc.save(&path).with_context(|| format!("writing {path}"))?;

    println!("USE=\"{}\"", new_use);
    Ok(())
}

fn show(path: &Utf8Path) -> Result<()> {
    let mc = MakeConf::load(path).with_context(|| format!("reading {path}"))?;

    match mc.get("USE") {
        Some(val) => println!("USE=\"{}\"", val),
        None => println!("USE not set in {}", path),
    }
    Ok(())
}

fn resolve_path(override_path: Option<&Utf8Path>) -> Result<Utf8PathBuf> {
    if let Some(p) = override_path {
        return Ok(p.to_owned());
    }
    for candidate in [DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF] {
        let p = Utf8Path::new(candidate);
        if p.exists() {
            return Ok(p.to_owned());
        }
    }
    bail!(
        "no make.conf found at {} or {}",
        DEFAULT_MAKE_CONF,
        LEGACY_MAKE_CONF
    )
}
