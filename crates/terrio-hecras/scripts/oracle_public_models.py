"""Independent h5py oracle for terrio-hecras's opt-in public-model test.

Reads HEC-RAS geometry/plan HDF files with h5py (not the engine) and writes, per file and mesh:
the stored Projection text, the cells the engine contract draws (three or more listed faces), and
SHA-256 digests of the joined summary values in cell order. Usage:

    python oracle_public_models.py <test-data-dir> <out.json>

<test-data-dir> holds HEC's public example models as downloaded by ras-commander's RasExamples:
BaldEagleCrkMulti2D/BaldEagleDamBrk.g12.hdf, BaldEagleCrkMulti2D/BaldEagleDamBrk.p07.hdf,
Muncie/Muncie.g04.hdf and Muncie/Muncie.p04.hdf (any that exist are recorded).
"""
import hashlib
import json
import sys
from pathlib import Path

import h5py
import numpy as np

FILES = [
    "BaldEagleCrkMulti2D/BaldEagleDamBrk.g12.hdf",
    "BaldEagleCrkMulti2D/BaldEagleDamBrk.p07.hdf",
    "Muncie/Muncie.g04.hdf",
    "Muncie/Muncie.p04.hdf",
]
SUMMARY = "Results/Unsteady/Output/Output Blocks/Base Output/Summary Output/2D Flow Areas"


def text(v):
    if isinstance(v, bytes):
        return v.decode("utf-8")
    if isinstance(v, np.ndarray) and v.shape == ():
        return text(v[()])
    return str(v)


def digest(values):
    return hashlib.sha256(np.asarray(values, dtype="<f8").tobytes()).hexdigest()


def main(root, out):
    report = {}
    for rel in FILES:
        path = Path(root) / rel
        if not path.exists():
            continue
        with h5py.File(path, "r") as f:
            proj = f.attrs.get("Projection")
            entry = {
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                "projection": None if proj is None else text(proj),
                "units_system": text(f.attrs["Units System"]) if "Units System" in f.attrs else None,
                "meshes": {},
            }
            areas = f["Geometry/2D Flow Areas"]
            # Mesh groups, as the engine lists them (HEC-RAS 5.0 files have no Attributes table).
            names = [k for k, v in areas.items() if isinstance(v, h5py.Group) and "Cells Face and Orientation Info" in v]
            for name in names:
                g = f[f"Geometry/2D Flow Areas/{name}"]
                faces = g["Cells Face and Orientation Info"][:, 1]
                drawn = np.flatnonzero(faces >= 3)
                mesh = {"stored_cells": int(len(faces)), "drawn_cell_ids": [int(drawn[0]), int(drawn[-1])], "drawn": int(len(drawn))}
                ds = f.get(f"{SUMMARY}/{name}/Maximum Water Surface")
                if ds is not None:
                    v = ds[()]
                    mesh["maximum_water_surface"] = {
                        "shape": list(v.shape),
                        "location": text(ds.attrs["Location"]) if "Location" in ds.attrs else None,
                        "value_sha256": digest(v[0][drawn]),
                        "time_sha256": digest(v[1][drawn]) if v.shape[0] == 2 else None,
                        "first": [float(v[0][drawn[0]]), float(v[1][drawn[0]])],
                    }
                entry["meshes"][name] = mesh
        report[rel] = entry
    Path(out).write_text(json.dumps(report, indent=2))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
