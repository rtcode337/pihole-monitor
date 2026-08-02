#!/usr/bin/env python3
"""static/icon.svg と同じ図形をラスタライズして PNG アイコンを書き出す。

iOSのホーム画面(apple-touch-icon)とAndroidのマニフェストはSVGを受け付けないため
PNGが要るが、そのためだけに Pillow や cairosvg を入れたくない。図形が円と円弧
だけで済む単純さなので、数式で直接評価して zlib で PNG を組み立てている。

**図形を変えたら static/icon.svg と本ファイルの定数を両方直すこと**(自動で
同期する仕組みは無い)。書き出し先は static/ 配下で、生成物もコミットする
(ビルド時に include_bytes! で実行ファイルへ埋め込むため)。

    python3 scripts/gen_icons.py
"""

import math
import struct
import zlib
from pathlib import Path

# --- static/icon.svg と一致させる値 -------------------------------------
UNITS = 64.0  # SVGのviewBox
CENTER = 32.0
ARC_R = 24.375  # まぶたの円弧の半径
ARC_OFFSET = 12.375  # 円弧の中心を上下にずらす量(= ARC_R - まぶたの半分の高さ)
LID_WIDTH = 3.5  # まぶたの線幅
PUPIL_R = 7.5
GLOW_R = 14.0
GLOW_ALPHA = 0.55

BG = (0x0D, 0x11, 0x17)
LID = (0xC9, 0xD1, 0xD9)
RED = (0xF8, 0x51, 0x49)
# -----------------------------------------------------------------------

SIZES = [32, 180, 192, 512]
SUPERSAMPLE = 4  # 1ピクセルあたり SUPERSAMPLE^2 回サンプリングしてアンチエイリアス


def sample(x, y):
    """SVG座標(0〜64)の1点の色を返す。SVGの重ね順と同じ順で塗り重ねる。"""
    dx = x - CENTER
    dy = y - CENTER
    dist = math.hypot(dx, dy)

    color = BG

    # 瞳のグロー(中心から外へ不透明度が線形に落ちる radialGradient)
    if dist < GLOW_R:
        a = GLOW_ALPHA * (1.0 - dist / GLOW_R)
        color = tuple(c + (r - c) * a for c, r in zip(color, RED))

    # まぶた: 上下の円弧が作るレンズ形の輪郭線。
    # レンズの内側では「各円の中心からの距離 - 半径」が負になるので、
    # 2つの大きいほうを取れば境界からの符号付き距離になる
    upper = math.hypot(dx, y - (CENTER + ARC_OFFSET)) - ARC_R
    lower = math.hypot(dx, y - (CENTER - ARC_OFFSET)) - ARC_R
    if abs(max(upper, lower)) <= LID_WIDTH / 2:
        color = LID

    if dist <= PUPIL_R:
        color = RED

    return color


def render(size):
    """size×size ピクセルのRGB行データを返す。"""
    scale = UNITS / size
    step = scale / SUPERSAMPLE
    n = SUPERSAMPLE * SUPERSAMPLE
    rows = []
    for py in range(size):
        row = bytearray()
        for px in range(size):
            acc = [0.0, 0.0, 0.0]
            for sy in range(SUPERSAMPLE):
                y = (py * SUPERSAMPLE + sy + 0.5) * step
                for sx in range(SUPERSAMPLE):
                    x = (px * SUPERSAMPLE + sx + 0.5) * step
                    c = sample(x, y)
                    acc[0] += c[0]
                    acc[1] += c[1]
                    acc[2] += c[2]
            row += bytes(min(255, int(v / n + 0.5)) for v in acc)
        rows.append(row)
    return rows


def write_png(path, size, rows):
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    # フィルタタイプ0(None)を各行の先頭に付けるだけの素直なRGB8
    raw = b"".join(b"\x00" + bytes(r) for r in rows)
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 9))
    png += chunk(b"IEND", b"")
    path.write_bytes(png)


def main():
    out_dir = Path(__file__).resolve().parent.parent / "static"
    for size in SIZES:
        path = out_dir / f"icon-{size}.png"
        write_png(path, size, render(size))
        print(f"{path} ({path.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
