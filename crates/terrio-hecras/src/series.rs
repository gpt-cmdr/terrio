//! Per-feature results at one stored unsteady output row, joined to the `structures` and
//! `bc_lines` layers by the identities the engine links:
//!
//! - structures: the engine's structure inventories (SA/2D area connections and lateral
//!   structures; 1D inline structures, bridges and culverts) give each stored structure group
//!   its `Geometry/Structures/Attributes` row;
//! - boundary-condition lines: the engine links each 2D area's boundary series to its line by
//!   the exact stored `Name` and `SA-2D`.
//!
//! Each stored column becomes one field. A feature the engine links no series to gets nulls (it
//! stores none); an engine refusal is an error.

use crate::families::Family;
use crate::{HecRasHdf, error, field_name};
use ras_commander_rust::hdf::lines::GeometryFamily;
use ras_commander_rust::hdf::metadata::{MetadataValue, TypedAttribute};
use ras_commander_rust::hdf::structures::{StructureFamilyKind, StructureSeries};
use ras_commander_rust::hdf::structures_1d::OneDStructureKind;
use ras_commander_rust::hdf::xs_geometry::OneDLineFamily;
use ras_commander_rust::hdf::{DEFAULT_MAX_BYTES, DEFAULT_READ_MAX_BYTES, Identity, NumericArray, ResultMode, Slice};
use ras_commander_rust::{Capability, ErrorKind, Reported};
use std::collections::HashMap;
use terrio::{FieldDef, FieldValue, Layer};

const STRUCTURE_ATTRIBUTES: &str = "Geometry/Structures/Attributes";

/// What to join to the `structures` or `bc_lines` layer.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FeatureSeriesOptions {
    /// For `structures`: stored series names relative to each structure group (exact, such as
    /// `Structure Variables`). Ignored by `bc_lines`, whose series are named after each line.
    pub variables: Vec<String>,
    /// The stored unsteady output row (zero-based).
    pub row: u64,
    /// Byte limit for each metadata object the engine consults.
    pub metadata_max_bytes: usize,
    /// Byte limit for each array read.
    pub max_bytes: usize,
}

impl FeatureSeriesOptions {
    /// Reads output row `row`.
    pub fn at_row(row: u64) -> Self {
        Self { variables: Vec::new(), row, metadata_max_bytes: DEFAULT_MAX_BYTES, max_bytes: DEFAULT_READ_MAX_BYTES }
    }

    /// Joins a stored structure series.
    pub fn with_variable(mut self, variable: impl Into<String>) -> Self {
        self.variables.push(variable.into());
        self
    }
}

/// One stored series joined to a layer.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SeriesField {
    /// The stored series name (`Structure Variables`, or the boundary-condition line's name).
    pub variable: String,
    /// The fields it fills, in schema order: one per stored column label.
    pub fields: Vec<String>,
    /// The stored column labels, exactly as stored.
    pub labels: Vec<String>,
    /// The stored unit of each field, exactly as stored; `None` when not stored.
    pub units: Vec<Option<String>>,
    /// The datasets read, one per linked feature.
    pub datasets: Vec<String>,
}

/// A family layer with per-feature values at one output row.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SeriesLayer {
    /// The layer: the family layer, then the joined fields.
    pub layer: Layer,
    /// The joined series.
    pub fields: Vec<SeriesField>,
    /// The stored `Time Date Stamp` of the row, exactly as stored.
    pub row_label: Option<String>,
}

/// A stored column label with its stored unit.
struct Label {
    name: String,
    unit: Option<String>,
}

/// One feature's values of one series.
struct Values {
    id: Identity,
    dataset: String,
    labels: Vec<Label>,
    values: Vec<FieldValue>,
}

impl HecRasHdf {
    /// Reads the `structures` layer and joins each requested structure series at one output row.
    /// Each stored column becomes `<series>_<column label>`.
    pub fn structures_at(&self, options: &FeatureSeriesOptions) -> terrio::Result<SeriesLayer> {
        if options.variables.is_empty() {
            return Err(terrio::Error::InvalidArgument("no structure series to join".into()));
        }
        let mut layer =
            self.family_layer("structures", Family::OneD(OneDLineFamily::StructureCenterlines, crate::families::Shape::Line))?;
        let row_label = self.row_label(options)?;
        let groups = self.structure_groups(options)?;
        let mut fields = Vec::new();
        for variable in &options.variables {
            let mut found = Vec::new();
            for g in &groups {
                if g.id.table != STRUCTURE_ATTRIBUTES {
                    continue;
                }
                if let Some(series) = g.series.iter().find(|s| &s.relative == variable) {
                    let array = g.read(self, series, options)?;
                    found.push(values(g.id.clone(), series, array)?);
                }
            }
            if found.is_empty() {
                return Err(terrio::Error::InvalidArgument(format!("HEC-RAS not_found: no structure stores {variable:?}")));
            }
            fields.push(join(&mut layer, variable, &format!("{}_", field_name(variable)), found)?);
        }
        Ok(SeriesLayer { layer, fields, row_label })
    }

    /// Reads the `bc_lines` layer and joins each line's stored boundary series at one output
    /// row: one field per stored column (`stage`, `flow`, ...), linked by the engine through the
    /// exact `Name` and `SA-2D`. The per-face series are not line values and are not joined.
    pub fn bc_lines_at(&self, options: &FeatureSeriesOptions) -> terrio::Result<SeriesLayer> {
        let mut layer = self.family_layer("bc_lines", Family::TwoD(GeometryFamily::BoundaryConditions, crate::families::Shape::Line))?;
        let row_label = self.row_label(options)?;
        let s = &self.session;
        let mut found = Vec::new();
        for area in self.mesh_names()? {
            let links = s.mesh_boundary_links(&area, options.metadata_max_bytes).map_err(error::from_engine)?;
            let links = reported(links, &format!("{area} boundary-condition links"))?;
            error::require(&links.geometry_capability, &format!("{area} boundary-condition lines"))?;
            error::require(&links.series_capability, &format!("{area} boundary-condition series"))?;
            let inventory = s.mesh_boundary_inventory(&area, options.metadata_max_bytes).map_err(error::from_engine)?;
            for m in &links.matched {
                let v =
                    inventory.variables.iter().find(|v| v.name == m.name).ok_or_else(|| {
                        terrio::Error::InvalidData(format!("{area} boundary series {:?} linked but not inventoried", m.name))
                    })?;
                error::require(&v.capability, &m.series_path)?;
                let labels = column_labels(&v.attributes, &m.series_path)?;
                let sel = s
                    .read_mesh_boundary_series(&area, &m.name, row_slice(options.row), None, options.metadata_max_bytes, options.max_bytes)
                    .map_err(error::from_engine)?;
                let values = row_values(sel.values, labels.len(), &m.series_path)?;
                found.push(Values { id: m.geometry.clone(), dataset: m.series_path.clone(), labels, values });
            }
        }
        if found.is_empty() {
            return Err(terrio::Error::InvalidArgument("HEC-RAS not_found: no boundary-condition line has a stored series".into()));
        }
        let fields = vec![join(&mut layer, "Boundary Conditions", "", found)?];
        Ok(SeriesLayer { layer, fields, row_label })
    }

    fn row_label(&self, options: &FeatureSeriesOptions) -> terrio::Result<Option<String>> {
        let t = self.session.unsteady_row_times(row_slice(options.row), options.metadata_max_bytes).map_err(error::from_engine)?;
        Ok(t.raw_strings.and_then(|d| d.into_iter().next()))
    }

    /// Every stored unsteady structure group the engine inventories, from every qualified
    /// family. An absent family is skipped; a refused one is an error.
    fn structure_groups(&self, options: &FeatureSeriesOptions) -> terrio::Result<Vec<Group>> {
        let s = &self.session;
        let mut out = Vec::new();
        for kind in [StructureFamilyKind::Sa2dAreaConnection, StructureFamilyKind::LateralStructure] {
            match s.structure_inventory(kind, options.metadata_max_bytes) {
                Ok(inv) => out.extend(inv.structures.into_iter().map(|d| Group {
                    source: Source::TwoD(kind),
                    name: d.name,
                    id: d.identity.id,
                    series: d.series,
                })),
                Err(e) if e.kind == ErrorKind::NotFound => {}
                Err(e) => return Err(error::from_engine(e)),
            }
        }
        let one_d = [OneDStructureKind::InlineStructure, OneDStructureKind::Bridge, OneDStructureKind::Culvert];
        for c in s.one_d_structure_capabilities(options.metadata_max_bytes).map_err(error::from_engine)? {
            if c.mode != ResultMode::Unsteady || !one_d.contains(&c.kind) || matches!(c.status, Capability::Unavailable { .. }) {
                continue;
            }
            error::require(&c.status, &c.group)?;
            let inv = s.one_d_structure_inventory(c.kind, ResultMode::Unsteady, options.metadata_max_bytes).map_err(error::from_engine)?;
            let kind = c.kind;
            out.extend(inv.structures.into_iter().map(|d| Group {
                source: Source::OneD(kind),
                name: d.name,
                id: d.identity.id,
                series: d.series,
            }));
        }
        Ok(out)
    }
}

#[derive(Clone, Copy)]
enum Source {
    TwoD(StructureFamilyKind),
    OneD(OneDStructureKind),
}

/// One stored structure group with its geometry identity.
struct Group {
    source: Source,
    name: String,
    id: Identity,
    series: Vec<StructureSeries>,
}

impl Group {
    fn read(&self, hdf: &HecRasHdf, series: &StructureSeries, options: &FeatureSeriesOptions) -> terrio::Result<NumericArray> {
        error::require(&series.capability, &series.path)?;
        let columns = match series.shape.as_slice() {
            [_, c] => *c,
            shape => return Err(terrio::Error::Unsupported(format!("{}: shape {shape:?}, not (rows, columns)", series.path))),
        };
        let s = &hdf.session;
        match self.source {
            Source::TwoD(kind) => s.read_structure_series(
                kind,
                Some(&self.name),
                &series.relative,
                row_slice(options.row),
                Slice::new(0, columns, 1),
                options.max_bytes,
            ),
            Source::OneD(kind) => s.read_one_d_structure_series(
                kind,
                ResultMode::Unsteady,
                Some(&self.name),
                &series.relative,
                row_slice(options.row),
                None,
                options.metadata_max_bytes,
                options.max_bytes,
            ),
        }
        .map_err(error::from_engine)
    }
}

fn row_slice(row: u64) -> Slice {
    Slice::new(row, row.saturating_add(1), 1)
}

fn reported<T: Clone>(r: Reported<T>, what: &str) -> terrio::Result<T> {
    match r.value() {
        Some(v) if r.is_supported() => Ok(v.clone()),
        _ => {
            error::require(r.capability(), what)?;
            Err(terrio::Error::InvalidData(format!("HEC-RAS {what}: supported without a value")))
        }
    }
}

/// One structure's series values with the engine's column labels.
fn values(id: Identity, series: &StructureSeries, array: NumericArray) -> terrio::Result<Values> {
    let labels: Vec<Label> = reported(series.columns.clone(), &format!("{} column labels", series.path))?
        .labels
        .into_iter()
        .map(|l| Label { name: l.name, unit: l.unit })
        .collect();
    let values = row_values(array, labels.len(), &series.path)?;
    Ok(Values { id, dataset: series.path.clone(), labels, values })
}

/// The stored `Columns` names of a boundary series, each with the unit stored in the attribute
/// of the same name.
fn column_labels(attributes: &[TypedAttribute], what: &str) -> terrio::Result<Vec<Label>> {
    let text = |name: &str| {
        attributes.iter().find(|a| a.name == name).and_then(|a| match &a.value {
            MetadataValue::Strings(v) => Some(v.clone()),
            _ => None,
        })
    };
    let names = text("Columns").ok_or_else(|| terrio::Error::Unsupported(format!("{what}: no stored Columns labels")))?;
    Ok(names
        .into_iter()
        .map(|name| {
            let unit = text(&name).filter(|u| u.len() == 1).map(|mut u| u.remove(0));
            Label { name, unit }
        })
        .collect())
}

/// The single row of a `(1, columns)` read.
fn row_values(array: NumericArray, columns: usize, what: &str) -> terrio::Result<Vec<FieldValue>> {
    if array.shape.as_slice() != [1, columns as u64] {
        return Err(terrio::Error::InvalidData(format!("{what}: read shape {:?} for {columns} labelled columns", array.shape)));
    }
    crate::numbers(array.values, what)
}

/// Adds one field per distinct column label (in first-seen order, `prefix` + label) and fills
/// each linked feature; unlinked features get nulls.
fn join(layer: &mut Layer, variable: &str, prefix: &str, found: Vec<Values>) -> terrio::Result<SeriesField> {
    let mut index = HashMap::with_capacity(layer.len());
    for (i, f) in layer.features.iter().enumerate() {
        if let Some(FieldValue::Integer(r)) = f.attributes.first() {
            index.insert(*r, i);
        }
    }
    let mut report =
        SeriesField { variable: variable.to_string(), fields: Vec::new(), labels: Vec::new(), units: Vec::new(), datasets: Vec::new() };
    let mut slot: HashMap<String, usize> = HashMap::new();
    let mut table: Vec<Vec<FieldValue>> = Vec::new();
    let mut seen = vec![false; layer.len()];
    for v in found {
        let row = i64::try_from(v.id.row).map_err(|_| terrio::Error::InvalidData("row overflow".into()))?;
        let feature = match index.get(&row) {
            Some(&f) if !seen[f] => f,
            _ => {
                return Err(terrio::Error::InvalidData(format!(
                    "{}: {} row {} matches no single feature",
                    v.dataset, v.id.table, v.id.row
                )));
            }
        };
        seen[feature] = true;
        for (label, value) in v.labels.iter().zip(v.values) {
            let name = format!("{prefix}{}", field_name(&label.name));
            let k = match slot.get(&name) {
                Some(&k) => {
                    if report.units[k] != label.unit {
                        return Err(terrio::Error::Unsupported(format!(
                            "{variable}: column {:?} is stored with different units",
                            label.name
                        )));
                    }
                    k
                }
                None => {
                    if name.is_empty() || name == prefix || layer.schema.fields().iter().any(|f| f.name == name) {
                        return Err(terrio::Error::Unsupported(format!(
                            "{variable}: column {:?} gives an empty or repeated field",
                            label.name
                        )));
                    }
                    slot.insert(name.clone(), report.fields.len());
                    report.fields.push(name);
                    report.labels.push(label.name.clone());
                    report.units.push(label.unit.clone());
                    table.push(vec![FieldValue::Null; layer.len()]);
                    report.fields.len() - 1
                }
            };
            if table[k][feature] != FieldValue::Null {
                return Err(terrio::Error::Unsupported(format!("{}: column {:?} stored twice", v.dataset, label.name)));
            }
            table[k][feature] = value;
        }
        report.datasets.push(v.dataset);
    }
    for (name, column) in report.fields.iter().zip(table) {
        let kind = crate::kind_of(&column.iter().filter(|v| **v != FieldValue::Null).cloned().collect::<Vec<_>>());
        layer.add_field(FieldDef::new(name.clone(), kind));
        for (feature, v) in layer.features.iter_mut().zip(column) {
            feature.attributes.push(v);
        }
    }
    Ok(report)
}
