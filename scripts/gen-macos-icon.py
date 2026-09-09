#!/usr/bin/env python3
"""生成 macOS 菜单栏输入源模板图标（Icon.pdf）。

设计：实心圆角气泡 + 右下尾巴，内部用 even-odd 填充挖出品牌「言」字。
18pt 模板画布既符合 macOS 菜单栏高度，又比旧版细线气泡更醒目；「言」字
与 `assets/branding/verba-256.png` 的应用图标保持同一品牌语言。

背景：菜单栏高度仅约 22pt，2026-08 的手写版 Icon.pdf 用了 144x144pt 的
MediaBox，菜单栏被顶高（用户反馈「图标快把菜单撑爆」）。本脚本按目标画布
重新缩放导出，并内置结构自校验（xref 偏移、Length、startxref、MediaBox、
even-odd 填充）。

为什么用脚本而不是手改 PDF：手写 PDF 的 /Length 与 xref 各字节偏移必须
精确，手改极易出错——仓库曾为 /Length 单独修过一次（commit b57c08b，
其 187 的口径是「运算符以 LF 连接、无尾换行」，与文件实际的 CRLF 数据
191 字节并不一致，解析器靠扫描 endstream 容错）。本脚本按「/Length =
stream 数据实际字节数」的精确口径生成，并内置结构自校验。

用法：
    python scripts/gen-macos-icon.py            # 18x18pt 输出到默认路径
    python scripts/gen-macos-icon.py --size 22 --margin 1.0
"""

from __future__ import annotations

import argparse
from pathlib import Path

DEFAULT_OUT = (
    Path(__file__).resolve().parents[1]
    / "frontends"
    / "macos"
    / "ime"
    / "app"
    / "Resources"
    / "Icon.pdf"
)

# 图标几何（目标画布上的「唯一来源」）：外层实心气泡 + 右下尾巴，
# 后续子路径作为 even-odd 负空间挖出品牌「言」字。
# 形如 ("c", [(x1,y1), (x2,y2), (x3,y3)]) 为三次贝塞尔。
PATH_OPS: list[tuple[str, list[tuple[float, float]]]] = [
    # 外层实心气泡：圆角矩形 + 右下尾巴（一个闭合子路径）
    ("m", [(4.0, 3.0)]),
    ("l", [(13.0, 3.0)]),
    ("l", [(16.0, 0.8)]),
    ("l", [(14.2, 3.0)]),
    ("l", [(14.5, 3.0)]),
    ("c", [(15.5, 3.0), (16.0, 3.5), (16.0, 4.5)]),
    ("l", [(16.0, 13.5)]),
    ("c", [(16.0, 14.5), (15.5, 15.0), (14.5, 15.0)]),
    ("l", [(5.5, 15.0)]),
    ("c", [(4.5, 15.0), (4.0, 14.5), (4.0, 13.5)]),
    ("l", [(4.0, 4.5)]),
    ("c", [(4.0, 3.5), (4.5, 3.0), (5.5, 3.0)]),
    ("h", []),
    # 以下子路径是 even-odd 负空间，挖出品牌「言」字：
    # 顶部斜点
    ("m", [(8.1, 13.8)]),
    ("l", [(9.2, 14.8)]),
    ("l", [(10.5, 13.3)]),
    ("l", [(9.4, 12.3)]),
    ("h", []),
    # 上横
    ("m", [(5.4, 11.0)]),
    ("l", [(12.6, 11.0)]),
    ("l", [(12.6, 12.4)]),
    ("l", [(5.4, 12.4)]),
    ("h", []),
    # 中横
    ("m", [(6.0, 9.0)]),
    ("l", [(12.0, 9.0)]),
    ("l", [(12.0, 10.4)]),
    ("l", [(6.0, 10.4)]),
    ("h", []),
    # 下方「口」外框
    ("m", [(5.6, 4.8)]),
    ("l", [(12.4, 4.8)]),
    ("l", [(12.4, 8.4)]),
    ("l", [(5.6, 8.4)]),
    ("h", []),
    # 下方「口」内框（与外框构成负空间边框）
    ("m", [(7.0, 6.2)]),
    ("l", [(7.0, 7.0)]),
    ("l", [(11.0, 7.0)]),
    ("l", [(11.0, 6.2)]),
    ("h", []),
]

def bbox(ops: list[tuple[str, list[tuple[float, float]]]]) -> tuple[float, float, float, float]:
    xs = [p[0] for _, pts in ops for p in pts]
    ys = [p[1] for _, pts in ops for p in pts]
    return min(xs), min(ys), max(xs), max(ys)


def transform(
    ops: list[tuple[str, list[tuple[float, float]]]],
    size: float,
    margin: float,
) -> list[tuple[str, list[tuple[float, float]]]]:
    """等比缩放到 size x size 画布、留 margin 薄边并居中。"""
    x0, y0, x1, y1 = bbox(ops)
    w, h = x1 - x0, y1 - y0
    avail = size - 2 * margin
    s = min(avail / w, avail / h)
    ox = (size - w * s) / 2 - x0 * s
    oy = (size - h * s) / 2 - y0 * s
    return [
        (op, [(px * s + ox, py * s + oy) for px, py in pts]) for op, pts in ops
    ]


def fmt(v: float) -> str:
    s = f"{v:.3f}".rstrip("0").rstrip(".")
    return s if s else "0"


def render_ops(ops: list[tuple[str, list[tuple[float, float]]]]) -> str:
    parts: list[str] = []
    for op, pts in ops:
        if op == "h":
            parts.append("h")
        else:
            coords = " ".join(f"{fmt(px)} {fmt(py)}" for px, py in pts)
            parts.append(f"{coords} {op}")
    return " ".join(parts)


def build_pdf(size: float, ops: list[tuple[str, list[tuple[float, float]]]]) -> bytes:
    """组装单页 PDF；/Length 与 xref 偏移按实际字节计算。"""
    content = f"0 0 0 rg\n{render_ops(ops)}\nf*\n".encode("ascii")
    size_str = fmt(size)
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        (
            f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {size_str} {size_str}] "
            "/Resources << >> /Contents 4 0 R >>"
        ).encode("ascii"),
        (
            f"<< /Length {len(content)} >>\nstream\n"
            + content.decode("ascii")
            + "endstream"
        ).encode("ascii"),
    ]
    out = bytearray(b"%PDF-1.4\n")
    offsets: list[int] = []
    for i, body in enumerate(objs, start=1):
        offsets.append(len(out))
        out += f"{i} 0 obj\n".encode("ascii")
        out += body
        out += b"\nendobj\n"
    xref_pos = len(out)
    out += f"xref\n0 {len(objs) + 1}\n".encode("ascii")
    out += b"0000000000 65535 f \n"
    for off in offsets:
        out += f"{off:010d} 00000 n \n".encode("ascii")
    out += (
        f"trailer << /Size {len(objs) + 1} /Root 1 0 R >>\n"
        f"startxref\n{xref_pos}\n%%EOF\n"
    ).encode("ascii")
    return bytes(out)


def verify(pdf: bytes, size: float) -> None:
    """结构自校验：xref 偏移、stream Length、MediaBox、even-odd 填充。

    这是「单一事实来源 + 断言相等」的落地：生成器若写出偏移错误的
    PDF，在这里红掉，而不是留给 macOS 解析器容错。
    """
    # 1) xref 每个偏移都指向对应的 "N 0 obj"
    xref_pos = int(pdf[pdf.rfind(b"startxref\n") + len(b"startxref\n"):].split(b"\n")[0])
    assert pdf[xref_pos:xref_pos + 5] == b"xref\n", "startxref 未指向 xref 表"
    table = pdf[xref_pos:].split(b"\n")
    count = int(table[1].split()[1])
    for i in range(1, count):
        # 表布局：table[2] 是 0 号 free 条目，对象 i 的条目在 table[2 + i]。
        entry = table[2 + i]
        off = int(entry[:10])
        expect = f"{i} 0 obj".encode("ascii")
        assert pdf[off:off + len(expect)] == expect, f"xref 偏移 {off} 未指向对象 {i}"

    # 2) /Length 与 stream 实际数据字节数一致
    s = pdf.find(b"stream\n") + len(b"stream\n")
    e = pdf.find(b"endstream")
    declared = int(pdf[:s].rsplit(b"/Length ", 1)[1].split(b" ")[0])
    assert declared == e - s, f"/Length {declared} != 实际 {e - s}"

    # 3) MediaBox 与请求尺寸一致
    assert f"[0 0 {fmt(size)} {fmt(size)}]".encode("ascii") in pdf, "MediaBox 与目标尺寸不符"

    # 4) 品牌「言」字依赖 even-odd 负空间填充
    assert b"f*\n" in pdf, "图标未使用 even-odd 填充"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--size", type=float, default=18.0, help="画布边长（pt），默认 18")
    ap.add_argument("--margin", type=float, default=0.8, help="字形四周薄边（pt），默认 0.8")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT, help="输出路径")
    args = ap.parse_args()

    pdf = build_pdf(args.size, transform(PATH_OPS, args.size, args.margin))
    verify(pdf, args.size)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_bytes(pdf)
    print(f"OK: {args.out} ({len(pdf)} bytes, MediaBox {fmt(args.size)}x{fmt(args.size)}pt)")


if __name__ == "__main__":
    main()
