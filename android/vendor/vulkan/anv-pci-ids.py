#!/usr/bin/env python3
"""Generate the launcher's ANV allowlist from the Mesa source we build.

Expand Mesa's device-info macros with the C preprocessor, then read the final
.ver initializer for each PCI family. Take both generation limits from ANV's
physical-device checks and exclude devices that require INTEL_FORCE_PROBE.
"""

import re
import subprocess
import sys
from pathlib import Path

mesa = Path(sys.argv[1])
info = (mesa / "src/intel/dev/intel_device_info.c").read_text()
# Only the constant device definitions are needed, without Mesa's headers or
# any compiled code. Undefined C types and enum names are fine to preprocess.
info = info.split("struct device_init_config {")[0]
info = re.sub(r"^#include .*", "", info, flags=re.M)
expanded = subprocess.run(
  ["cc", "-E", "-P", "-x", "c", "-"],
  input=info,
  text=True,
  check=True,
  capture_output=True,
).stdout
versions = {}
for family, fields in re.findall(
  r"static const struct intel_device_info intel_device_info_(\w+)\s*=\s*\{(.*?)\n\};",
  expanded,
  re.S,
):
  # Designated initializers override earlier values from nested macros.
  versions[family] = int(re.findall(r"\.ver\s*=\s*(\d+)\b", fields)[-1])
anv = (mesa / "src/intel/vulkan/anv_physical_device.c").read_text()
(minimum,) = re.findall(r"if \(devinfo.ver < (\d+)\)", anv)
(maximum,) = re.findall(r"if \(devinfo.ver > (\d+)\)", anv)
ids = []
for pci, family, rest in re.findall(
  r"^CHIPSET\(0x([0-9a-fA-F]+),\s*(\w+),(.*)$",
  (mesa / "include/pci_ids/iris_pci_ids.h").read_text(),
  re.M,
):
  if (
    int(minimum) <= versions[family] <= int(maximum)
    and "FORCE_PROBE" not in rest
  ):
    ids.append(int(pci, 16))
assert ids, "no supported ANV PCI IDs found"
print("".join(f"{pci:04x}\n" for pci in sorted(set(ids))), end="")
