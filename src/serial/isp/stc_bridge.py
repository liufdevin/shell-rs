"""stcgal adapter: validate capacity before erase and emit bounded progress.

Requires an installed stcgal package in the selected Python environment.
No vendor option overrides, reset shell commands or EEPROM images are passed.
"""
import json
import re
import sys

from stcgal.frontend import StcGal, cli
from stcgal.protocols import StcBaseProtocol, Stc15AProtocol, Stc15Protocol


def progress(current, _written, maximum):
    percent = min(95, 10 + int(85 * current / max(1, maximum)))
    print("\nSHELLLF_ISP " + json.dumps({"percent": percent, "message": "Writing flash"}), flush=True)


original_program = StcGal.program_mcu
original_info = StcBaseProtocol.print_mcu_info


def identified(self):
    original_info(self)
    name = self.model.name
    if not re.match(r"^(STC|IAP|IRC)15\D", name):
        raise IOError("Only known STC15/IAP15 devices are supported by this panel")
    early = re.fullmatch(r"(STC|IAP)15[FL][012]0\d(E|EA|)", name)
    if type(self) is not (Stc15AProtocol if early else Stc15Protocol):
        raise IOError("The selected protocol does not match this chip; use auto-detect")
    print("\nSHELLLF_ISP " + json.dumps({"identity": self.model.name + " / BSL " + self.mcu_bsl_version}), flush=True)


def checked_program(self):
    if type(self.protocol) not in (Stc15Protocol, Stc15AProtocol):
        raise IOError("Only STC15/IAP15 protocols are supported by this panel")
    if self.protocol.model.name == "UNKNOWN":
        raise IOError("Unknown chip capacity; refusing to erase")
    if self.protocol.model.mcs251:
        raise IOError("MCS-251 images are not supported by this panel")
    capacity = self.protocol.split_code if self.protocol.model.iap and self.protocol.split_code else self.protocol.model.code
    image = self.opts.code_image
    image.seek(0, 2)
    length = image.tell()
    image.seek(0)
    # stcgal pads to a 512-byte boundary. Never allow implicit truncation or
    # writing across the code/EEPROM boundary, even for a successful exit code.
    if not length or ((length + 511) // 512) * 512 > capacity:
        raise IOError("Firmware exceeds the detected code flash capacity")
    self.protocol.progress_cb = progress
    print("SHELLLF_ISP " + json.dumps({"percent": 5, "message": "Chip identified; preparing erase"}), flush=True)
    return original_program(self)


StcGal.program_mcu = checked_program
StcBaseProtocol.print_mcu_info = identified
result = cli()
if result == 0:
    print("\nSHELLLF_ISP " + json.dumps({"complete": True}), flush=True)
sys.exit(result)
