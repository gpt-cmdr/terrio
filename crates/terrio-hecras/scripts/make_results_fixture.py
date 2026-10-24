"""Writes tests/fixtures/results_mesh_xs.h5: a synthetic plan-HDF-shaped file for CI.

It holds no model data. The mesh topology is the engine coverage fixture's `Good` mesh (two
drawable cells and a one-face cell, see README.md), copied from coverage_mesh.h5; every other
value is made up here. Dataset names, shapes, types and attributes follow what HEC-RAS 6.x/7.0
writes (as seen in HEC's public example plans), so the engine qualifies them as it does real files:

- `Geometry/2D Flow Areas/Attributes` (name and physical `Cell Count`) and the static cell and face
  arrays (minimum elevations, Manning's n, face normals and lengths, face cell indexes);
- `Summary Output/2D Flow Areas/Good/Maximum Water Surface` (value and Time rows);
- `Unsteady Time Series` `Time`, `Time Date Stamp`, cell `Water Surface` and face `Face Velocity`;
- three cross sections (attributes and cut lines), their `Summary Output/Cross Sections/Maximum
  Water Surface` (value, flow and time rows) and unsteady `Water Surface`;
- one river junction (`Geometry/Junctions` attributes and point).

Usage: python -I make_results_fixture.py <coverage_mesh.h5> <out.h5>
"""
import sys

import h5py
import numpy as np

BASE = "Results/Unsteady/Output/Output Blocks/Base Output"
MESH = "Good"
TOPOLOGY = [
    "Cells Face and Orientation Info",
    "Cells Face and Orientation Values",
    "FacePoints Coordinate",
    "Faces FacePoint Indexes",
    "Faces Perimeter Info",
    "Faces Perimeter Values",
]
WKT = 'PROJCS["Synthetic",GEOGCS["GCS_North_American_1983",DATUM["D_North_American_1983",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",-87.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Foot_US",0.3048006096012192]]'


def labelled(group, name, data, column, row):
    d = group.create_dataset(name, data=data)
    d.attrs["Can Plot"] = np.bytes_(b"False")
    d.attrs["Column"] = np.array(column, dtype=f"S{max(len(c) for c in column)}")
    d.attrs["Row"] = np.bytes_(row)
    return d


def main(source, out):
    with h5py.File(source, "r") as src, h5py.File(out, "w") as f:
        f.attrs["File Type"] = np.bytes_(b"HEC-RAS Results")
        f.attrs["File Version"] = np.bytes_(b"HEC-RAS 6.6 September 2024")
        f.attrs["Projection"] = np.bytes_(WKT.encode())
        f.attrs["Units System"] = np.bytes_(b"US Customary")

        areas = f.create_group("Geometry/2D Flow Areas")
        attrs = np.zeros(1, dtype=[("Name", "S16"), ("Cell Count", "<i4")])
        attrs[0] = (MESH.encode(), 2)
        areas.create_dataset("Attributes", data=attrs)
        g = areas.create_group(MESH)
        for name in TOPOLOGY:
            g.create_dataset(name, data=src[f"Geometry/2D Flow Areas/{MESH}/{name}"][()])
        cells = len(g["Cells Face and Orientation Info"])
        faces = len(g["Faces FacePoint Indexes"])
        fp = g["FacePoints Coordinate"][()]
        ends = g["Faces FacePoint Indexes"][()]
        g.create_dataset("Cells Center Coordinate", data=np.array([[0.5, 0.5], [1.5, 0.5], [2.5, 0.5]][:cells]))
        labelled(g, "Cells Minimum Elevation", np.array([10.25, 11.5, np.nan][:cells], dtype="<f4"), [b"Minimum Elevation"], b"Cell")
        labelled(g, "Cells Center Manning's n", np.array([0.035, 0.06, 0.035][:cells], dtype="<f4"), [b"Index"], b"Cell")
        labelled(g, "Faces Minimum Elevation", (9.0 + np.arange(faces) / 4).astype("<f4"), [b"Minimum Elevation"], b"Face")
        d = fp[ends[:, 1]] - fp[ends[:, 0]]
        length = np.hypot(d[:, 0], d[:, 1])
        normal = np.stack([d[:, 1] / length, -d[:, 0] / length, length], axis=1).astype("<f4")
        labelled(g, "Faces NormalUnitVector and Length", normal, [b"X Component", b"Y Component", b"Face Length"], b"Face")
        cell_index = np.array([[i % cells, (i + 1) % cells] for i in range(faces)], dtype="<i4")
        labelled(g, "Faces Cell Indexes", cell_index, [b"Cell 0", b"Cell 1"], b"Face")

        # Results: three output rows, one 2D mesh, three cross sections.
        times = np.array([0.0, 0.5, 1.0])
        ts = f.create_group(f"{BASE}/Unsteady Time Series")
        t = ts.create_dataset("Time", data=times)
        t.attrs["Time"] = np.bytes_(b"days")
        ts.create_dataset("Time Date Stamp", data=np.array([b"01JAN2000 00:00:00", b"01JAN2000 12:00:00", b"02JAN2000 00:00:00"], dtype="S19"))
        m = ts.create_group(f"2D Flow Areas/{MESH}")
        wse = np.array([[12.0, 12.5, 13.0], [12.75, 12.25, 13.0], [12.5, 13.0, 13.0]], dtype="<f4")[:, :cells]
        for name, data, columns, units in [
            ("Water Surface", wse, b"Cells", b"ft"),
            ("Face Velocity", (np.arange(3 * faces).reshape(3, faces) % 5 - 2).astype("<f4") / 4, b"Faces", b"ft/s"),
        ]:
            d = m.create_dataset(name, data=data)
            d.attrs["Columns"] = np.bytes_(columns)
            d.attrs["Location"] = np.bytes_(columns)
            d.attrs["Rows"] = np.bytes_(b"Times")
            d.attrs["Units"] = np.bytes_(units)
        s = f.create_group(f"{BASE}/Summary Output/2D Flow Areas/{MESH}")
        at = np.argmax(wse, axis=0)
        d = s.create_dataset("Maximum Water Surface", data=np.stack([wse.max(axis=0), times[at]]).astype("<f4"))
        d.attrs["Rows Variables"] = np.array([b"WSEL", b"Time"], dtype="S16")
        d.attrs["Units"] = np.array([b"ft", b"days"], dtype="S16")

        xs = f.create_group("Geometry/Cross Sections")
        rows = [(b"Creek", b"Upper", b"300", b"A"), (b"Creek", b"Upper", b"200", b"B"), (b"Creek", b"Lower", b"100", b"C")]
        a = np.zeros(3, dtype=[("River", "S16"), ("Reach", "S16"), ("RS", "S8"), ("Name", "S16"), ("Left Bank", "<f4"), ("Right Bank", "<f4")])
        for i, r in enumerate(rows):
            a[i] = (*r, 10.0 * i, 10.0 * i + 5)
        xs.create_dataset("Attributes", data=a)
        points = np.array([[0.0, 3.0], [4.0, 3.0], [0.0, 2.0], [2.0, 2.5], [4.0, 2.0], [0.0, 1.0], [4.0, 1.0]])
        labelled(xs, "Polyline Info", np.array([[0, 2, 0, 1], [2, 3, 1, 1], [5, 2, 2, 1]], dtype="<i4"),
                 [b"Point Starting Index", b"Point Count", b"Part Starting Index", b"Part Count"], b"Feature").attrs["Feature Type"] = np.bytes_(b"Polyline")
        labelled(xs, "Polyline Parts", np.array([[0, 2], [0, 3], [0, 2]], dtype="<i4"), [b"Point Starting Index", b"Point Count"], b"Part")
        labelled(xs, "Polyline Points", points, [b"X", b"Y"], b"Points")
        j = f.create_group("Geometry/Junctions")
        ja = np.zeros(1, dtype=[("Name", "S16"), ("Description", "S32")])
        ja[0] = (b"Confluence", b"synthetic")
        j.create_dataset("Attributes", data=ja)
        j.create_dataset("Points", data=np.array([[4.0, 1.5]]))
        xr = ts.create_group("Cross Sections")
        ra = np.zeros(3, dtype=[("River", "S16"), ("Reach", "S16"), ("Station", "S8"), ("Name", "S16")])
        for i, r in enumerate(rows):
            ra[i] = r
        xr.create_dataset("Cross Section Attributes", data=ra)
        xs_wse = np.array([[20.0, 18.0, 15.0], [21.5, 18.5, 15.25], [21.0, 19.0, 15.5]], dtype="<f4")
        d = xr.create_dataset("Water Surface", data=xs_wse)
        d.attrs["Variable"] = np.array([b"Stage", b"Feet"], dtype="S32")
        d.attrs["Variable Name"] = np.bytes_(b"Stage")
        d.attrs["Variable Units"] = np.bytes_(b"Feet")
        xs_at = np.argmax(xs_wse, axis=0)
        flow = np.array([100.0, 150.0, 175.0], dtype="<f4")
        d = f.create_dataset(f"{BASE}/Summary Output/Cross Sections/Maximum Water Surface",
                             data=np.stack([xs_wse.max(axis=0), flow, times[xs_at]]).astype("<f4"))
        d.attrs["Variable_Unit"] = np.array([[b"Max WSEL", b"ft"], [b"Flow at Max WSEL", b"cfs"], [b"Time at Max WSEL", b"days"]], dtype="S32")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
