#!/usr/bin/env python3
# render_icon.py — rasterize the ollama-usage-tray SVG icon to ARGB raw bytes on stdout.
# Usage: render_icon.py <size> <frac>
# Output: raw ARGB bytes (a,r,g,b per pixel, row-major), size*size*4 long.
import sys

import gi

gi.require_version('Rsvg', '2.0')
from gi.repository import Rsvg, GdkPixbuf

SVG = """<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24">
  <circle cx="12" cy="12" r="11" fill="#2b2b3b"/>
  <circle cx="12" cy="12" r="9.5" fill="none" stroke="{ring}" stroke-width="1.6"/>
  <path d="M7.6 17.6a3.4 3.4 0 0 1-0.38-6.7A4.7 4.7 0 0 1 16.2 10.0a3.1 3.1 0 0 1 0.19 6.15H7.6z" fill="#ffffff"/>
  <ellipse cx="10.2" cy="14.0" rx="0.9" ry="1.05" fill="#1e1e28"/>
  <ellipse cx="13.8" cy="14.0" rx="0.9" ry="1.05" fill="#1e1e28"/>
  <path d="M11.4 15.9h1.2l-0.6 0.9z" fill="#1e1e28"/>
</svg>"""


def ring_color(frac: float) -> str:
    if frac >= 0.9:
        return "#e5484d"
    if frac >= 0.7:
        return "#ffac33"
    return "#53bdfa"


def main():
    size, frac = int(sys.argv[1]), float(sys.argv[2])
    svg = SVG.format(ring=ring_color(frac))
    handle = Rsvg.Handle.new_from_data(svg.encode())
    pb = handle.get_pixbuf()
    if pb.get_width() != size:
        pb = pb.scale_simple(size, size, GdkPixbuf.InterpType.BILINEAR)
    # pixbuf pixels: RGBA bytes, row-major, rowstride = width*4
    pixels = bytes(pb.get_pixels())
    n = size * size * 4
    pixels = pixels[:n]
    out = bytearray(n)
    for i in range(0, n, 4):
        r, g, b, a = pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]
        out[i:i + 4] = bytes([a, r, g, b])
    sys.stdout.buffer.write(bytes(out))


if __name__ == '__main__':
    main()
