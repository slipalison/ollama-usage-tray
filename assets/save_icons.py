#!/usr/bin/env python3
# save_icons.py — render assets/icon.svg to PNG icon theme files.
# Saves ollama-usage-tray.png for sizes 22/32/48 under --home=.../hicolor/
import os
import sys

import gi

gi.require_version('Rsvg', '2.0')
from gi.repository import Rsvg, GdkPixbuf

SVG = """<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24">
  <circle cx="12" cy="12" r="11" fill="#2b2b3b"/>
  <circle cx="12" cy="12" r="9.5" fill="none" stroke="#53bdfa" stroke-width="1.6"/>
  <path d="M7.6 17.6a3.4 3.4 0 0 1-0.38-6.7A4.7 4.7 0 0 1 16.2 10.0a3.1 3.1 0 0 1 0.19 6.15H7.6z" fill="#ffffff"/>
  <ellipse cx="10.2" cy="14.0" rx="0.9" ry="1.05" fill="#1e1e28"/>
  <ellipse cx="13.8" cy="14.0" rx="0.9" ry="1.05" fill="#1e1e28"/>
  <path d="M11.4 15.9h1.2l-0.6 0.9z" fill="#1e1e28"/>
</svg>"""

home = os.path.expanduser("~")
for arg in sys.argv[1:]:
    if arg.startswith("--home="):
        home = arg.split("=", 1)[1]

handle = Rsvg.Handle.new_from_data(SVG.encode())
pb = handle.get_pixbuf()
for size in (22, 32, 48):
    d = os.path.join(home, ".local/share/icons/hicolor", f"{size}x{size}", "apps")
    os.makedirs(d, exist_ok=True)
    scaled = pb.scale_simple(size, size, GdkPixbuf.InterpType.BILINEAR)
    out = os.path.join(d, "ollama-usage-tray.png")
    scaled.savev(out, "png", [], [])
    print(out)
