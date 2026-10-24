//! Prints what terrio-hecras reads from a HEC-RAS geometry or plan HDF file.
//!
//! ```sh
//! cargo run --example hecras_info -- BaldEagleDamBrk.p07.hdf "Maximum Water Surface"
//! ```

use terrio_hecras::{HecRasHdf, MeshCellOptions};

fn main() -> Result<(), terrio::Error> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: hecras_info <file.hdf> [summary variable...]");
    let hdf = HecRasHdf::open(&path)?;
    println!("file: {path}");
    println!("engine API: {}", terrio_hecras::engine_api_version());
    println!("layers: {:?}", hdf.layer_names()?);
    println!("meshes: {:?}", hdf.mesh_names()?);
    println!("units system: {:?}", hdf.units_system()?);
    println!("CRS WKT: {:?}", hdf.crs()?.map(|w| w.chars().take(80).collect::<String>()));
    let mut options = MeshCellOptions::default();
    for variable in args {
        options = options.with_summary(variable);
    }
    let cells = hdf.mesh_cells(&options)?;
    let mut layer = cells.layer;
    let bbox = layer.bbox();
    println!("{}: {} polygons, bbox {:?}", layer.name, layer.len(), bbox);
    println!("fields: {:?}", layer.schema.fields().iter().map(|f| f.name.as_str()).collect::<Vec<_>>());
    println!("omitted cells: {}", cells.omitted.len());
    for s in &cells.summaries {
        println!(
            "summary {:?}: field {}, time field {:?}, units {:?}, datasets {:?}",
            s.variable, s.field, s.time_field, s.units, s.datasets
        );
    }
    if let Some(f) = layer.features.first() {
        println!("first feature: {:?}", f.attributes);
    }
    Ok(())
}
