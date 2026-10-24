//! Opt-in checks on HEC's public example models against an independent h5py oracle.
//!
//! Set `TERRIO_HECRAS_TEST_DATA` to a folder of the example HDF files and
//! `TERRIO_HECRAS_ORACLE` to the JSON that `scripts/oracle_public_models.py` writes for them.
//! Without both, the test reports that it was skipped and passes.
#![cfg(feature = "results")]

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use terrio::{FieldValue, Geometry, Layer};
use terrio_hecras::{HecRasHdf, MeshOptions};

/// Reads the pinned engine refuses on these files, with the refusal it gives. ras-commander has
/// no h5py reading for them either (HEC-RAS 5.0 files without `2D Flow Areas/Attributes`).
/// Through terrio a refusal must be an error, never an empty or guessed layer. A refusal not
/// listed here fails the test, and so does a listed one that starts to succeed.
const KNOWN_REFUSALS: &[(&str, &str, &str)] = &[
    ("BaldEagleDamBrk.p07.hdf", "maximum_water_surface", "HEC-RAS unsupported(identity_layout)"),
    ("BaldEagleDamBrk.p08.hdf", "maximum_water_surface", "HEC-RAS unsupported(identity_layout)"),
    ("BaldEagleDamBrk.p19.hdf", "maximum_water_surface", "HEC-RAS unsupported(identity_layout)"),
    ("BaldEagleDamBrk.p07.hdf", "maximum_face_velocity", "HEC-RAS unsupported(identity_layout)"),
    ("BaldEagleDamBrk.p08.hdf", "maximum_face_velocity", "HEC-RAS unsupported(identity_layout)"),
    ("BaldEagleDamBrk.p19.hdf", "maximum_face_velocity", "HEC-RAS unsupported(identity_layout)"),
    ("BaldEagleDamBrk.g12.hdf", "mesh_cell_points", "HEC-RAS not_found"),
    ("BaldEagleDamBrk.p07.hdf", "mesh_cell_points", "HEC-RAS not_found"),
    ("BaldEagleDamBrk.p08.hdf", "mesh_cell_points", "HEC-RAS not_found"),
    ("BaldEagleDamBrk.p19.hdf", "mesh_cell_points", "HEC-RAS not_found"),
];

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// The oracle's line digest: per line its vertex count, then x, y as little-endian f64.
fn lines_digest<'a>(lines: impl Iterator<Item = Vec<(f64, f64)>> + 'a) -> String {
    let mut h = Sha256::new();
    for line in lines {
        h.update((line.len() as u64).to_le_bytes());
        for (x, y) in line {
            h.update(x.to_le_bytes());
            h.update(y.to_le_bytes());
        }
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn float_column(layer: &Layer, rows: std::ops::Range<usize>, field: &str) -> String {
    let i = layer.schema.fields().iter().position(|f| f.name == field).unwrap_or_else(|| panic!("no field {field}"));
    let bytes: Vec<u8> = layer.features[rows]
        .iter()
        .flat_map(|f| match f.attributes[i] {
            FieldValue::Float(v) => v.to_le_bytes(),
            ref other => panic!("{field}: {other:?}"),
        })
        .collect();
    sha256_hex(&bytes)
}

struct Check<'a> {
    file: &'a str,
    refusals_seen: Vec<(String, String)>,
}

impl Check<'_> {
    /// `Some(value)` on success; `None` for a listed refusal (checked).
    fn expect<T>(&mut self, what: &str, r: terrio::Result<T>) -> Option<T> {
        let known = KNOWN_REFUSALS.iter().find(|(f, w, _)| *f == self.file && *w == what);
        match (r, known) {
            (Ok(v), None) => Some(v),
            (Ok(_), Some(_)) => panic!("{} {what}: now succeeds; update KNOWN_REFUSALS and the oracle checks", self.file),
            (Err(e), Some((_, _, prefix))) => {
                assert!(
                    matches!(&e, terrio::Error::Unsupported(m) | terrio::Error::InvalidArgument(m) if m.starts_with(prefix)),
                    "{} {what}: {e}",
                    self.file
                );
                self.refusals_seen.push((self.file.to_string(), what.to_string()));
                None
            }
            (Err(e), None) => panic!("{} {what}: unexpected refusal: {e}", self.file),
        }
    }
}

#[test]
fn public_models_match_the_h5py_oracle() {
    let (Some(root), Some(oracle)) = (std::env::var_os("TERRIO_HECRAS_TEST_DATA"), std::env::var_os("TERRIO_HECRAS_ORACLE")) else {
        eprintln!("skipped: set TERRIO_HECRAS_TEST_DATA and TERRIO_HECRAS_ORACLE");
        return;
    };
    let oracle: Value = serde_json::from_slice(&std::fs::read(oracle).unwrap()).unwrap();
    let files = oracle.as_object().unwrap();
    assert!(!files.is_empty(), "the oracle lists no files");
    let mut seen = Vec::new();
    let (mut cells_checked, mut faces_checked, mut families_checked) = (0usize, 0usize, 0usize);
    for (file, want) in files {
        let path = PathBuf::from(&root).join(file);
        assert_eq!(sha256_hex(&std::fs::read(&path).unwrap()), want["sha256"].as_str().unwrap(), "{file}: not the file the oracle read");
        let mut check = Check { file, refusals_seen: Vec::new() };
        let hdf = HecRasHdf::open(&path).unwrap();
        assert_eq!(hdf.crs().unwrap().as_deref(), want["projection"].as_str(), "{file} CRS");
        assert_eq!(hdf.units_system().unwrap().as_deref(), want["units_system"].as_str(), "{file} units");
        let layers = hdf.layer_names().unwrap();
        let meshes = want["meshes"].as_object().unwrap();

        if !meshes.is_empty() {
            let mut names = hdf.mesh_names().unwrap();
            names.sort();
            let mut want_names: Vec<_> = meshes.keys().cloned().collect();
            want_names.sort();
            assert_eq!(names, want_names, "{file} meshes");
            let order = hdf.mesh_names().unwrap();

            // Cells, with Maximum Water Surface where stored.
            let has_wse = meshes.values().any(|m| m.get("maximum_water_surface").is_some());
            let cells = hdf.mesh_cells(&MeshOptions::default()).unwrap();
            assert_eq!(cells.layer.crs.as_ref().and_then(|c| c.wkt.as_deref()), want["projection"].as_str());
            assert!(cells.layer.crs.as_ref().is_none_or(|c| c.epsg.is_none()), "no EPSG code is guessed");
            let joined = if has_wse {
                check.expect("maximum_water_surface", hdf.mesh_cells(&MeshOptions::default().with_summary("Maximum Water Surface")))
            } else {
                None
            };
            let mut offset = 0;
            for name in &order {
                let mesh = &meshes[name];
                let drawn = mesh["drawn"].as_u64().unwrap() as usize;
                let rows = offset..offset + drawn;
                let features = &cells.layer.features[rows.clone()];
                assert!(features.iter().all(|f| f.attributes[0] == FieldValue::Text(name.clone())), "{file} {name}");
                assert_eq!(features[0].attributes[1], FieldValue::Integer(mesh["drawn_cell_ids"][0].as_i64().unwrap()));
                assert_eq!(features[drawn - 1].attributes[1], FieldValue::Integer(mesh["drawn_cell_ids"][1].as_i64().unwrap()));
                let stored = mesh["stored_cells"].as_u64().unwrap() as usize;
                assert_eq!(cells.omitted.iter().filter(|o| &o.mesh_name == name).count(), stored - drawn, "{file} {name} omitted");
                if let Some(joined) = &joined {
                    let w = &mesh["maximum_water_surface"];
                    assert_eq!(
                        float_column(&joined.layer, rows.clone(), "maximum_water_surface"),
                        w["value_sha256"].as_str().unwrap(),
                        "{file} {name} WSE"
                    );
                    assert_eq!(
                        float_column(&joined.layer, rows.clone(), "maximum_water_surface_time"),
                        w["time_sha256"].as_str().unwrap(),
                        "{file} {name} WSE time"
                    );
                }
                cells_checked += drawn;
                offset += drawn;
            }
            assert_eq!(offset, cells.layer.len(), "{file}: every polygon belongs to a listed mesh");

            // Cell points and ghost flags.
            if let Some(points) = check.expect("mesh_cell_points", hdf.mesh_cell_points(&MeshOptions::default())) {
                let mut offset = 0;
                for name in &order {
                    let mesh = &meshes[name];
                    let stored = mesh["stored_cells"].as_u64().unwrap() as usize;
                    let physical = mesh["physical_cells"].as_u64().expect("ghost flags need the physical count") as usize;
                    let ghosts = points.layer.features[offset..offset + stored]
                        .iter()
                        .filter(|f| f.attributes[2] == FieldValue::Boolean(true))
                        .count();
                    assert_eq!(ghosts, stored - physical, "{file} {name} ghosts");
                    offset += stored;
                }
                assert_eq!(offset, points.layer.len());
            }

            // Faces, with Maximum Face Velocity where stored.
            let has_fv = meshes.values().any(|m| m.get("maximum_face_velocity").is_some());
            let faces = hdf.mesh_faces(&MeshOptions::default()).unwrap();
            let joined = if has_fv {
                check.expect("maximum_face_velocity", hdf.mesh_faces(&MeshOptions::default().with_summary("Maximum Face Velocity")))
            } else {
                None
            };
            let mut offset = 0;
            for name in &order {
                let mesh = &meshes[name];
                let n = mesh["faces"].as_u64().unwrap() as usize;
                let rows = offset..offset + n;
                let digest = lines_digest(faces.layer.features[rows.clone()].iter().map(|f| match &f.geometry {
                    Some(Geometry::LineString(l)) => l.iter().map(|c| (c.x, c.y)).collect(),
                    other => panic!("{other:?}"),
                }));
                assert_eq!(digest, mesh["faces_sha256"].as_str().unwrap(), "{file} {name} faces");
                if let Some(joined) = &joined {
                    let w = &mesh["maximum_face_velocity"];
                    assert_eq!(
                        float_column(&joined.layer, rows.clone(), "maximum_face_velocity"),
                        w["value_sha256"].as_str().unwrap(),
                        "{file} {name} FV"
                    );
                    assert_eq!(
                        float_column(&joined.layer, rows.clone(), "maximum_face_velocity_time"),
                        w["time_sha256"].as_str().unwrap(),
                        "{file} {name} FV time"
                    );
                }
                faces_checked += n;
                offset += n;
            }
            assert_eq!(offset, faces.layer.len());

            // Perimeters (closed again for the oracle's ring digest).
            let perimeters = hdf.mesh_perimeters(&MeshOptions::default()).unwrap();
            for (name, f) in order.iter().zip(&perimeters.layer.features) {
                let Some(Geometry::Polygon { exterior, .. }) = &f.geometry else { panic!() };
                let mut ring: Vec<_> = exterior.coords().iter().map(|c| (c.x, c.y)).collect();
                ring.push(ring[0]);
                assert_eq!(
                    lines_digest(std::iter::once(ring)),
                    meshes[name]["perimeter_sha256"].as_str().unwrap(),
                    "{file} {name} perimeter"
                );
            }
        }

        // Family identities, for the families whose geometry the file stores.
        for (layer, fam) in want["families"].as_object().unwrap() {
            if !layers.iter().any(|l| l == layer) {
                continue;
            }
            let l = hdf.read_layer(layer).unwrap();
            assert_eq!(l.len() as u64, fam["count"].as_u64().unwrap(), "{file} {layer} count");
            let idx: Vec<usize> = fam["fields"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| {
                    let name = terrio_field(f.as_str().unwrap());
                    l.schema.fields().iter().position(|d| d.name == name).unwrap_or_else(|| panic!("{file} {layer}: no field {name}"))
                })
                .collect();
            let rows: Vec<String> = l
                .features
                .iter()
                .map(|f| {
                    idx.iter()
                        .map(|&i| match &f.attributes[i] {
                            FieldValue::Text(t) => t.trim_end().to_string(),
                            other => panic!("{other:?}"),
                        })
                        .collect::<Vec<_>>()
                        .join("|")
                })
                .collect();
            assert_eq!(sha256_hex(rows.join("\n").as_bytes()), fam["sha256"].as_str().unwrap(), "{file} {layer} identities");
            families_checked += l.len();
        }
        seen.extend(check.refusals_seen);
    }
    for (file, what, _) in KNOWN_REFUSALS {
        if files.contains_key(*file) {
            assert!(seen.iter().any(|(f, w)| f == file && w == what), "{file} {what}: listed refusal not exercised");
        }
    }
    eprintln!("checked {cells_checked} cells, {faces_checked} faces, {families_checked} family features; {} known refusals", seen.len());
}

/// The field name terrio-hecras gives a stored column (lower case, other runs as `_`).
fn terrio_field(stored: &str) -> String {
    let mut out = String::new();
    for c in stored.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}
