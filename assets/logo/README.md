# ShellLF application icon

`shelllf.png` is the supplied source image. Keep its transparency and artwork.

Generate the platform files with Python and Pillow:

```sh
python scripts/build-icons.py
```

- `shelllf.ico`: Windows executable, taskbar, tray and installer; includes
  16, 24, 32, 48, 64, 128 and 256 pixel images.
- `shelllf.icns`: macOS application bundle; includes sizes up to 1024 pixels.
- `shelllf.png`: Linux application icon and the macOS development Dock icon.
