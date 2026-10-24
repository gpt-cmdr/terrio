"""Independent h5py oracle for terrio-hecras's opt-in public-model test.

Reads HEC-RAS geometry/plan HDF files with h5py and NumPy only (not the engine) and writes, per
file: the stored Projection and Units System, and per 2D flow area: drawn/stored cell counts,
physical cell count, face count and a digest of every face polyline (start facepoint, stored
perimeter vertices, end facepoint), a digest of the stored perimeter ring, and digests of the
Maximum Water Surface (cells) and Maximum Face Velocity (faces) value and time rows; plus the
count and identity digest of the 1D cross sections, river reaches, storage areas, structures and
2D boundary-condition lines. Usage:

    python -I oracle_public_models.py <test-data-dir> <out.json> <relative file>...

<test-data-dir> holds HEC's public example models (BaldEagleCrkMulti2D, Muncie, Chippewa_2D,
DavisStormSystem, ...) as downloaded by ras-commander's RasExamples. Models are never committed.
"""
import hashlib
import json
import struct
import sys
from pathlib import Path

import h5py
import numpy as np

SUMMARY = "Results/Unsteady/Output/Output Blocks/Base Output/Summary Output/2D Flow Areas"


def text(v):
    if isinstance(v, (bytes, np.bytes_)):
        return bytes(v).decode("utf-8")
    if isinstance(v, np.ndarray) and v.shape == ():
        return text(v[()])
    return str(v)


def digest(values):
    return hashlib.sha256(np.asarray(values, dtype="<f8").tobytes()).hexdigest()


def lines_digest(lines):
    h = hashlib.sha256()
    for coords in lines:
        h.update(struct.pack("<Q", len(coords)))
        for x, y in coords:
            h.update(struct.pack("<dd", float(x), float(y)))
    return h.hexdigest()


def strings_digest(rows):
    return hashlib.sha256("\n".join("|".join(r) for r in rows).encode()).hexdigest()


def table_rows(f, path, fields):
    if path not in f:
        return None
    rows = f[path][()]
    names = rows.dtype.names or ()
    return [[text(r[k]).rstrip() if k in names else "" for k in fields] for r in rows]


def summary(ds, columns):
    v = ds[()]
    return {
        "location": text(ds.attrs["Location"]) if "Location" in ds.attrs else None,
        "value_sha256": digest(v[0][columns]),
        "time_sha256": digest(v[1][columns]) if v.shape[0] == 2 else None,
    }


def main(root, out, files):
    report = {}
    for rel in files:
        path = Path(root) / rel
        with h5py.File(path, "r") as f:
            proj = f.attrs.get("Projection")
            entry = {
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                "projection": None if proj is None else text(proj),
                "units_system": text(f.attrs["Units System"]) if "Units System" in f.attrs else None,
                "meshes": {},
            }
            areas = f.get("Geometry/2D Flow Areas")
            names = [k for k, v in areas.items() if isinstance(v, h5py.Group) and "Cells Face and Orientation Info" in v] if areas else []
            physical = {}
            if areas is not None and "Attributes" in areas:
                attrs = areas["Attributes"][()]
                if "Cell Count" in (attrs.dtype.names or ()):
                    physical = {text(a["Name"]).rstrip(): int(a["Cell Count"]) for a in attrs}
            for name in names:
                g = areas[name]
                faces_per_cell = g["Cells Face and Orientation Info"][:, 1]
                drawn = np.flatnonzero(faces_per_cell >= 3)
                fp = g["FacePoints Coordinate"][()]
                ends = g["Faces FacePoint Indexes"][()]
                info = g["Faces Perimeter Info"][()]
                values = g["Faces Perimeter Values"][()]
                lines = []
                for (a, b), (start, count) in zip(ends, info):
                    coords = [fp[a]] + list(values[start:start + count]) + [fp[b]]
                    lines.append([(c[0], c[1]) for c in coords])
                mesh = {
                    "stored_cells": int(len(faces_per_cell)),
                    "drawn": int(len(drawn)),
                    "drawn_cell_ids": [int(drawn[0]), int(drawn[-1])],
                    "physical_cells": physical.get(name),
                    "faces": len(lines),
                    "faces_sha256": lines_digest(lines),
                    "perimeter_sha256": None,
                }
                if "Perimeter" in g:
                    ring = [tuple(r[:2]) for r in g["Perimeter"][()]]
                    if ring[0] != ring[-1]:
                        ring.append(ring[0])
                    mesh["perimeter_sha256"] = lines_digest([ring])
                s = f.get(f"{SUMMARY}/{name}")
                if s is not None and "Maximum Water Surface" in s:
                    mesh["maximum_water_surface"] = summary(s["Maximum Water Surface"], drawn)
                if s is not None and "Maximum Face Velocity" in s:
                    mesh["maximum_face_velocity"] = summary(s["Maximum Face Velocity"], np.arange(len(lines)))
                entry["meshes"][name] = mesh
            families = {}
            for layer, table, fields in [
                ("cross_sections", "Geometry/Cross Sections/Attributes", ["River", "Reach", "RS"]),
                ("river_centerlines", "Geometry/River Centerlines/Attributes", ["River Name", "Reach Name"]),
                ("storage_areas", "Geometry/Storage Areas/Attributes", ["Name"]),
                ("structures", "Geometry/Structures/Attributes", ["Type", "River", "Reach", "RS", "Connection"]),
                ("bc_lines", "Geometry/Boundary Condition Lines/Attributes", ["Name", "SA-2D", "Type"]),
            ]:
                rows = table_rows(f, table, fields)
                if rows is not None:
                    families[layer] = {"fields": fields, "count": len(rows), "sha256": strings_digest(rows)}
            entry["families"] = families
        report[rel] = entry
    Path(out).write_text(json.dumps(report, indent=2))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2], sys.argv[3:])
