use auditable_serde::VersionInfo;
use cargo_metadata::{Metadata, MetadataCommand};
use miniz_oxide::deflate::compress_to_vec_zlib;
use std::{collections::BTreeSet, convert::TryFrom, str::from_utf8};

use crate::{cargo_arguments::CargoArgs, rustc_arguments::RustcArgs};

/// Calls `cargo metadata` to obtain the dependency tree, serializes it to JSON and compresses it
pub fn compressed_dependency_list(rustc_args: &RustcArgs, target_triple: &str, known_features: &Option<BTreeSet<String>>) -> Vec<u8> {
    let metadata = get_metadata(rustc_args, target_triple, known_features);
    let version_info = VersionInfo::try_from(&metadata).unwrap();
    let json = serde_json::to_string(&version_info).unwrap();
    // compression level 7 makes this complete in a few milliseconds, so no need to drop to a lower level in debug mode
    let compressed_json = compress_to_vec_zlib(json.as_bytes(), 7);
    compressed_json
}

fn get_metadata(args: &RustcArgs, target_triple: &str, known_features: &Option<BTreeSet<String>>) -> Metadata {
    let mut metadata_command = MetadataCommand::new();

    // Cargo sets the path to itself in the `CARGO` environment variable:
    // https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-3rd-party-subcommands
    // This is also useful for using `cargo auditable` as a drop-in replacement for Cargo.
    if let Some(path) = std::env::var_os("CARGO") {
        metadata_command.cargo_path(path);
    }

    // Point cargo-metadata to the correct Cargo.toml in a workspace.
    // CARGO_MANIFEST_DIR env var will be set by Cargo when it calls our rustc wrapper
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR").unwrap();
    metadata_command.current_dir(manifest_dir);

    // Pass the features that are actually enabled for this crate to cargo-metadata
    let mut features = args.enabled_features();
    if let Some(index) = features.iter().position(|x| x == &"default") {
        features.remove(index);
    } else {
        metadata_command.features(cargo_metadata::CargoOpt::NoDefaultFeatures);
    }
    let mut owned_features: Vec<String> = features.iter().map(|s| s.to_string()).collect();
    // Cargo passes nonexistent features to `rustc` when the `dep:` syntax is used:
    // https://github.com/rust-secure-code/cargo-auditable/issues/124
    // We work around that by filtering the list Cargo passed to rustc against the list of known features,
    // if available.
    if let Some(known_features) = known_features {
        owned_features.retain(|f| known_features.contains(f));
    };
    metadata_command.features(cargo_metadata::CargoOpt::SomeFeatures(owned_features));

    // Restrict the dependency resolution to just the platform the binary is being compiled for.
    // By default `cargo metadata` resolves the dependency tree for all platforms.
    let mut other_args = vec!["--filter-platform".to_owned(), target_triple.to_owned()];

    // Pass arguments such as `--config`, `--offline` and `--locked`
    // from the original CLI invocation of `cargo auditable`
    let orig_args = CargoArgs::from_env()
        .expect("Env var 'CARGO_AUDITABLE_ORIG_ARGS' set by 'cargo-auditable' is unset!");
    other_args.extend_from_slice(&orig_args.to_args());

    // This can only be done once, multiple calls will replace previously set options.
    metadata_command.other_options(other_args);

    execute_cargo_metadata(&metadata_command).unwrap()
}

/// Re-implements `MetadataCommand::exec` to clear `RUSTC_WORKSPACE_WRAPPER`
/// in the child process to avoid recursion.
/// The alternative would be modifying the environment of our own process,
/// which is sketchy and discouraged on POSIX because it's not thread-safe:
/// https://doc.rust-lang.org/stable/std/env/fn.remove_var.html
pub fn execute_cargo_metadata(
    command: &MetadataCommand,
) -> Result<Metadata, cargo_metadata::Error> {
    let mut command = command.cargo_command();
    // this line is the only custom addition
    command.env_remove("RUSTC_WORKSPACE_WRAPPER");
    // end of custom code
    let output = command.output()?;
    if !output.status.success() {
        return Err(cargo_metadata::Error::CargoMetadata {
            stderr: String::from_utf8(output.stderr)?,
        });
    }
    let stdout = from_utf8(&output.stdout)?
        .lines()
        .find(|line| line.starts_with('{'))
        .ok_or(cargo_metadata::Error::NoJson)?;
    // TODO: check if workspace_default_members is available
    // to work around https://github.com/oli-obk/cargo_metadata/issues/254
    MetadataCommand::parse(stdout)
}
