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
joined summaries, byte budgets).

| Layer | Geometry | Fields | Engine source |
|---|---|---|---|
| `mesh_cells` | Polygon | `mesh_name`, `cell_id`, joined cell summaries | `read_topology` + `assemble` (API 10 ring order) |
| `mesh_cell_points` | Point | `mesh_name`, `cell_id`, `is_ghost`, joined cell summaries | `mesh_geometry` (physical count) + `CellCenters` |
| `mesh_faces` | LineString | `mesh_name`, `face_id`, joined face summaries | `Topology::face_lines` |
| `mesh_perimeters` | Polygon | `mesh_name` | `read_mesh_perimeter` |
| `cross_sections` | LineString | `source_row`, stored columns (`river`, `reach`, `rs`, `name`, bank stations, ...) | `one_d_line_family(CrossSectionCutLines)` |
| `river_centerlines`, `river_bank_lines`, `river_edge_lines`, `river_flow_paths` | LineString | `source_row`, stored columns | `one_d_line_family(...)` |
| `storage_areas` | Polygon | `source_row`, stored columns | `one_d_line_family(StorageAreaPolygons)` |
| `structures` | LineString | `source_row`, stored columns (`type`, `river`, `reach`, `rs`, `connection`, ...) | `one_d_line_family(StructureCenterlines)` |
| `bc_lines`, `breaklines`, `reference_lines` | LineString | `source_row`, stored columns (`name`, `sa_2d`, `type`, ...) | `geometry_family(...)` |
| `refinement_regions` | Polygon | `source_row`, stored columns | `geometry_family(RefinementRegions)` |
| `reference_points` | Point | `source_row`, stored columns | `reference_point_geometry` |

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
store summaries without a `Location`, and the pinned engine refuses them. Engine PR
gpt-cmdr/ras-commander-fast-hdf#9 qualifies their water-surface summaries; their face summaries
stay refused. Through terrio a refusal surfaces as an error, never as an empty layer.

## Features

| Feature | Default | Adds |
|---|---|---|
| `mesh` | yes | The four mesh layers |
| `results` | yes | Summary results joined to cells and faces (implies `mesh`) |

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
538,946 faces and 1,203 family features against h5py: geometry, identities, CRS, units,
ghost counts, and Maximum Water Surface and Maximum Face Velocity values and time rows.
