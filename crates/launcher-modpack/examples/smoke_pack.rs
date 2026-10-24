//! Creates a small, real MRPACK acceptance fixture without bundling game or mod binaries.

use std::{io::Write as _, path::PathBuf};

use anyhow::{Context as _, Result};
use zip::{ZipWriter, write::SimpleFileOptions};

fn main() -> Result<()> {
    let target = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .context("usage: cargo run -p launcher-modpack --example smoke_pack -- OUTPUT.mrpack")?;
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .with_context(|| {
            format!(
                "create `{}` without overwriting an existing file",
                target.display()
            )
        })?;
    let mut archive = ZipWriter::new(output);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    archive.start_file("modrinth.index.json", options)?;
    archive.write_all(include_bytes!("../tests/fixtures/smoke-index.json"))?;
    archive.start_file("overrides/config/nudge-validation.txt", options)?;
    archive.write_all(b"This file was safely imported by Nudge.\n")?;
    archive.finish()?.sync_all()?;
    println!("{}", target.display());
    Ok(())
}
