//! Prints what terrio-hecras reads from a HEC-RAS geometry or plan HDF file.
//!
//! ```sh
//! cargo run --example hecras_info -- BaldEagleDamBrk.p07.hdf "Maximum Water Surface"
//! ```

use terrio_hecras::{HecRasHdf, MeshOptions};

fn main() -> Result<(), terrio::Error> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: hecras_info <file.hdf> [summary variable...]");
    let hdf = HecRasHdf::open(&path)?;
    println!("file: {path}");
    println!("engine API: {}", terrio_hecras::engine_api_version());
    println!("layers: {:?}", hdf.layer_names()?);
    let layers = hdf.layer_names()?;
    println!("units system: {:?}", hdf.units_system()?);
    println!("CRS WKT: {:?}", hdf.crs()?.map(|w| w.chars().take(80).collect::<String>()));
    let mut options = MeshOptions::default();
    for variable in args {
        options = options.with_summary(variable);
    }
    if !layers.iter().any(|l| l == "mesh_cells") {
        println!("no 2D flow areas");
    } else {
        println!("meshes: {:?}", hdf.mesh_names()?);
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
    }
    for name in layers {
        if name == "mesh_cells" {
            continue;
        }
        match hdf.read_layer(&name) {
            Ok(l) => {
                println!("{name}: {} features, fields {:?}", l.len(), l.schema.fields().iter().map(|f| f.name.as_str()).collect::<Vec<_>>())
            }
            Err(e) => println!("{name}: error: {e}"),
        }
    }
    Ok(())
}
