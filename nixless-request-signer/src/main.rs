use std::path::PathBuf;

use anyhow::{anyhow, Context};
use clap::{Parser, Subcommand};
use nix_core::NixStylePrivateKey;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Signs a file.
    Sign {
        #[arg(long)]
        file_path: PathBuf,

        #[arg(long)]
        private_key_encoded: String,
    },
    /// Returns the public key of an encoded private key.
    GetPublicKey {
        #[arg(long)]
        private_key_encoded: String,
    },
}

fn sign_file(path: PathBuf, private_key_encoded: String) -> anyhow::Result<String> {
    if !path.exists() {
        return Err(anyhow!(
            "File at path {} doesn't exist!",
            path.to_string_lossy()
        ));
    }

    if !path.is_file() {
        return Err(anyhow!(
            "Path {} doesn't point to a file!",
            path.to_string_lossy()
        ));
    }

    let mut pk = NixStylePrivateKey::from_nix_format(&private_key_encoded)
        .context("failed to read the given private key")?;
    let file_contents = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "failed to read the contents of the file at '{}'",
            path.to_string_lossy()
        )
    })?;
    Ok(pk
        .sign_to_base64(file_contents.trim().as_bytes())
        .context("failed to sign the contents of the file")?)
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Sign {
            file_path,
            private_key_encoded,
        } => {
            let signature = sign_file(file_path, private_key_encoded)?;
            println!("{}", signature);
        }
        Command::GetPublicKey {
            private_key_encoded,
        } => {
            let pk = NixStylePrivateKey::from_nix_format(&private_key_encoded)
                .context("failed to read the given private key")?;
            println!("{}", pk.public_key_nix_format());
        }
    }

    Ok(())
}
