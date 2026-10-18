# -*- coding: utf-8 -*-
"""生成 assets/sainput.ico —— 和 tray.rs 里画的是同一个麦克风。

Rust 那边用 GDI 自己画（不依赖外部资源）；这里生成一个 .ico 给桌面快捷方式用。
画法保持一致：胶囊话筒 + U 形支架 + 立杆 + 底座，3x3 超采样抗锯齿。
"""
import os, struct

SIZES = [16, 32, 48]


def seg_dist(px, py, x1, y1, x2, y2):
    dx, dy = x2 - x1, y2 - y1
    l2 = dx * dx + dy * dy
    t = 0.0 if l2 == 0 else max(0.0, min(1.0, ((px - x1) * dx + (py - y1) * dy) / l2))
    cx, cy = x1 + t * dx, y1 + t * dy
    return ((px - cx) ** 2 + (py - cy) ** 2) ** 0.5


def in_mic(x, y, scale):
    """坐标系固定在 32x32，按目标尺寸缩放。"""
    k = 32.0 / scale
    x, y = x * k, y * k
    if seg_dist(x, y, 16.0, 9.0, 16.0, 14.5) <= 4.6:
        return True
    d = ((x - 16.0) ** 2 + (y - 15.0) ** 2) ** 0.5
    if y >= 15.0 and 8.2 <= d <= 10.6:
        return True
    if 15.2 <= x <= 16.8 and 25.0 <= y <= 27.5:
        return True
    if seg_dist(x, y, 12.0, 28.2, 20.0, 28.2) <= 1.5:
        return True
    return False


def render(size):
    ss = 3
    px = bytearray(size * size * 4)
    for py in range(size):
        for pxx in range(size):
            hit = 0
            for sy in range(ss):
                for sx in range(ss):
                    if in_mic(pxx + (sx + 0.5) / ss, py + (sy + 0.5) / ss, size):
                        hit += 1
            a = hit * 255 // (ss * ss)
            o = (py * size + pxx) * 4
            px[o], px[o + 1], px[o + 2], px[o + 3] = 255, 200, 130, a  # BGRA
    return bytes(px)


def ico_image(size):
    """ICO 里的单个图像：BITMAPINFOHEADER(高度写双倍) + BGRA 自下而上 + AND 掩码。"""
    header = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, size * size * 4, 0, 0, 0, 0)
    bgra = render(size)
    bottom_up = b"".join(bgra[y * size * 4:(y + 1) * size * 4] for y in range(size - 1, -1, -1))
    mask_row = ((size + 31) // 32) * 4
    return header + bottom_up + b"\x00" * (mask_row * size)


def build(path):
    imgs = [ico_image(s) for s in SIZES]
    out = struct.pack("<HHH", 0, 1, len(imgs))
    offset = 6 + 16 * len(imgs)
    for s, data in zip(SIZES, imgs):
        out += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    for data in imgs:
        out += data
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(out)
    print("写出 %s (%d bytes, sizes=%s)" % (path, len(out), SIZES))


if __name__ == "__main__":
    build(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "assets", "sainput.ico"))