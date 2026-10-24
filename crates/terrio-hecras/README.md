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

| Layer | Geometry | Fields | Source |
|---|---|---|---|
| `mesh_cells` | Polygon | `mesh_name` (text), `cell_id` (integer), then one float field per joined summary (`maximum_water_surface`, and `maximum_water_surface_time` when the engine proves a stored Time row) | `Geometry/2D Flow Areas/<mesh>` topology; `Results/.../Summary Output/2D Flow Areas/<mesh>/<variable>` |

- A cell is identified by `mesh_name` and `cell_id` together. `cell_id` is the stored cell index
  and repeats across meshes.
- Polygons are the engine's (API 10): clockwise, starting at the first listed face. terrio
  rings omit the closing vertex. Cells the engine does not draw (fewer than three faces, as
  for ghost cells) are listed in `MeshCellLayer::omitted`.
- The CRS is the stored `Projection` text, attached as WKT. No EPSG code is guessed. A file
  without a projection gives layers without a CRS. Malformed, conflicting, unsupported or
  corrupt projection evidence is an error.
- Summary values are the stored numbers, widened from `f32` to `f64` exactly. Time rows are
  the stored values (days), not timestamps. `SummaryField::units` gives the units the engine
  can prove; it is `None` otherwise and is never inferred.

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

Some real files are refused today. For example, the engine does not qualify `Maximum Water
Surface` in HEC's `BaldEagleDamBrk.p07.hdf` (written by HEC-RAS 5.00, with no stored `Location`).
ras-commander reads that file with h5py instead. Through terrio the refusal surfaces as an error.

## Features

| Feature | Default | Adds |
|---|---|---|
| `mesh` | yes | The `mesh_cells` layer |
| `results` | yes | Summary results joined to cells (implies `mesh`) |

With no features, only `HecRasHdf::open`, `crs` and `units_system` are built.

## Why a separate crate

The engine is not on crates.io. It is pinned by Git revision, which a crates.io package cannot
depend on, even optionally. So this crate is outside terrio's workspace, with its own
`Cargo.lock` and CI job (`.github/workflows/hecras.yml`). terrio's releases, licence notices
and WebAssembly builds are unchanged. The engine memory-maps files, so this crate is
native-only.

## Testing

```sh
cargo test                       # synthetic engine fixtures (tests/fixtures)
python -I scripts/oracle_public_models.py <models> oracle.json   # needs h5py and numpy
TERRIO_HECRAS_TEST_DATA=<models> TERRIO_HECRAS_ORACLE=oracle.json cargo test --test public_models
```

`<models>` holds HEC's public example models (`BaldEagleCrkMulti2D`, `Muncie`), as downloaded by
ras-commander's `RasExamples`, with computed plan HDFs.
