//! HEC-RAS geometry (`.g##.hdf`) and plan (`.p##.hdf`) HDF files as terrio vector layers.
//!
//! This crate is a thin adapter: it opens the file with the
//! [`ras-commander-rust`](https://github.com/gpt-cmdr/ras-commander-fast-hdf) engine (the one
//! ras-commander uses) and translates what the engine returns into terrio's [`Layer`],
//! [`terrio::FieldDef`] and [`terrio::Error`] types. It holds no HEC-RAS rule of its own:
//!
//! - **Geometry** is the engine's packed cell polygons, in the engine's ring order (clockwise from
//!   the first listed face). The closing vertex is dropped because terrio rings store none.
//!   Cells the engine omits (ghost cells, cells with fewer than three faces) are reported in
//!   [`MeshCellLayer::omitted`], never invented.
//! - **Identities** are the engine's: `mesh_name` (the exact 2D flow area name) and `cell_id` (the
//!   stored cell index). A cell is identified by both; `cell_id` repeats across meshes.
//! - **CRS** is the stored `Projection` text, attached as WKT. No EPSG code is guessed. A file
//!   without one gives layers without a CRS; conflicting, malformed, unsupported or corrupt
//!   projection evidence is an error ([`HecRasHdf::crs`]).
//! - **Results** are stored values, unconverted: a summary such as `Maximum Water Surface` becomes
//!   a float field, and its stored time row (when the engine proves one) a second field.
//! - **Refusals** (unsupported, corrupt, absent, over budget) are errors (see [`error`]), never
//!   empty layers or partial results.
//!
//! ```no_run
//! # #[cfg(feature = "results")]
//! # fn main() -> Result<(), terrio::Error> {
//! use terrio_hecras::{HecRasHdf, MeshCellOptions};
//!
//! let hdf = HecRasHdf::open("Muncie.p04.hdf")?;
//! let cells = hdf.mesh_cells(&MeshCellOptions::default().with_summary("Maximum Water Surface"))?;
//! println!("{}: {} cells, CRS {:?}", cells.layer.name, cells.layer.len(), cells.layer.crs);
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "results"))]
//! # fn main() {}
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
#[cfg(feature = "mesh")]
mod mesh;

#[cfg(feature = "mesh")]
pub use mesh::{MeshCellLayer, MeshCellOptions, OmittedCell, SummaryField};
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

/// The name of the 2D mesh cell polygon layer.
pub const MESH_CELLS: &str = "mesh_cells";

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
        #[cfg(feature = "mesh")]
        if self.has_meshes()? {
            return Ok(vec![MESH_CELLS.to_string()]);
        }
        Ok(Vec::new())
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
