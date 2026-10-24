//! The 2D mesh cell polygon layer, with optional stored summary results joined by cell.

use crate::{HecRasHdf, MESH_CELLS, error};
use ras_commander_rust::hdf::{DEFAULT_MAX_BYTES, DEFAULT_READ_MAX_BYTES};
use ras_commander_rust::mesh::{GeometryPolicy, OmissionKind, PackedMesh};
use terrio::vector::{Coord, Ring};
use terrio::{Feature, FieldDef, FieldType, FieldValue, Geometry, GeometryType, Layer};

/// What to read into the mesh cell layer.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct MeshCellOptions {
    /// Only these 2D flow areas (exact names), in this order. `None` reads every mesh.
    pub meshes: Option<Vec<String>>,
    /// The engine's geometry policy. [`GeometryPolicy::Strict`] (the default) fails on an invalid
    /// cell polygon; [`GeometryPolicy::QuarantineInvalid`] leaves it out and reports it in
    /// [`MeshCellLayer::omitted`]. Structural corruption fails under both.
    pub policy: GeometryPolicy,
    /// Stored `Summary Output` variables (exact dataset names, such as `Maximum Water Surface`)
    /// to join to each cell. Needs a plan HDF with results and the `results` feature.
    pub summaries: Vec<String>,
    /// Byte limit for each metadata object the engine consults.
    pub metadata_max_bytes: usize,
    /// Byte limit for each array read (one mesh topology, one summary array).
    pub max_bytes: usize,
}

impl Default for MeshCellOptions {
    fn default() -> Self {
        Self {
            meshes: None,
            policy: GeometryPolicy::Strict,
            summaries: Vec::new(),
            metadata_max_bytes: DEFAULT_MAX_BYTES,
            max_bytes: DEFAULT_READ_MAX_BYTES,
        }
    }
}

impl MeshCellOptions {
    /// Reads only these meshes.
    pub fn with_meshes<S: Into<String>>(mut self, meshes: impl IntoIterator<Item = S>) -> Self {
        self.meshes = Some(meshes.into_iter().map(Into::into).collect());
        self
    }

    /// Joins a stored summary variable to the cells.
    pub fn with_summary(mut self, variable: impl Into<String>) -> Self {
        self.summaries.push(variable.into());
        self
    }

    /// Sets the geometry policy.
    pub fn with_policy(mut self, policy: GeometryPolicy) -> Self {
        self.policy = policy;
        self
    }
}

/// A cell the engine did not turn into a polygon.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct OmittedCell {
    /// The 2D flow area.
    pub mesh_name: String,
    /// The stored cell index.
    pub cell_id: i64,
    /// Why: fewer than three faces (ghost and boundary cells), none, or invalid geometry
    /// (quarantine policy only).
    pub kind: OmissionKind,
    /// The engine's reason text (display only).
    pub reason: String,
}

/// A summary variable joined to the cells.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SummaryField {
    /// The stored variable name.
    pub variable: String,
    /// The field holding its value row.
    pub field: String,
    /// The field holding its stored time row, when the engine proves the second row is `Time`.
    /// Values are the stored numbers (days in HEC-RAS files); no timestamp is derived.
    pub time_field: Option<String>,
    /// The units the engine reports for the dataset, exactly as stored; `None` when it reports
    /// none it can prove. Never inferred.
    pub units: Option<String>,
    /// The dataset paths read, one per mesh.
    pub datasets: Vec<String>,
}

/// The mesh cell polygon layer and what the engine reported while building it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct MeshCellLayer {
    /// The `mesh_cells` layer: one polygon per cell, with `mesh_name`, `cell_id` and the summary
    /// fields, in the stored CRS.
    pub layer: Layer,
    /// Cells left out, by mesh.
    pub omitted: Vec<OmittedCell>,
    /// The joined summary variables.
    pub summaries: Vec<SummaryField>,
}

impl HecRasHdf {
    /// Reads the 2D mesh cell polygons as the [`MESH_CELLS`] layer.
    ///
    /// Every requested mesh must read completely: one corrupt or unsupported mesh fails the call.
    pub fn mesh_cells(&self, options: &MeshCellOptions) -> terrio::Result<MeshCellLayer> {
        let meshes = match &options.meshes {
            Some(m) => m.clone(),
            None => self.mesh_names()?,
        };
        if meshes.is_empty() {
            return Err(terrio::Error::InvalidArgument(format!("{}: no 2D flow areas to read", self.path.display())));
        }
        #[cfg(not(feature = "results"))]
        if !options.summaries.is_empty() {
            return Err(terrio::Error::Unsupported("summary results need terrio-hecras's `results` feature".into()));
        }

        let mut layer = Layer::new(MESH_CELLS).with_geom_type(GeometryType::Polygon);
        if let Some(wkt) = self.crs()? {
            layer = layer.with_crs_wkt(wkt);
        }
        let mut id = FieldDef::new("mesh_name", FieldType::Text);
        id.nullable = false;
        layer.add_field(id);
        let mut id = FieldDef::new("cell_id", FieldType::Integer);
        id.nullable = false;
        layer.add_field(id);

        #[cfg_attr(not(feature = "results"), allow(unused_mut))]
        let mut summaries: Vec<SummaryField> = Vec::new();
        let mut omitted = Vec::new();
        for mesh in &meshes {
            let named = self.session.read_topology_bounded(mesh, options.max_bytes).map_err(error::from_engine)?;
            let packed = named.topology.assemble_with_session(options.policy, &self.session).map_err(error::from_engine)?;
            omitted.extend(packed.diagnostics.iter().map(|d| OmittedCell {
                mesh_name: mesh.clone(),
                cell_id: d.cell_id,
                kind: d.kind,
                reason: d.reason.clone(),
            }));

            #[cfg(feature = "results")]
            let columns = {
                let mut columns = Vec::new();
                for (i, variable) in options.summaries.iter().enumerate() {
                    let column = results::read_summary(self, mesh, variable, options)?;
                    if summaries.len() == i {
                        summaries.push(results::declare(&mut layer, variable, &column)?);
                    }
                    results::merge(&mut summaries[i], &column)?;
                    columns.push(column);
                }
                columns
            };

            for (row, &cell_id) in packed.cell_ids.iter().enumerate() {
                let mut feature = Feature::new();
                feature.fid = layer.features.len() as u64;
                feature.geometry = Some(polygon(&packed, row, mesh, cell_id)?);
                feature.attributes = vec![FieldValue::Text(mesh.clone()), FieldValue::Integer(cell_id)];
                #[cfg(feature = "results")]
                for column in &columns {
                    column.push_values(cell_id, mesh, &mut feature.attributes)?;
                }
                layer.push(feature);
            }
        }
        Ok(MeshCellLayer { layer, omitted, summaries })
    }
}

/// The polygon of packed row `row`. The engine's rings are closed (last vertex = first); terrio
/// rings are stored without the closing vertex.
fn polygon(packed: &PackedMesh, row: usize, mesh: &str, cell_id: i64) -> terrio::Result<Geometry> {
    let bad = |what: &str| terrio::Error::InvalidData(format!("HEC-RAS engine ring for {mesh} cell {cell_id}: {what}"));
    let (start, end) = match (packed.offsets.get(row), packed.offsets.get(row + 1)) {
        (Some(&s), Some(&e)) => (usize::try_from(s).map_err(|_| bad("offset"))?, usize::try_from(e).map_err(|_| bad("offset"))?),
        _ => return Err(bad("missing offset")),
    };
    let ring = packed.coordinates.get(start..end).ok_or_else(|| bad("offset out of range"))?;
    match ring {
        [first, .., last] if ring.len() >= 4 && first == last => {
            let coords = ring[..ring.len() - 1].iter().map(|p| Coord::xy(p[0], p[1])).collect();
            Ok(Geometry::Polygon { exterior: Ring::new(coords), interiors: Vec::new() })
        }
        _ => Err(bad("not a closed ring of at least three vertices")),
    }
}

#[cfg(feature = "results")]
mod results {
    use super::*;
    use ras_commander_rust::hdf::NumericValues;
    use ras_commander_rust::hdf::mesh_results::ElementDomain;

    /// One summary variable of one mesh, by stored cell index.
    pub(super) struct SummaryColumn {
        pub dataset: String,
        pub units: Option<String>,
        pub cells: usize,
        pub value: Vec<f64>,
        pub time: Option<Vec<f64>>,
    }

    impl SummaryColumn {
        pub fn push_values(&self, cell_id: i64, mesh: &str, out: &mut Vec<FieldValue>) -> terrio::Result<()> {
            let i = usize::try_from(cell_id).ok().filter(|&i| i < self.cells).ok_or_else(|| {
                terrio::Error::InvalidData(format!("{mesh} cell {cell_id} is outside {} ({} cells)", self.dataset, self.cells))
            })?;
            out.push(FieldValue::Float(self.value[i]));
            if let Some(time) = &self.time {
                out.push(FieldValue::Float(time[i]));
            }
            Ok(())
        }
    }

    pub(super) fn read_summary(hdf: &HecRasHdf, mesh: &str, variable: &str, options: &MeshCellOptions) -> terrio::Result<SummaryColumn> {
        let s = &hdf.session;
        let inventory = s.mesh_summary_inventory(mesh, options.metadata_max_bytes).map_err(error::from_engine)?;
        error::require(&inventory.capability, &format!("{mesh} Summary Output"))?;
        let v = inventory
            .variables
            .iter()
            .find(|v| v.name == variable)
            .ok_or_else(|| terrio::Error::InvalidArgument(format!("HEC-RAS not_found: {mesh} Summary Output has no {variable:?}")))?;
        error::require(&v.capability, &format!("{mesh} {variable}"))?;
        if v.domain != ElementDomain::Cell {
            return Err(terrio::Error::Unsupported(format!("{mesh} {variable}: domain {}, not cells", v.domain.as_str())));
        }
        let array =
            s.read_mesh_summary_values(mesh, variable, None, options.metadata_max_bytes, options.max_bytes).map_err(error::from_engine)?;
        let (rows, cells) = match array.shape.as_slice() {
            [n] => (1, *n),
            [r, n] => (*r, *n),
            _ => return Err(terrio::Error::Unsupported(format!("{mesh} {variable}: shape {:?}", array.shape))),
        };
        let cells = usize::try_from(cells).map_err(|_| terrio::Error::Unsupported(format!("{mesh} {variable}: too many cells")))?;
        // f32 widens to f64 exactly. Integer counters are left for a later, explicit mapping.
        let values: Vec<f64> = match array.values {
            NumericValues::F32(v) => v.into_iter().map(f64::from).collect(),
            NumericValues::F64(v) => v,
            _ => return Err(terrio::Error::Unsupported(format!("{mesh} {variable}: non-float summary arrays are not mapped yet"))),
        };
        let (value, time) = match (rows, v.time_of_extreme_row) {
            (1, _) => (values, None),
            (2, Some(1)) => {
                let (a, b) = values.split_at(cells);
                (a.to_vec(), Some(b.to_vec()))
            }
            _ => {
                return Err(terrio::Error::Unsupported(format!(
                    "{mesh} {variable}: {rows} stored rows; only a value row, or a value row and a proven Time row, map to fields"
                )));
            }
        };
        let units = s.dataset_units(&v.path).map_err(error::from_engine)?.units.value().cloned();
        Ok(SummaryColumn { dataset: v.path.clone(), units, cells, value, time })
    }

    /// Adds the fields of the first mesh's column to the layer.
    pub(super) fn declare(layer: &mut Layer, variable: &str, column: &SummaryColumn) -> terrio::Result<SummaryField> {
        let field = field_name(variable);
        if field.is_empty() {
            return Err(terrio::Error::InvalidArgument(format!("summary {variable:?} gives no field name")));
        }
        let time_field = column.time.as_ref().map(|_| format!("{field}_time"));
        for name in std::iter::once(&field).chain(time_field.as_ref()) {
            if layer.schema.fields().iter().any(|f| &f.name == name) {
                return Err(terrio::Error::InvalidArgument(format!("summary {variable:?} gives the field {name:?} twice")));
            }
            layer.add_field(FieldDef::new(name.clone(), FieldType::Float));
        }
        Ok(SummaryField { variable: variable.to_string(), field, time_field, units: column.units.clone(), datasets: Vec::new() })
    }

    /// Records a further mesh's column; its layout and units must match the first mesh's.
    pub(super) fn merge(field: &mut SummaryField, column: &SummaryColumn) -> terrio::Result<()> {
        if field.time_field.is_some() != column.time.is_some() || (!field.datasets.is_empty() && field.units != column.units) {
            return Err(terrio::Error::Unsupported(format!(
                "{}: meshes store it with different rows or units; it cannot be one field",
                field.variable
            )));
        }
        field.datasets.push(column.dataset.clone());
        Ok(())
    }

    /// A field name for a stored variable name: lower case, runs of other characters as `_`.
    pub(super) fn field_name(variable: &str) -> String {
        let mut out = String::new();
        for c in variable.chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c.to_ascii_lowercase());
            } else if !out.ends_with('_') {
                out.push('_');
            }
        }
        out.trim_matches('_').to_string()
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn field_names() {
            assert_eq!(super::field_name("Maximum Water Surface"), "maximum_water_surface");
            assert_eq!(super::field_name("Maximum Face Velocity (ft/s)"), "maximum_face_velocity_ft_s");
        }
    }
}
