//! Opt-in checks on HEC's public example models against an independent h5py oracle.
//!
//! Set `TERRIO_HECRAS_TEST_DATA` to a folder of the example models (see
//! `scripts/oracle_public_models.py` for the files) and `TERRIO_HECRAS_ORACLE` to the JSON that
//! script writes for them. Without both, the test reports that it was skipped and passes.
#![cfg(feature = "results")]

use sha2::{Digest, Sha256};
use std::path::PathBuf;
use terrio::FieldValue;
use terrio_hecras::{HecRasHdf, MeshCellOptions};

/// Summaries the engine refuses on these files, with the refusal it gives. ras-commander reads
/// them with h5py; terrio must surface the refusal rather than an empty or guessed field.
const KNOWN_REFUSALS: &[(&str, &str)] =
    &[("BaldEagleCrkMulti2D/BaldEagleDamBrk.p07.hdf", "HEC-RAS unsupported(identity_layout): BaldEagleCr Maximum Water Surface")];

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn public_models_match_the_h5py_oracle() {
    let (Some(root), Some(oracle)) = (std::env::var_os("TERRIO_HECRAS_TEST_DATA"), std::env::var_os("TERRIO_HECRAS_ORACLE")) else {
        eprintln!("skipped: set TERRIO_HECRAS_TEST_DATA and TERRIO_HECRAS_ORACLE");
        return;
    };
    let oracle: serde_json::Value = serde_json::from_slice(&std::fs::read(oracle).unwrap()).unwrap();
    let files = oracle.as_object().unwrap();
    assert!(!files.is_empty(), "the oracle lists no files");
    for (rel, want) in files {
        let path = PathBuf::from(&root).join(rel);
        assert_eq!(sha256_hex(&std::fs::read(&path).unwrap()), want["sha256"].as_str().unwrap(), "{rel}: not the file the oracle read");
        let hdf = HecRasHdf::open(&path).unwrap();
        assert_eq!(hdf.crs().unwrap().as_deref(), want["projection"].as_str(), "{rel} CRS");
        assert_eq!(hdf.units_system().unwrap().as_deref(), want["units_system"].as_str(), "{rel} units");
        let meshes = want["meshes"].as_object().unwrap();
        assert_eq!(hdf.mesh_names().unwrap(), meshes.keys().cloned().collect::<Vec<_>>(), "{rel} meshes");

        let cells = hdf.mesh_cells(&MeshCellOptions::default()).unwrap();
        assert_eq!(cells.layer.crs.as_ref().and_then(|c| c.wkt.as_deref()), want["projection"].as_str());
        assert!(cells.layer.crs.as_ref().is_none_or(|c| c.epsg.is_none()), "no EPSG code is guessed");
        let mut offset = 0;
        for (name, mesh) in meshes {
            let drawn = mesh["drawn"].as_u64().unwrap() as usize;
            let ids = &mesh["drawn_cell_ids"];
            let rows = &cells.layer.features[offset..offset + drawn];
            assert!(rows.iter().all(|f| f.attributes[0] == FieldValue::Text(name.clone())), "{rel} {name}");
            assert_eq!(rows[0].attributes[1], FieldValue::Integer(ids[0].as_i64().unwrap()));
            assert_eq!(rows[drawn - 1].attributes[1], FieldValue::Integer(ids[1].as_i64().unwrap()));
            let stored = mesh["stored_cells"].as_u64().unwrap() as usize;
            assert_eq!(cells.omitted.iter().filter(|o| &o.mesh_name == name).count(), stored - drawn, "{rel} {name} omitted");
            offset += drawn;
        }
        assert_eq!(offset, cells.layer.len(), "{rel}: every polygon belongs to a listed mesh");

        if !meshes.values().any(|m| m.get("maximum_water_surface").is_some()) {
            continue;
        }
        let joined = hdf.mesh_cells(&MeshCellOptions::default().with_summary("Maximum Water Surface"));
        if let Some((_, refusal)) = KNOWN_REFUSALS.iter().find(|(f, _)| f == rel) {
            let e = joined.err().unwrap_or_else(|| panic!("{rel}: the engine now qualifies this summary; update KNOWN_REFUSALS"));
            assert!(matches!(&e, terrio::Error::Unsupported(m) if m.starts_with(refusal)), "{rel}: {e}");
            continue;
        }
        let joined = joined.unwrap();
        assert_eq!(joined.summaries[0].time_field.as_deref(), Some("maximum_water_surface_time"));
        let mut offset = 0;
        for mesh in meshes.values() {
            let drawn = mesh["drawn"].as_u64().unwrap() as usize;
            let column = |i: usize| -> Vec<u8> {
                joined.layer.features[offset..offset + drawn]
                    .iter()
                    .flat_map(|f| match f.attributes[i] {
                        FieldValue::Float(v) => v.to_le_bytes(),
                        ref other => panic!("{rel}: {other:?}"),
                    })
                    .collect()
            };
            let want = &mesh["maximum_water_surface"];
            assert_eq!(sha256_hex(&column(2)), want["value_sha256"].as_str().unwrap(), "{rel} values");
            assert_eq!(sha256_hex(&column(3)), want["time_sha256"].as_str().unwrap(), "{rel} time row");
            offset += drawn;
        }
    }
}
