# Remote networking components

ShellLF uses the unmodified EasyTier core and CLI as independent processes.

- EasyTier: v2.6.4 (`2.6.4-8428a89d`), GNU LGPL v3. License: `EasyTier-LICENSE`.
  Corresponding source: https://github.com/EasyTier/EasyTier/tree/v2.6.4
  Source archive: https://github.com/EasyTier/EasyTier/archive/refs/tags/v2.6.4.tar.gz
- Wintun: 0.14.1, the original prebuilt DLL shipped by EasyTier, used through its published API.
  License: `Wintun-LICENSE.txt`. Source and official binary: https://www.wintun.net/
- WinPcap Packet API compatibility DLL: x64, from the official WinPcap 4.1.3 archive.
  BSD-style license: `WinPcap-LICENSE.html` (https://www.winpcap.org/misc/copyright.htm).
  Source: https://www.winpcap.org/devel.htm
  Only Packet.dll is redistributed, not the driver, installer or capture service. ShellLF does not install a capture driver.
  EasyTier's TUN operation uses Wintun. The Packet API DLL satisfies the official core's Windows loader dependency.

The original Npcap Packet.dll in the EasyTier archive is not distributed here. The WinPcap compatibility DLL has the exact API exports imported by this pinned EasyTier core. All network binaries and DLLs are verified against fixed SHA-256 hashes by the packaging script and again before network startup.

The root `LICENSE` is ShellLF's GNU GPL v3 license and is also included alongside these notices.
