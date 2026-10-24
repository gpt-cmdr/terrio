//! 2D mesh layers: cell polygons, cell centre points, face lines and area perimeters, with
//! optional stored summary results joined by cell or face.

use crate::{HecRasHdf, MESH_CELL_POINTS, MESH_CELLS, MESH_FACES, MESH_PERIMETERS, error, id_field};
use ras_commander_rust::hdf::mesh_geometry::MeshGeometryField;
use ras_commander_rust::hdf::{DEFAULT_MAX_BYTES, DEFAULT_READ_MAX_BYTES, NumericValues};
use ras_commander_rust::mesh::{GeometryPolicy, OmissionKind};
use terrio::vector::{Coord, Ring};
use terrio::{Feature, FieldDef, FieldType, FieldValue, Geometry, GeometryType, Layer};

/// What to read into a mesh layer.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct MeshOptions {
    /// Only these 2D flow areas (exact names), in this order. `None` reads every mesh.
    pub meshes: Option<Vec<String>>,
    /// The engine's geometry policy for cell polygons. [`GeometryPolicy::Strict`] (the default)
    /// fails on an invalid cell polygon; [`GeometryPolicy::QuarantineInvalid`] leaves it out and
    /// reports it in [`MeshLayer::omitted`]. Structural corruption fails under both.
    pub policy: GeometryPolicy,
    /// Stored `Summary Output` variables (exact dataset names, such as `Maximum Water Surface`
    /// or `Maximum Face Velocity`) to join: cell variables to cell layers, face variables to
    /// the face layer. Needs a plan HDF with results and the `results` feature.
    pub summaries: Vec<String>,
    /// Byte limit for each metadata object the engine consults.
    pub metadata_max_bytes: usize,
    /// Byte limit for each array read (one mesh topology, one summary array).
    pub max_bytes: usize,
}

/// The former name of [`MeshOptions`].
pub type MeshCellOptions = MeshOptions;

impl Default for MeshOptions {
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

impl MeshOptions {
    /// Reads only these meshes.
    pub fn with_meshes<S: Into<String>>(mut self, meshes: impl IntoIterator<Item = S>) -> Self {
        self.meshes = Some(meshes.into_iter().map(Into::into).collect());
        self
    }

    /// Joins a stored summary variable.
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

/// A summary variable joined to a layer.
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

/// A mesh layer and what the engine reported while building it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct MeshLayer {
    /// The layer, in the stored CRS.
    pub layer: Layer,
    /// Cells left out of a cell polygon layer, by mesh (empty for other layers).
    pub omitted: Vec<OmittedCell>,
    /// The joined summary variables.
    pub summaries: Vec<SummaryField>,
}

/// The former name of [`MeshLayer`].
pub type MeshCellLayer = MeshLayer;

/// The element a mesh layer has one feature per.
#[derive(Clone, Copy, PartialEq)]
enum Element {
    Cell,
    Face,
}

impl HecRasHdf {
    fn mesh_list(&self, options: &MeshOptions) -> terrio::Result<Vec<String>> {
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
        Ok(meshes)
    }

    fn new_layer(&self, name: &str, geom: GeometryType, id: Option<&str>) -> terrio::Result<Layer> {
        let mut layer = Layer::new(name).with_geom_type(geom);
        if let Some(wkt) = self.crs()? {
            layer = layer.with_crs_wkt(wkt);
        }
        layer.add_field(id_field("mesh_name", FieldType::Text));
        if let Some(id) = id {
            layer.add_field(id_field(id, FieldType::Integer));
        }
        Ok(layer)
    }

    /// Builds a per-element layer: `rows(mesh)` gives each feature's element ID, geometry and extra
    /// attributes, and the summaries of `element` are joined by element ID.
    fn element_layer(
        &self,
        name: &str,
        geom: GeometryType,
        element: Element,
        extra: &[(&str, FieldType)],
        options: &MeshOptions,
        mut rows: impl FnMut(&str, &mut Vec<OmittedCell>) -> terrio::Result<Vec<(i64, Geometry, Vec<FieldValue>)>>,
    ) -> terrio::Result<MeshLayer> {
        let meshes = self.mesh_list(options)?;
        let mut layer = self.new_layer(name, geom, Some(if element == Element::Cell { "cell_id" } else { "face_id" }))?;
        for (field, kind) in extra {
            layer.add_field(FieldDef::new(*field, *kind));
        }
        #[cfg_attr(not(feature = "results"), allow(unused_mut))]
        let mut summaries: Vec<SummaryField> = Vec::new();
        let mut omitted = Vec::new();
        for mesh in &meshes {
            let features = rows(mesh, &mut omitted)?;
            #[cfg(feature = "results")]
            let columns = {
                let mut columns = Vec::new();
                for (i, variable) in options.summaries.iter().enumerate() {
                    let column = results::read_summary(self, mesh, variable, element, options)?;
                    if summaries.len() == i {
                        summaries.push(results::declare(&mut layer, variable, &column)?);
                    }
                    results::merge(&mut summaries[i], &column)?;
                    columns.push(column);
                }
                columns
            };
            for (id, geometry, extra_values) in features {
                let mut feature = Feature::new();
                feature.fid = layer.features.len() as u64;
                feature.geometry = Some(geometry);
                feature.attributes = vec![FieldValue::Text(mesh.clone()), FieldValue::Integer(id)];
                feature.attributes.extend(extra_values);
                #[cfg(feature = "results")]
                for column in &columns {
                    column.push_values(id, mesh, &mut feature.attributes)?;
                }
                layer.push(feature);
            }
        }
        Ok(MeshLayer { layer, omitted, summaries })
    }

    /// Reads the 2D mesh cell polygons as the [`MESH_CELLS`] layer: `mesh_name`, `cell_id`, then
    /// joined cell summaries.
    ///
    /// Every requested mesh must read completely: one corrupt or unsupported mesh fails the call.
    pub fn mesh_cells(&self, options: &MeshOptions) -> terrio::Result<MeshLayer> {
        self.element_layer(MESH_CELLS, GeometryType::Polygon, Element::Cell, &[], options, |mesh, omitted| {
            let named = self.session.read_topology_bounded(mesh, options.max_bytes).map_err(error::from_engine)?;
            let packed = named.topology.assemble_with_session(options.policy, &self.session).map_err(error::from_engine)?;
            omitted.extend(packed.diagnostics.iter().map(|d| OmittedCell {
                mesh_name: mesh.to_string(),
                cell_id: d.cell_id,
                kind: d.kind,
                reason: d.reason.clone(),
            }));
            let mut out = Vec::with_capacity(packed.cell_ids.len());
            for (row, &cell_id) in packed.cell_ids.iter().enumerate() {
                let ring = slice(&packed.coordinates, &packed.offsets, row, mesh, "cell", cell_id)?;
                out.push((cell_id, polygon(ring, mesh, "cell", cell_id)?, Vec::new()));
            }
            Ok(out)
        })
    }

    /// Reads every stored cell centre as the [`MESH_CELL_POINTS`] layer: `mesh_name`, `cell_id`,
    /// `is_ghost`, then joined cell summaries.
    ///
    /// Ghost cells are the stored rows at and above the physical cell count the engine reads
    /// from `Geometry/2D Flow Areas/Attributes`; files without that table (HEC-RAS 5.0) are
    /// refused rather than guessed.
    pub fn mesh_cell_points(&self, options: &MeshOptions) -> terrio::Result<MeshLayer> {
        let extra = [("is_ghost", FieldType::Boolean)];
        self.element_layer(MESH_CELL_POINTS, GeometryType::Point, Element::Cell, &extra, options, |mesh, _| {
            let geometry = self.session.mesh_geometry(mesh, options.metadata_max_bytes).map_err(error::from_engine)?;
            let physical = geometry.mesh.physical_cells;
            let array = self
                .session
                .read_mesh_geometry_cached(&geometry, MeshGeometryField::CellCenters, None, options.max_bytes)
                .map_err(error::from_engine)?;
            let xy = xy_rows(array.shape.as_slice(), array.values, mesh, "Cells Center Coordinate")?;
            Ok(xy
                .into_iter()
                .enumerate()
                .map(|(i, [x, y])| (i as i64, Geometry::Point(Coord::xy(x, y)), vec![FieldValue::Boolean(i as u64 >= physical)]))
                .collect())
        })
    }

    /// Reads every stored face as the [`MESH_FACES`] layer: `mesh_name`, `face_id`, then joined
    /// face summaries. Lines are the engine's face polylines (start point, stored perimeter
    /// vertices, end point), as ras-commander's `get_mesh_cell_faces` builds them.
    pub fn mesh_faces(&self, options: &MeshOptions) -> terrio::Result<MeshLayer> {
        self.element_layer(MESH_FACES, GeometryType::LineString, Element::Face, &[], options, |mesh, _| {
            let named = self.session.read_topology_bounded(mesh, options.max_bytes).map_err(error::from_engine)?;
            let lines = named.topology.face_lines().map_err(error::from_engine)?;
            let mut out = Vec::with_capacity(lines.face_ids.len());
            for (row, &face_id) in lines.face_ids.iter().enumerate() {
                let line = slice(&lines.coordinates, &lines.offsets, row, mesh, "face", face_id)?;
                out.push((face_id, Geometry::LineString(line.iter().map(|p| Coord::xy(p[0], p[1])).collect()), Vec::new()));
            }
            Ok(out)
        })
    }

    /// Reads each 2D flow area's stored perimeter as the [`MESH_PERIMETERS`] layer: one polygon
    /// per mesh with `mesh_name`. An area without a stored `Perimeter` dataset is an error.
    pub fn mesh_perimeters(&self, options: &MeshOptions) -> terrio::Result<MeshLayer> {
        let meshes = self.mesh_list(options)?;
        if !options.summaries.is_empty() {
            return Err(terrio::Error::InvalidArgument("summaries join to cell and face layers, not perimeters".into()));
        }
        let mut layer = self.new_layer(MESH_PERIMETERS, GeometryType::Polygon, None)?;
        for mesh in &meshes {
            let p = self.session.read_mesh_perimeter(mesh, options.max_bytes).map_err(error::from_engine)?;
            let mut feature = Feature::new();
            feature.fid = layer.features.len() as u64;
            feature.geometry = Some(polygon(&p.ring, mesh, "perimeter", 0)?);
            feature.attributes = vec![FieldValue::Text(mesh.clone())];
            layer.push(feature);
        }
        Ok(MeshLayer { layer, omitted: Vec::new(), summaries: Vec::new() })
    }
}

/// Rows of an `(n, 2)` float array (f32 widened exactly).
fn xy_rows(shape: &[u64], values: NumericValues, mesh: &str, what: &str) -> terrio::Result<Vec<[f64; 2]>> {
    if shape.len() != 2 || shape[1] != 2 {
        return Err(terrio::Error::InvalidData(format!("{mesh} {what}: shape {shape:?}, not (rows, 2)")));
    }
    let v: Vec<f64> = match values {
        NumericValues::F64(v) => v,
        NumericValues::F32(v) => v.into_iter().map(f64::from).collect(),
        _ => return Err(terrio::Error::Unsupported(format!("{mesh} {what}: coordinates are not floats"))),
    };
    Ok(v.as_chunks::<2>().0.to_vec())
}

/// Packed row `row` of `coordinates`/`offsets`.
fn slice<'a>(coordinates: &'a [[f64; 2]], offsets: &[u64], row: usize, mesh: &str, what: &str, id: i64) -> terrio::Result<&'a [[f64; 2]]> {
    let bad = |m: &str| terrio::Error::InvalidData(format!("HEC-RAS engine {what} {id} of {mesh}: {m}"));
    let (start, end) = match (offsets.get(row), offsets.get(row + 1)) {
        (Some(&s), Some(&e)) => (usize::try_from(s).map_err(|_| bad("offset"))?, usize::try_from(e).map_err(|_| bad("offset"))?),
        _ => return Err(bad("missing offset")),
    };
    coordinates.get(start..end).ok_or_else(|| bad("offset out of range"))
}

/// A closed engine ring (last vertex = first) as a terrio polygon, whose rings store no closing
/// vertex.
fn polygon(ring: &[[f64; 2]], mesh: &str, what: &str, id: i64) -> terrio::Result<Geometry> {
    match ring {
        [first, .., last] if ring.len() >= 4 && first == last => {
            let coords = ring[..ring.len() - 1].iter().map(|p| Coord::xy(p[0], p[1])).collect();
            Ok(Geometry::Polygon { exterior: Ring::new(coords), interiors: Vec::new() })
        }
        _ => Err(terrio::Error::InvalidData(format!("HEC-RAS engine {what} {id} of {mesh}: not a closed ring of at least three vertices"))),
    }
}

#[cfg(feature = "results")]
mod results {
    use super::*;
    use crate::field_name;
    use ras_commander_rust::hdf::mesh_results::ElementDomain;

    /// One summary variable of one mesh, by stored element index.
    pub(super) struct SummaryColumn {
        pub dataset: String,
        pub units: Option<String>,
        pub elements: usize,
        pub value: Vec<f64>,
        pub time: Option<Vec<f64>>,
    }

    impl SummaryColumn {
        pub fn push_values(&self, id: i64, mesh: &str, out: &mut Vec<FieldValue>) -> terrio::Result<()> {
            let i = usize::try_from(id).ok().filter(|&i| i < self.elements).ok_or_else(|| {
                terrio::Error::InvalidData(format!("{mesh} element {id} is outside {} ({} elements)", self.dataset, self.elements))
            })?;
            out.push(FieldValue::Float(self.value[i]));
            if let Some(time) = &self.time {
                out.push(FieldValue::Float(time[i]));
            }
            Ok(())
        }
    }

    pub(super) fn read_summary(
        hdf: &HecRasHdf,
        mesh: &str,
        variable: &str,
        element: Element,
        options: &MeshOptions,
    ) -> terrio::Result<SummaryColumn> {
        let s = &hdf.session;
        let inventory = s.mesh_summary_inventory(mesh, options.metadata_max_bytes).map_err(error::from_engine)?;
        error::require(&inventory.capability, &format!("{mesh} Summary Output"))?;
        let v = inventory
            .variables
            .iter()
            .find(|v| v.name == variable)
            .ok_or_else(|| terrio::Error::InvalidArgument(format!("HEC-RAS not_found: {mesh} Summary Output has no {variable:?}")))?;
        error::require(&v.capability, &format!("{mesh} {variable}"))?;
        let (want, label) = match element {
            Element::Cell => (ElementDomain::Cell, "cells"),
            Element::Face => (ElementDomain::Face, "faces"),
        };
        if v.domain != want {
            return Err(terrio::Error::InvalidArgument(format!(
                "{mesh} {variable}: stored on {}, not {label}; join it to the matching layer",
                v.domain.as_str()
            )));
        }
        let array =
            s.read_mesh_summary_values(mesh, variable, None, options.metadata_max_bytes, options.max_bytes).map_err(error::from_engine)?;
        let (rows, elements) = match array.shape.as_slice() {
            [n] => (1, *n),
            [r, n] => (*r, *n),
            _ => return Err(terrio::Error::Unsupported(format!("{mesh} {variable}: shape {:?}", array.shape))),
        };
        let elements =
            usize::try_from(elements).map_err(|_| terrio::Error::Unsupported(format!("{mesh} {variable}: too many elements")))?;
        // f32 widens to f64 exactly. Integer counters are left for a later, explicit mapping.
        let values: Vec<f64> = match array.values {
            NumericValues::F32(v) => v.into_iter().map(f64::from).collect(),
            NumericValues::F64(v) => v,
            _ => return Err(terrio::Error::Unsupported(format!("{mesh} {variable}: non-float summary arrays are not mapped yet"))),
        };
        let (value, time) = match (rows, v.time_of_extreme_row) {
            (1, _) => (values, None),
            (2, Some(1)) => {
                let (a, b) = values.split_at(elements);
                (a.to_vec(), Some(b.to_vec()))
            }
            _ => {
                return Err(terrio::Error::Unsupported(format!(
                    "{mesh} {variable}: {rows} stored rows; only a value row, or a value row and a proven Time row, map to fields"
                )));
            }
        };
        let units = s.dataset_units(&v.path).map_err(error::from_engine)?.units.value().cloned();
        Ok(SummaryColumn { dataset: v.path.clone(), units, elements, value, time })
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
}
