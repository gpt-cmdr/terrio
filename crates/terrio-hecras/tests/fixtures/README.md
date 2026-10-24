# Fixtures

Synthetic HEC-RAS-shaped HDF5 files copied unchanged from the engine repository,
[gpt-cmdr/ras-commander-fast-hdf](https://github.com/gpt-cmdr/ras-commander-fast-hdf)
(MIT), `crates/ras-commander-rust/tests/fixtures/coverage/`, at commit
`a7243bfa36b2fb7d2d54c1a49ea2906e68d6ef3d` (the engine revision this crate pins). Their generator
and full manifest are listed in that directory's `coverage_manifest.json`. They contain no model
data.

| File | SHA-256 | Contents |
|---|---|---|
| `coverage_mesh.h5` | `ff8172ccd3ff76e643416faa474110e6ba8bf8ff218333e8423dbe8591491117` | 19 meshes: `Good` (two drawable cells and a one-face cell), precision and width variants, and broken topologies |
| `coverage_mesh_nogroups.h5` | `4ed5ad7c89e14ff367a4a0ac34d2ea85c5b78913d651d62a49fe1eddedef84fd` | Legacy names-only layout (the engine refuses it) |
| `results_mesh_xs.h5` | `3bc7a6aa9e0aa4ba9090f6f1b0ea6763cfc6cc9da5425283f83f874428213c9a` | Written by `scripts/make_results_fixture.py` from `coverage_mesh.h5` (this crate, MIT): the `Good` mesh with made-up static arrays, summary, time series and three cross sections with summary and unsteady results, in the stored HEC-RAS 6.x layout. No model data |
| `coverage_mesh_nogeometry.h5` | `3117dbec3e3c323ffbec8ea95728a55b3e592d5f792ab6fa15151902e0220485` | No `Geometry/2D Flow Areas` group |

Real HEC-RAS models are not committed. `tests/public_models.rs` reads HEC's public example
models from `TERRIO_HECRAS_TEST_DATA` and compares them with the JSON that
`scripts/oracle_public_models.py` writes with h5py.
