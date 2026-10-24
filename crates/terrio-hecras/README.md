# terrio-hecras

HEC-RAS geometry (`.g##.hdf`) and plan (`.p##.hdf`) HDF files as terrio vector layers.

This crate is a thin adapter between terrio and
[`ras-commander-rust`](https://github.com/gpt-cmdr/ras-commander-fast-hdf), the read-only
HEC-RAS engine that ras-commander uses. The engine owns every HEC-RAS rule: HDF layouts, mesh and
cell identities, polygon ring order, CRS evidence, units and refusals. This crate only translates
the engine's values into terrio's `Layer`, field and error types. It does not read `.prj` or plan
text, run HEC-RAS or write HEC-RAS files.

```rust,no_run
use terrio_hecras::{HecRasHdf, MeshCellOptions};

let hdf = HecRasHdf::open("Muncie.p04.hdf")?;
let cells = hdf.mesh_cells(&MeshCellOptions::default().with_summary("Maximum Water Surface"))?;
terrio::write_vector(&[cells.layer], "muncie_cells.fgb", &Default::default())?;
# Ok::<(), terrio::Error>(())
```

## Layers

`HecRasHdf::layer_names()` lists the layers a file stores; `read_layer(name)` reads one with
default options. Mesh layers also have methods taking `MeshOptions` (meshes, geometry policy,
joined values, byte budgets), and `cross_sections(&CrossSectionOptions)` joins 1D results.

| Layer | Geometry | Fields | Engine source |
|---|---|---|---|
| `mesh_cells` | Polygon | `mesh_name`, `cell_id`, joined cell values | `read_topology` + `assemble` (API 10 ring order) |
| `mesh_cell_points` | Point | `mesh_name`, `cell_id`, `is_ghost`, joined cell values | `mesh_geometry` (physical count) + `CellCenters` |
| `mesh_faces` | LineString | `mesh_name`, `face_id`, joined face values | `Topology::face_lines` |
| `mesh_perimeters` | Polygon | `mesh_name` | `read_mesh_perimeter` |
| `cross_sections` | LineString | `source_row`, stored columns (`river`, `reach`, `rs`, `name`, bank stations, ...), joined 1D results | `one_d_line_family(CrossSectionCutLines)` |
| `river_centerlines`, `river_bank_lines`, `river_edge_lines`, `river_flow_paths` | LineString | `source_row`, stored columns | `one_d_line_family(...)` |
| `storage_areas` | Polygon | `source_row`, stored columns | `one_d_line_family(StorageAreaPolygons)` |
| `structures` | LineString | `source_row`, stored columns (`type`, `river`, `reach`, `rs`, `connection`, ...) | `one_d_line_family(StructureCenterlines)` |
| `bc_lines`, `breaklines`, `reference_lines` | LineString | `source_row`, stored columns (`name`, `sa_2d`, `type`, ...) | `geometry_family(...)` |
| `refinement_regions` | Polygon | `source_row`, stored columns | `geometry_family(RefinementRegions)` |
| `reference_points` | Point | `source_row`, stored columns | `reference_point_geometry` |
| `river_junctions`, `pump_stations` | Point | `source_row`, stored columns (`name`, ...) | `river_junctions`, `pump_stations` |

- Mesh features are identified by `mesh_name` with `cell_id` or `face_id`, the stored indices,
  which repeat across meshes. Other features are identified by `source_row`, the stored
  attribute-table row (or Info row for families without an attribute table), and keep every
  stored attribute column as a field named after it (lower case, other characters as `_`).
- Polygons are the engine's. Cell rings run clockwise from the first listed face (API 10).
  terrio rings omit the closing vertex. Cells the engine does not draw (fewer than three faces,
  such as ghost cells) are listed in `MeshLayer::omitted`. Ghost flags need the physical cell
  count of `Geometry/2D Flow Areas/Attributes`, so HEC-RAS 5.0 files without it refuse
  `mesh_cell_points`. A family line with several parts is a MultiLineString. A polygon family
  feature with several parts is refused, because which parts are holes is not a proven layout.
- The CRS is the stored `Projection` text, attached as WKT. No EPSG code is guessed. A file
  without a projection gives layers without a CRS. Malformed, conflicting, unsupported or
  corrupt projection evidence is an error.
- Summary values are the stored numbers, widened from `f32` to `f64` exactly, named after the
  stored variable (`maximum_water_surface`, plus `maximum_water_surface_time` when the engine
  proves a stored Time row). Time rows are the stored values (days), not timestamps. Cell
  summaries join to cell layers and face summaries to `mesh_faces`; a mismatch is an error.
  `SummaryField::units` gives the units the engine can prove; it is `None` otherwise and is never
  inferred.

### Joined values

Every joined value is a stored number, joined by the engine's element identity; asking for a
value a mesh or file does not store is an error, never an empty field.

| `MeshOptions` | Fields | Stored source |
|---|---|---|
| `with_summary(v)` | `<v>`, `<v>_time` | `Summary Output/2D Flow Areas/<mesh>/<v>` |
| `with_attribute(MinimumElevation)` | `minimum_elevation` | `Cells Minimum Elevation` (cells), `Faces Minimum Elevation` (faces) |
| `with_attribute(ManningsN)` | `mannings_n` | `Cells Center Manning's n` (cells) |
| `with_attribute(FaceNormalAndLength)` | `normal_x`, `normal_y`, `length` | `Faces NormalUnitVector and Length` (faces) |
| `with_attribute(FaceCells)` | `cell_a`, `cell_b` (stored cell indices) | `Faces Cell Indexes` (faces) |
| `with_series(v).at_time_step(row)` | `<v>` at one stored output row; `MeshLayer::time_step` gives its stored `Time` and `Time Date Stamp` | `Unsteady Time Series/2D Flow Areas/<mesh>/<v>` (`Water Surface`, `Face Velocity`, `Depth`, ...) |
| `with_extreme(v, Maximum)` (or `Minimum`) | `<v>_max`, `<v>_max_row` | the engine's reduction of the stored series over every row (first occurrence; NaN propagates) |

| `CrossSectionOptions` | Fields | Stored source |
|---|---|---|
| `with_summary(v)` | `<v>` (row 0), `<v>_time` (a proven Time row), `<v>_<label>` (other labelled rows) | `Summary Output/Cross Sections/<v>` (`Maximum Water Surface`: `Max WSEL`, `Flow at Max WSEL`, `Time at Max WSEL`) |
| `with_unsteady(v, row)` / `with_steady(v, profile)` | `<v>`; `CrossSectionLayer::row_label` gives the stored time stamp or profile name | `Unsteady Time Series/Cross Sections/<v>`, `Steady Profiles/Cross Sections/<v>` |

| `HecRasHdf` method | Fields | Stored source |
|---|---|---|
| `structures_at(FeatureSeriesOptions::at_row(row).with_variable(v))` | `<v>_<column label>` (`structure_variables_total_flow`, ...); nulls on structures that store no `v` | `Unsteady Time Series/{SA 2D Area Conn, Lateral Structures, Inline Structures, Bridges, Culverts}/<structure>/<v>`, linked to `Geometry/Structures/Attributes` rows by the engine |
| `bc_lines_at(FeatureSeriesOptions::at_row(row))` | one field per stored `Columns` label (`stage`, `flow`, ...); nulls on lines without a series | `Unsteady Time Series/2D Flow Areas/<area>/Boundary Conditions/<line name>`, linked by the engine through `Name` and `SA-2D` |

Reference line and reference point results are not joined: the engine reads them, but does not
yet link their columns to the geometry rows, and this crate does not infer that link.

1D values are joined by the cross-section identity the engine validates (River, Reach and RS
against `Geometry/Cross Sections/Attributes`), so a result column always lands on its own cut
line. Units are reported per field (`CrossSectionField::units`) exactly as stored. Depth is not
a stored 2D summary in HEC-RAS 6.x and 7.0, so no depth field is derived here; HEC-RAS 5.0
plans store a `Depth` time series, which `with_series` and `with_extreme` read.

## Errors

Every refusal is an error, never an empty or partial layer. Messages start with the engine's
stable spellings, such as `HEC-RAS unsupported(identity_layout): ...`.

| Engine | terrio |
|---|---|
| `file_not_found` | `Error::Io` (`NotFound`) |
| `io`, `internal` | `Error::Io` |
| `unsupported`, or a budget (`resource_limit`) | `Error::Unsupported` |
| `not_found`, `invalid_request`, an unavailable capability | `Error::InvalidArgument` |
| `corrupt`, `structural_topology`, `invalid_geometry` | `Error::InvalidData` |
| CRS evidence that is not usable | `Error::Crs` |

Some real files are refused today. HEC-RAS 5.0 files such as HEC's `BaldEagleDamBrk.p07.hdf`
store summaries without a `Location`, and the pinned engine refuses them. Their static mesh arrays
are refused too, because the engine reads them through `2D Flow Areas/Attributes`, which 5.0
files lack. Their time series read (`Water Surface`, `Depth`, ...), so `with_extreme` gives a
maximum water surface for them. Engine PR
gpt-cmdr/ras-commander-fast-hdf#9 qualifies their water-surface summaries; their face summaries
stay refused. Through terrio a refusal surfaces as an error, never as an empty layer.

## Features

| Feature | Default | Adds |
|---|---|---|
| `mesh` | yes | The four mesh layers |
| `results` | yes | Summaries, time-step values and series extremes joined to cells and faces, and 1D results joined to `cross_sections` (implies `mesh`) |

With no features, the geometry family layers, `crs` and `units_system` are built.

## Why a separate crate

The engine is not on crates.io. It is pinned by Git revision, which a crates.io package cannot
depend on, even optionally. So this crate is outside terrio's workspace, with its own
`Cargo.lock` and CI job (`.github/workflows/hecras.yml`). terrio's releases, licence notices
and WebAssembly builds are unchanged. The engine memory-maps files, so this crate is
native-only.

## Testing

```sh
cargo test                       # synthetic engine fixtures (tests/fixtures)
cd <models> && python -I <crate>/scripts/oracle_public_models.py . oracle.json *.hdf   # h5py, numpy
TERRIO_HECRAS_TEST_DATA=<models> TERRIO_HECRAS_ORACLE=<models>/oracle.json cargo test --test public_models
```

`<models>` holds HDF files of HEC's public example models (BaldEagleCrkMulti2D, Muncie,
Chippewa_2D, DavisStormSystem, ConSpan, Bald Eagle Creek), as downloaded by ras-commander's
`RasExamples`, with computed plan HDFs. On 29 such files the test checks 258,350 cells,
538,946 faces and 1,205 family features against h5py: geometry, identities, CRS, units,
ghost counts, and Maximum Water Surface and Maximum Face Velocity values and time rows. It also
checks 1,370,298 joined mesh values (static cell and face arrays, Water Surface and Face Velocity
at one output row with its time stamp, and the Water Surface maximum and its row) and 2,216
cross-section values (Muncie's Maximum Water Surface, Maximum Flow and Maximum Channel Velocity
rows, unsteady Water Surface and Flow at one row, and ConSpan's steady profiles) and 64 structure
and boundary-condition values (Muncie's lateral structures, Chippewa_2D's and DavisStormSystem's
boundary lines, each linked by the oracle independently of the engine), bit for bit.

`tests/results.rs` checks the same joins with exact values on the synthetic plan
`tests/fixtures/results_mesh_xs.h5`, written by `scripts/make_results_fixture.py`.
