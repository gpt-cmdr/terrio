//! HEC-RAS geometry (`.g##.hdf`) and plan (`.p##.hdf`) HDF files as terrio vector layers.
//!
//! This crate is a thin adapter: it opens the file with the
//! [`ras-commander-rust`](https://github.com/gpt-cmdr/ras-commander-fast-hdf) engine (the one
//! ras-commander uses) and translates what the engine returns into terrio's [`Layer`],
//! [`terrio::FieldDef`] and [`terrio::Error`] types. It holds no HEC-RAS rule of its own:
//!
//! - **Geometry** is the engine's: packed cell polygons in its ring order (clockwise from the
//!   first listed face), face polylines, stored perimeters and family parts. Closing vertices are
//!   dropped because terrio rings store none. Cells the engine omits (ghost cells, cells with
//!   fewer than three faces) are reported in [`MeshLayer::omitted`], never invented.
//! - **Identities** are the engine's: `mesh_name` (the exact 2D flow area name) with `cell_id` or
//!   `face_id` (stored indices that repeat across meshes), and `source_row` (the stored
//!   attribute-table row) with the stored attribute columns for the other families.
//! - **CRS** is the stored `Projection` text, attached as WKT. No EPSG code is guessed. A file
//!   without one gives layers without a CRS; conflicting, malformed, unsupported or corrupt
//!   projection evidence is an error ([`HecRasHdf::crs`]).
//! - **Results** are stored values, unconverted: a summary such as `Maximum Water Surface` becomes
//!   a float field, and its stored time row (when the engine proves one) a second field. Static
//!   mesh arrays, time-step values and the engine's series extremes join to mesh layers
//!   ([`MeshOptions`]); 1D summaries and results join to `cross_sections` by the engine's
//!   validated cross-section identity ([`HecRasHdf::cross_sections`]).
//! - **Refusals** (unsupported, corrupt, absent, over budget) are errors (see [`error`]), never
//!   empty layers or partial results.
//!
//! ```no_run
//! # #[cfg(feature = "results")]
//! # fn main() -> Result<(), terrio::Error> {
//! use terrio_hecras::{HecRasHdf, MeshOptions};
//!
//! let hdf = HecRasHdf::open("Muncie.p04.hdf")?;
//! println!("layers: {:?}", hdf.layer_names()?);
//! let cells = hdf.mesh_cells(&MeshOptions::default().with_summary("Maximum Water Surface"))?;
//! println!("{}: {} cells, CRS {:?}", cells.layer.name, cells.layer.len(), cells.layer.crs);
//! let sections = hdf.read_layer("cross_sections")?;
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "results"))]
//! # fn main() {}
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
mod families;
#[cfg(feature = "mesh")]
mod mesh;
#[cfg(feature = "results")]
mod one_d;

#[cfg(feature = "mesh")]
pub use mesh::{
    Extreme, JoinedField, MeshAttribute, MeshCellLayer, MeshCellOptions, MeshLayer, MeshOptions, OmittedCell, SummaryField, TimeStep,
};
#[cfg(feature = "results")]
pub use one_d::{CrossSectionField, CrossSectionLayer, CrossSectionOptions};
#[cfg(feature = "results")]
pub use ras_commander_rust::hdf::ResultMode;
#[cfg(feature = "mesh")]
pub use ras_commander_rust::mesh::{GeometryPolicy, OmissionKind};
pub use terrio::Layer;

use ras_commander_rust::crs::EvidenceStatus;
use ras_commander_rust::hdf::ReadOnlyHdfSession;
use std::path::{Path, PathBuf};

/// The engine contract this crate is written against: API 10 fixes the ring order of mesh cell
/// polygons (clockwise from the first listed face), identical to ras-commander's Python path.
pub const REQUIRED_ENGINE_API: u32 = 10;

/// The engine's `API_VERSION`.
pub fn engine_api_version() -> u32 {
    ras_commander_rust::api::API_VERSION
}

/// The 2D mesh cell polygon layer ([`HecRasHdf::mesh_cells`]).
pub const MESH_CELLS: &str = "mesh_cells";
/// The 2D mesh cell centre point layer ([`HecRasHdf::mesh_cell_points`]).
pub const MESH_CELL_POINTS: &str = "mesh_cell_points";
/// The 2D mesh face line layer ([`HecRasHdf::mesh_faces`]).
pub const MESH_FACES: &str = "mesh_faces";
/// The 2D flow area perimeter polygon layer ([`HecRasHdf::mesh_perimeters`]).
pub const MESH_PERIMETERS: &str = "mesh_perimeters";
/// The mesh layers, in listing order.
pub const MESH_LAYERS: &[&str] = &[MESH_CELLS, MESH_CELL_POINTS, MESH_FACES, MESH_PERIMETERS];

/// The names of the geometry family layers ([`HecRasHdf::read_layer`]): `cross_sections`,
/// `river_centerlines`, `river_bank_lines`, `river_edge_lines`, `river_flow_paths`,
/// `storage_areas`, `structures`, `bc_lines`, `breaklines`, `refinement_regions`,
/// `reference_lines` and `reference_points`.
pub fn family_layer_names() -> impl Iterator<Item = &'static str> {
    families::FAMILY_LAYERS.iter().map(|(name, _)| *name)
}

/// A non-nullable identity field.
pub(crate) fn id_field(name: &str, kind: terrio::FieldType) -> terrio::FieldDef {
    let mut f = terrio::FieldDef::new(name, kind);
    f.nullable = false;
    f
}

/// A field name for a stored name: lower case, runs of other characters as `_`.
pub(crate) fn field_name(stored: &str) -> String {
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

/// Stored numbers as field values: floats (`f32` widened exactly) or integers.
#[cfg(feature = "mesh")]
pub(crate) fn numbers(values: ras_commander_rust::hdf::NumericValues, what: &str) -> terrio::Result<Vec<terrio::FieldValue>> {
    use ras_commander_rust::hdf::NumericValues as N;
    use terrio::FieldValue as V;
    let int = V::Integer;
    Ok(match values {
        N::F32(v) => v.into_iter().map(|x| V::Float(f64::from(x))).collect(),
        N::F64(v) => v.into_iter().map(V::Float).collect(),
        N::I8(v) => v.into_iter().map(|x| int(x.into())).collect(),
        N::I16(v) => v.into_iter().map(|x| int(x.into())).collect(),
        N::I32(v) => v.into_iter().map(|x| int(x.into())).collect(),
        N::I64(v) => v.into_iter().map(int).collect(),
        N::U8(v) => v.into_iter().map(|x| int(x.into())).collect(),
        N::U16(v) => v.into_iter().map(|x| int(x.into())).collect(),
        N::U32(v) => v.into_iter().map(|x| int(x.into())).collect(),
        N::U64(v) => v
            .into_iter()
            .map(|x| i64::try_from(x).map(int).map_err(|_| terrio::Error::Unsupported(format!("{what}: value above i64"))))
            .collect::<terrio::Result<_>>()?,
        _ => return Err(terrio::Error::Unsupported(format!("{what}: unmapped numeric kind"))),
    })
}

/// The field type of values from [`numbers`] (all share one).
#[cfg(feature = "mesh")]
pub(crate) fn kind_of(values: &[terrio::FieldValue]) -> terrio::FieldType {
    match values.first() {
        Some(terrio::FieldValue::Integer(_)) => terrio::FieldType::Integer,
        _ => terrio::FieldType::Float,
    }
}

/// A HEC-RAS geometry or plan HDF file, opened read-only.
///
/// The file is memory-mapped by the engine and must not change while this value lives.
pub struct HecRasHdf {
    session: ReadOnlyHdfSession,
    path: PathBuf,
}

impl HecRasHdf {
    /// Opens a geometry (`.g##.hdf`) or plan (`.p##.hdf`) HDF file.
    pub fn open(path: impl AsRef<Path>) -> terrio::Result<Self> {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
        Self::open_with_threads(path, threads)
    }

    /// Opens the file with `threads` engine workers (1 to 1024) for polygon assembly.
    pub fn open_with_threads(path: impl AsRef<Path>, threads: usize) -> terrio::Result<Self> {
        const _: () = assert!(ras_commander_rust::api::API_VERSION >= REQUIRED_ENGINE_API);
        let path = path.as_ref().to_path_buf();
        let session = ReadOnlyHdfSession::with_options(&path, threads, threads > 1).map_err(error::from_engine)?;
        Ok(Self { session, path })
    }

    /// The file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The names of the layers this file can give. Listing does not validate them: reading a
    /// layer can still refuse.
    pub fn layer_names(&self) -> terrio::Result<Vec<String>> {
        let mut names = Vec::new();
        #[cfg(feature = "mesh")]
        if self.has_meshes()? {
            names.extend(MESH_LAYERS.iter().map(|n| n.to_string()));
        }
        for (name, family) in families::FAMILY_LAYERS {
            if self.family_present(*family)? {
                names.push(name.to_string());
            }
        }
        Ok(names)
    }

    /// Reads a layer by name, with default options for mesh layers (every mesh, strict
    /// geometry, no joined summaries). A name this crate does not know, or a layer the file does
    /// not store, is [`terrio::Error::InvalidArgument`].
    pub fn read_layer(&self, name: &str) -> terrio::Result<Layer> {
        #[cfg(feature = "mesh")]
        {
            let options = MeshOptions::default();
            match name {
                MESH_CELLS => return Ok(self.mesh_cells(&options)?.layer),
                MESH_CELL_POINTS => return Ok(self.mesh_cell_points(&options)?.layer),
                MESH_FACES => return Ok(self.mesh_faces(&options)?.layer),
                MESH_PERIMETERS => return Ok(self.mesh_perimeters(&options)?.layer),
                _ => {}
            }
        }
        match families::FAMILY_LAYERS.iter().find(|(n, _)| *n == name) {
            Some((name, family)) => self.family_layer(name, *family),
            None => Err(terrio::Error::InvalidArgument(format!("no HEC-RAS layer named {name:?}"))),
        }
    }

    /// The exact 2D flow area names, in the engine's order. A file without a `2D Flow Areas` group
    /// is an error ([`terrio::Error::InvalidArgument`]), as is a legacy names-only layout
    /// ([`terrio::Error::Unsupported`]).
    pub fn mesh_names(&self) -> terrio::Result<Vec<String>> {
        self.session.mesh_names().map_err(error::from_engine)
    }

    #[cfg(feature = "mesh")]
    fn has_meshes(&self) -> terrio::Result<bool> {
        match self.session.mesh_names() {
            Ok(names) => Ok(!names.is_empty()),
            Err(e) if e.kind == ras_commander_rust::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(error::from_engine(e)),
        }
    }

    /// The stored CRS as a terrio layer CRS (WKT only, no EPSG code), or `None` when the file
    /// stores no projection.
    ///
    /// The engine reads the root `Projection` attribute. Malformed text, several disagreeing
    /// declarations, or an attribute it cannot decode are errors: a layer is never placed with a
    /// CRS the file does not state.
    pub fn crs(&self) -> terrio::Result<Option<String>> {
        let report = self.session.raw_crs_evidence().map_err(error::from_engine)?;
        match &report.status {
            EvidenceStatus::Unknown => Ok(None),
            // Lexically readable; every non-empty declaration has the same exact text.
            EvidenceStatus::Unvalidated => {
                Ok(report.evidence.iter().find(|e| e.status != EvidenceStatus::Unknown).map(|e| e.exact_wkt.clone()))
            }
            status => Err(terrio::Error::Crs(match error::require(&status.capability(), "stored Projection") {
                Err(e) => e.to_string(),
                Ok(()) => "HEC-RAS stored Projection is not usable".to_string(),
            })),
        }
    }

    /// The stored hydraulic unit system text (the root `Units System` attribute, for example
    /// `US Customary`), exactly as stored; `None` when absent. An attribute the engine cannot
    /// decode, or a damaged one, is an error.
    pub fn units_system(&self) -> terrio::Result<Option<String>> {
        let report = self.session.raw_crs_evidence().map_err(error::from_engine)?;
        for evidence in &report.hydraulic_units_evidence {
            error::require(&evidence.capability(), "stored Units System")?;
        }
        Ok(report.hydraulic_units)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn field_names_follow_stored_names() {
        assert_eq!(super::field_name("Maximum Water Surface"), "maximum_water_surface");
        assert_eq!(super::field_name("Maximum Face Velocity (ft/s)"), "maximum_face_velocity_ft_s");
        assert_eq!(super::field_name("SA-2D"), "sa_2d");
        assert_eq!(super::field_name("River Name"), "river_name");
    }
}
