#!/usr/bin/env python3
"""生成 cloud-init NoCloud 种子 ISO（卷标 CIDATA）。"""
import sys

sys.path.insert(0, r"M:\lutraos\_cache\python")
import pycdlib  # noqa: E402

SRC = r"M:\lutraos\_vm\cloud-init"
OUT = r"M:\lutraos\_vm\seed.iso"

iso = pycdlib.PyCdlib()
iso.new(interchange_level=3, joliet=3, vol_ident="cidata", rock_ridge="1.09")
for name in ("user-data", "meta-data"):
    iso.add_file(
        filename=rf"{SRC}\{name}",
        iso_path=f"/{name.upper().replace('-', '_')};1",
        rr_name=name,
        joliet_path=f"/{name}",
    )
iso.write(OUT)
iso.close()
print(f"wrote {OUT}")
