"""生成应用图标源图（1024x1024 PNG），交给 `pnpm tauri icon` 派生出全套尺寸。

不依赖 Pillow：手写 PNG，用圆角矩形的有符号距离场做抗锯齿。
设计：暖褐色圆角方块 + 一本摊开的书（两页，中间留书脊缝）。
"""

import math
import os
import struct
import zlib

SIZE = 1024
BG = (154, 107, 69)
PAGE = (251, 249, 245)


def rrect_sdf(x, y, cx, cy, hw, hh, r):
    dx = abs(x - cx) - (hw - r)
    dy = abs(y - cy) - (hh - r)
    ax, ay = max(dx, 0.0), max(dy, 0.0)
    return math.hypot(ax, ay) + min(max(dx, dy), 0.0) - r


def coverage(sdf):
    return min(max(0.5 - sdf, 0.0), 1.0)


def render():
    bg_c = SIZE / 2
    bg_h = SIZE / 2
    # 两页
    pages = [
        (SIZE * 0.356, SIZE * 0.5, SIZE * 0.126, SIZE * 0.222, 26.0),
        (SIZE * 0.644, SIZE * 0.5, SIZE * 0.126, SIZE * 0.222, 26.0),
    ]

    rows = []
    for y in range(SIZE):
        row = bytearray()
        py = y + 0.5
        for x in range(SIZE):
            px = x + 0.5

            a_bg = coverage(rrect_sdf(px, py, bg_c, SIZE * 0.5, bg_h - 8, SIZE * 0.5 - 8, 220.0))
            if a_bg <= 0.0:
                row += bytes((0, 0, 0, 0))
                continue

            r, g, b = BG

            page_a = 0.0
            for cx, cy, hw, hh, rad in pages:
                page_a = max(page_a, coverage(rrect_sdf(px, py, cx, cy, hw, hh, rad)))
            if page_a > 0:
                r = round(r + (PAGE[0] - r) * page_a)
                g = round(g + (PAGE[1] - g) * page_a)
                b = round(b + (PAGE[2] - b) * page_a)

            row += bytes((r, g, b, round(255 * a_bg)))
        rows.append(bytes(row))
    return rows


def write_png(path, rows):
    w = h = SIZE
    raw = b"".join(b"\x00" + r for r in rows)

    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(png)


if __name__ == "__main__":
    rows = render()
    write_png("assets/app-icon.png", rows)
    print("assets/app-icon.png")
