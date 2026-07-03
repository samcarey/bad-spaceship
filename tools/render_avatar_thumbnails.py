"""Render square, face-framed thumbnails of the monster glTF models with a pure
numpy software rasterizer (no GPU / no system GL libs). Front view (models face
+Z), orthographic, texture-atlas colours sampled per face, simple lambert shading,
4x supersample -> smooth downscale, transparent background.

Regenerates client/assets/monsters/thumbnails/<monster>.png (one per entry in
`MONSTERS`, matching client/src/monster.rs). Run from the repo root:
    pip install numpy pillow trimesh networkx
    python3 tools/render_avatar_thumbnails.py
No GPU required — it runs anywhere Python does (e.g. the headless Mac build box)."""
import numpy as np
import trimesh
from PIL import Image

SRC = "client/assets/monsters"
OUT = "client/assets/monsters/thumbnails"
MONSTERS = [
    "Alien.glb", "Alien_Tall.glb", "Ghost.glb", "GreenDemon.glb",
    "Cyclops.glb", "Demon.glb", "Yeti.glb", "Mushroom.glb",
]

OUTSIZE = 128      # final thumbnail size (px)
SS = 4             # supersample factor
FACE_DROP = 0.27   # center the crop this fraction of height below the top
FACE_ZOOM = 0.36   # square half-size as fraction of model height
LIGHT = np.array([0.35, 0.55, 1.0]); LIGHT = LIGHT / np.linalg.norm(LIGHT)
AMBIENT = 0.62


def sample_texture(mesh, faces):
    """Return an (nfaces, 3) RGB uint8 colour per face from its centroid UV, or a
    neutral grey fallback."""
    vis = mesh.visual
    n = len(faces)
    fallback = np.full((n, 3), 180, np.uint8)
    try:
        if isinstance(vis, trimesh.visual.TextureVisuals) and vis.uv is not None:
            tex = vis.material.baseColorTexture
            if tex is None:
                base = vis.material.baseColorFactor
                if base is not None:
                    return np.tile((np.array(base[:3]) * 255).astype(np.uint8), (n, 1))
                return fallback
            tex = tex.convert("RGB")
            tw, th = tex.size
            timg = np.asarray(tex)
            uv = np.asarray(vis.uv)
            cuv = uv[faces].mean(axis=1)  # centroid uv per face
            px = np.clip((cuv[:, 0] % 1.0) * (tw - 1), 0, tw - 1).astype(int)
            py = np.clip((1.0 - (cuv[:, 1] % 1.0)) * (th - 1), 0, th - 1).astype(int)
            return timg[py, px]
        if hasattr(vis, "face_colors") and vis.face_colors is not None:
            return np.asarray(vis.face_colors)[:, :3]
    except Exception as e:
        print("  texture sample failed:", repr(e))
    return fallback


def render(path):
    scene = trimesh.load(path, process=False)
    mesh = scene.to_geometry() if isinstance(scene, trimesh.Scene) else scene
    V = np.asarray(mesh.vertices, float)
    F = np.asarray(mesh.faces)
    fcol = sample_texture(mesh, F).astype(float)
    fn = np.asarray(mesh.face_normals, float)

    bmin, bmax = V.min(0), V.max(0)
    cx = (bmin[0] + bmax[0]) / 2
    height = bmax[1] - bmin[1]
    cy = bmax[1] - FACE_DROP * height
    half = FACE_ZOOM * height

    S = OUTSIZE * SS
    img = np.zeros((S, S, 4), np.float32)  # RGBA, premultiplied-ish (alpha last)
    zbuf = np.full((S, S), -1e9, np.float32)

    # Orthographic front camera at +Z looking toward -Z: screen x = world x, up = world y.
    # u,v in [-1,1] around (cx,cy); depth = world z (nearer to +Z camera = larger).
    def to_screen(p):
        u = (p[:, 0] - cx) / half
        v = (p[:, 1] - cy) / half
        sx = (u * 0.5 + 0.5) * (S - 1)
        sy = (0.5 - v * 0.5) * (S - 1)  # flip y for image space
        return sx, sy

    sx, sy = to_screen(V)
    depth = V[:, 2]

    # Lambert per face (double-sided: abs so back faces still lit).
    lam = np.abs(fn @ LIGHT)
    shade = AMBIENT + (1 - AMBIENT) * lam

    for i, (a, b, c) in enumerate(F):
        x = np.array([sx[a], sx[b], sx[c]])
        y = np.array([sy[a], sy[b], sy[c]])
        z = np.array([depth[a], depth[b], depth[c]])
        col = np.clip(fcol[i] * shade[i], 0, 255)
        x0, x1 = int(np.floor(x.min())), int(np.ceil(x.max()))
        y0, y1 = int(np.floor(y.min())), int(np.ceil(y.max()))
        x0, y0 = max(x0, 0), max(y0, 0)
        x1, y1 = min(x1, S - 1), min(y1, S - 1)
        if x1 < x0 or y1 < y0:
            continue
        xs, ys = np.meshgrid(np.arange(x0, x1 + 1), np.arange(y0, y1 + 1))
        # Barycentric
        d = (y[1] - y[2]) * (x[0] - x[2]) + (x[2] - x[1]) * (y[0] - y[2])
        if abs(d) < 1e-9:
            continue
        w0 = ((y[1] - y[2]) * (xs - x[2]) + (x[2] - x[1]) * (ys - y[2])) / d
        w1 = ((y[2] - y[0]) * (xs - x[2]) + (x[0] - x[2]) * (ys - y[2])) / d
        w2 = 1 - w0 - w1
        inside = (w0 >= 0) & (w1 >= 0) & (w2 >= 0)
        if not inside.any():
            continue
        pz = w0 * z[0] + w1 * z[1] + w2 * z[2]
        yy, xx = ys[inside], xs[inside]
        pzz = pz[inside]
        cur = zbuf[yy, xx]
        win = pzz > cur
        yy, xx, pzz = yy[win], xx[win], pzz[win]
        zbuf[yy, xx] = pzz
        img[yy, xx, :3] = col
        img[yy, xx, 3] = 255

    out = Image.fromarray(img.astype(np.uint8), "RGBA")
    out = out.resize((OUTSIZE, OUTSIZE), Image.LANCZOS)
    return out


def main():
    import os
    os.makedirs(OUT, exist_ok=True)
    for name in MONSTERS:
        img = render(f"{SRC}/{name}")
        stem = name[:-4].lower()
        img.save(f"{OUT}/{stem}.png")
        print("rendered", stem, img.size)


if __name__ == "__main__":
    main()
