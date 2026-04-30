//! Vendored from `cargo::core::compiler::lto`.
//!
//! Cargo computes LTO modes per-unit and folds them into `Metadata::c_extra_filename`,
//! which determines the hash in artifact filenames (`lib<crate>-<HASH>.rlib`). Our
//! post-build harvest needs to ask cargo's `CompilationFiles` for those filenames,
//! and `CompilationFiles::metadata(unit)` panics with `no entry found for key` if
//! `BuildRunner::lto` isn't populated. The real `cargo::core::compiler::lto::generate`
//! is `pub` but its parent module is private, so external crates can't reach it.
//!
//! Ported verbatim from cargo 0.94.0's `cargo/src/cargo/core/compiler/lto.rs`.
//! License: MIT/Apache-2.0 (same as cargo).

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use anyhow::Result;
use cargo::core::compiler::{BuildContext, CompileMode, CrateType, Lto, Unit};
use cargo::core::profiles;

pub fn generate(bcx: &BuildContext<'_, '_>) -> Result<HashMap<Unit, Lto>> {
    let mut map = HashMap::new();
    for unit in bcx.roots.iter() {
        let root_lto = match unit.profile.lto {
            profiles::Lto::Bool(false) => Lto::OnlyObject,
            profiles::Lto::Off => Lto::Off,
            _ => {
                let crate_types = unit.target.rustc_crate_types();
                if unit.target.for_host() {
                    Lto::OnlyObject
                } else if needs_object(&crate_types) {
                    lto_when_needs_object(&crate_types)
                } else {
                    Lto::OnlyBitcode
                }
            }
        };
        calculate(bcx, &mut map, unit, root_lto)?;
    }
    Ok(map)
}

fn needs_object(crate_types: &[CrateType]) -> bool {
    crate_types.iter().any(|k| k.can_lto() || k.is_dynamic())
}

fn lto_when_needs_object(crate_types: &[CrateType]) -> Lto {
    if crate_types.iter().all(|ct| *ct == CrateType::Dylib) {
        Lto::OnlyObject
    } else {
        Lto::ObjectAndBitcode
    }
}

fn calculate(
    bcx: &BuildContext<'_, '_>,
    map: &mut HashMap<Unit, Lto>,
    unit: &Unit,
    parent_lto: Lto,
) -> Result<()> {
    let crate_types = match unit.mode {
        CompileMode::Test | CompileMode::Doctest => vec![CrateType::Bin],
        _ => unit.target.rustc_crate_types(),
    };
    let all_lto_types = crate_types.iter().all(CrateType::can_lto);
    let lto = if unit.target.for_host() {
        Lto::OnlyObject
    } else if all_lto_types {
        match unit.profile.lto {
            profiles::Lto::Named(s) => Lto::Run(Some(s)),
            profiles::Lto::Off => Lto::Off,
            profiles::Lto::Bool(true) => Lto::Run(None),
            profiles::Lto::Bool(false) => Lto::OnlyObject,
        }
    } else {
        match (parent_lto, needs_object(&crate_types)) {
            (Lto::Run(_), false) => Lto::OnlyBitcode,
            (Lto::Run(_), true) | (Lto::OnlyBitcode, true) => lto_when_needs_object(&crate_types),
            (Lto::Off, _) => Lto::Off,
            (_, false) | (Lto::OnlyObject, true) | (Lto::ObjectAndBitcode, true) => parent_lto,
        }
    };

    let merged_lto = match map.entry(unit.clone()) {
        Entry::Vacant(v) => *v.insert(lto),
        Entry::Occupied(mut v) => {
            let result = match (lto, v.get()) {
                (Lto::OnlyBitcode, Lto::OnlyBitcode) => Lto::OnlyBitcode,
                (Lto::OnlyObject, Lto::OnlyObject) => Lto::OnlyObject,
                (Lto::Run(s), _) | (_, &Lto::Run(s)) => Lto::Run(s),
                (Lto::Off, _) | (_, Lto::Off) => Lto::Off,
                (Lto::ObjectAndBitcode, _) | (_, Lto::ObjectAndBitcode) => Lto::ObjectAndBitcode,
                (Lto::OnlyObject, Lto::OnlyBitcode) | (Lto::OnlyBitcode, Lto::OnlyObject) => {
                    Lto::ObjectAndBitcode
                }
            };
            if result == *v.get() {
                return Ok(());
            }
            v.insert(result);
            result
        }
    };

    if let Some(deps) = bcx.unit_graph.get(unit) {
        for dep in deps {
            calculate(bcx, map, &dep.unit, merged_lto)?;
        }
    }
    Ok(())
}
